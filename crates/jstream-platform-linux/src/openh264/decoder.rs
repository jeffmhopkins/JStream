//! [`OpenH264Decoder`]: Cisco's OpenH264 binary as a software `VideoDecoder` (ADR-0140).
//!
//! It decodes the H.264 streams the engine encodes itself, such as the quality probe's
//! packets, never a media file (ADR-0072 decides those). OpenH264 decodes synchronously:
//! each packet given to `decode` puts out at most one picture before the call returns,
//! already in presentation order, and `finish` flushes the pictures a stream with
//! reordering still holds. Pictures come out as CPU frames in I420, as the library
//! decodes them, or NV12, with the chroma interleaved. Each picture keeps its packet's
//! `pts`: the library carries a time stamp from the packet to the picture, and the
//! decoder passes the `pts` through it.

use std::ffi::{c_int, c_long, c_void};
use std::ptr::NonNull;
use std::sync::Arc;

use jstream_platform::media::{
    Codec, ColorInfo, FrameSize, MediaTime, Packet, PixelFormat, Plane, Timebase, VideoFrame,
    VideoFrameInfo,
};
use jstream_platform::{
    CodecInfo, CodecRole, HardwarePreference, PlatformError, VideoDecoder, VideoDecoderConfig,
    VideoSurface,
};

use super::ffi::{
    Api, BufferInfo, DECODER_OPTION_TRACE_LEVEL, DS_ERROR_FREE, Decoder, DecoderVtbl,
    DecodingParam, ERROR_CON_DISABLE, VIDEO_BITSTREAM_AVC, VIDEO_FORMAT_I420, VideoProperty,
    WELS_LOG_QUIET,
};

/// The name the decoder lists under. It names Cisco, as its licence asks.
pub const OPENH264_DECODER_NAME: &str = "OpenH264 decoder (Cisco Systems, software)";

/// The codec list entry for the decoder.
pub fn openh264_decoder_info() -> CodecInfo {
    CodecInfo {
        name: OPENH264_DECODER_NAME.into(),
        codec: Codec::H264,
        role: CodecRole::VideoDecoder,
        hardware: false,
    }
}

/// The most pictures `finish` flushes: H.264's decoded picture buffer holds at most 16.
const MAX_FLUSH: usize = 16;

/// One OpenH264 decoder session.
pub struct OpenH264Decoder {
    /// The library's entry points, keeping it loaded while `decoder` lives.
    api: Arc<Api>,
    decoder: NonNull<Decoder>,
    info: CodecInfo,
    format: PixelFormat,
    /// The time base of the packets' `pts`, from the first packet: the library carries
    /// only the ticks.
    timebase: Option<Timebase>,
    frames: Vec<VideoSurface>,
    finished: bool,
}

impl std::fmt::Debug for OpenH264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264Decoder")
            .field("info", &self.info)
            .field("format", &self.format)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

// SAFETY: an OpenH264 decoder object has no thread affinity; it is only ever used through
// `&mut self`, so by one thread at a time.
unsafe impl Send for OpenH264Decoder {}

fn os(call: &'static str, rc: impl Into<i64>) -> PlatformError {
    let rc = rc.into();
    PlatformError::Os {
        call,
        code: u32::try_from(rc).ok(),
        message: format!("OpenH264 returned {rc}"),
    }
}

/// Checks `config` for the decoder.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec other than H.264, an output format other
/// than I420 or NV12, or [`HardwarePreference::Require`]: OpenH264 is software only.
pub(super) fn check(config: &VideoDecoderConfig) -> Result<(), PlatformError> {
    if config.codec != Codec::H264 {
        return Err(PlatformError::Unsupported {
            what: format!("{:?} decoding with OpenH264 (H.264 only)", config.codec),
        });
    }
    if !matches!(config.output_format, PixelFormat::I420 | PixelFormat::Nv12) {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{:?} output from OpenH264's decoder (I420 or NV12 only)",
                config.output_format
            ),
        });
    }
    if config.hardware == HardwarePreference::Require {
        return Err(PlatformError::Unsupported {
            what: "a hardware H.264 decoder (OpenH264 is software only)".into(),
        });
    }
    Ok(())
}

impl OpenH264Decoder {
    /// Opens a decoder on `api` for `config`.
    ///
    /// # Errors
    /// As for [`check`], [`PlatformError::MissingComponent`] for a library without the
    /// decoder's entry points, and [`PlatformError::Os`] when the library refuses to
    /// create or configure the decoder.
    pub(super) fn open(api: Arc<Api>, config: &VideoDecoderConfig) -> Result<Self, PlatformError> {
        check(config)?;
        let (create, _) = api.decoder.ok_or(PlatformError::MissingComponent {
            component: super::OPENH264_COMPONENT,
            remedy: "this OpenH264 library has no decoder".into(),
        })?;
        let mut raw: *mut Decoder = std::ptr::null_mut();
        // SAFETY: `WelsCreateDecoder` writes a decoder pointer through its argument,
        // which points at a live local.
        let rc = unsafe { create(&mut raw) };
        let decoder = NonNull::new(raw)
            .filter(|_| rc == 0)
            .ok_or_else(|| os("WelsCreateDecoder", rc))?;
        // From here on, `Drop` uninitialises and destroys the decoder.
        let mut this = Self {
            api,
            decoder,
            info: openh264_decoder_info(),
            format: config.output_format,
            timebase: None,
            frames: Vec::new(),
            finished: false,
        };
        let mut quiet: c_int = WELS_LOG_QUIET;
        this.set_option(DECODER_OPTION_TRACE_LEVEL, (&raw mut quiet).cast())?;
        let param = DecodingParam {
            file_name_restructed: std::ptr::null_mut(),
            cpu_load: 0,
            target_dq_layer: u8::MAX,
            ec_active_idc: ERROR_CON_DISABLE,
            parse_only: false,
            video_property: VideoProperty {
                size: std::mem::size_of::<VideoProperty>() as u32,
                bs_type: VIDEO_BITSTREAM_AVC,
            },
        };
        // SAFETY: the decoder is live, and `param` is a valid `SDecodingParam` the call
        // only reads.
        let rc = unsafe { (this.vtbl().initialize)(this.decoder.as_ptr(), &param) };
        if rc != 0 {
            return Err(os("ISVCDecoder::Initialize", rc));
        }
        Ok(this)
    }

    fn vtbl(&self) -> &DecoderVtbl {
        // SAFETY: an `ISVCDecoder*` points at the vtable pointer, which the library set
        // to its static vtable when it created the decoder; both outlive `self`.
        unsafe { &**self.decoder.as_ptr() }
    }

    fn set_option(&mut self, option: c_int, value: *mut c_void) -> Result<(), PlatformError> {
        // SAFETY: the decoder is live, and `value` points at the type `option` takes
        // (each caller passes the one its constant's comment names).
        let rc: c_long = unsafe { (self.vtbl().set_option)(self.decoder.as_ptr(), option, value) };
        if rc == 0 {
            Ok(())
        } else {
            Err(os("ISVCDecoder::SetOption", rc))
        }
    }

    /// Copies the picture `info` and `planes` describe, if the call put one out, into a
    /// frame of the configured format.
    fn take_picture(
        &mut self,
        info: &BufferInfo,
        planes: [*mut u8; 3],
    ) -> Result<(), PlatformError> {
        if info.buffer_status != 1 {
            return Ok(());
        }
        let buf = info.system_buffer;
        if buf.format != VIDEO_FORMAT_I420 || planes.iter().any(|p| p.is_null()) {
            return Err(PlatformError::Os {
                call: "ISVCDecoder::DecodeFrameNoDelay",
                code: None,
                message: format!("OpenH264 put out a picture in format {}", buf.format),
            });
        }
        let dim = |v: c_int| u32::try_from(v).ok().filter(|&v| v > 0);
        let size = dim(buf.width)
            .zip(dim(buf.height))
            .and_then(|(w, h)| FrameSize::new(w, h).ok())
            .ok_or_else(|| PlatformError::Os {
                call: "ISVCDecoder::DecodeFrameNoDelay",
                code: None,
                message: format!("OpenH264 put out a {}x{} picture", buf.width, buf.height),
            })?;
        let strides =
            [buf.stride[0], buf.stride[1], buf.stride[1]].map(|s| usize::try_from(s).unwrap_or(0));
        // The time stamp went in as the packet's `pts` ticks, bit for bit.
        let ticks = info.out_yuv_time_stamp as i64;
        let pts = MediaTime::new(ticks, self.timebase.unwrap_or(Timebase::MILLIS));
        let frame_info = VideoFrameInfo {
            size,
            format: self.format,
            color: ColorInfo::default(),
            pts,
        };
        let (w, h) = (size.width() as usize, size.height() as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let read = |plane: usize, rows: usize, row_bytes: usize| -> Result<&[u8], PlatformError> {
            let stride = strides[plane];
            if stride < row_bytes {
                return Err(PlatformError::Os {
                    call: "ISVCDecoder::DecodeFrameNoDelay",
                    code: None,
                    message: format!("OpenH264 put out a plane {plane} stride of {stride}"),
                });
            }
            let len = stride * (rows - 1) + row_bytes;
            // SAFETY: the library put out a picture (`iBufferStatus` is 1), whose plane
            // `plane` holds `rows` rows of `stride` bytes at `pDst[plane]`, each with at
            // least `row_bytes` of picture (checked above); it stays valid until the next
            // call on this decoder, and is only read here.
            Ok(unsafe { std::slice::from_raw_parts(planes[plane], len) })
        };
        let (y, u, v) = (read(0, h, w)?, read(1, ch, cw)?, read(2, ch, cw)?);
        let copy_rows = |src: &[u8], stride: usize, rows: usize, row_bytes: usize| {
            let mut out = Vec::with_capacity(rows * row_bytes);
            for row in 0..rows {
                out.extend_from_slice(&src[row * stride..][..row_bytes]);
            }
            out
        };
        let luma = Plane::new(copy_rows(y, strides[0], h, w), w);
        let planes = if self.format == PixelFormat::Nv12 {
            let mut uv = Vec::with_capacity(ch * cw * 2);
            for row in 0..ch {
                let (u_row, v_row) = (&u[row * strides[1]..][..cw], &v[row * strides[2]..][..cw]);
                for (&u, &v) in u_row.iter().zip(v_row) {
                    uv.extend([u, v]);
                }
            }
            vec![luma, Plane::new(uv, cw * 2)]
        } else {
            vec![
                luma,
                Plane::new(copy_rows(u, strides[1], ch, cw), cw),
                Plane::new(copy_rows(v, strides[2], ch, cw), cw),
            ]
        };
        let frame = VideoFrame::new(frame_info, planes)?;
        self.frames.push(VideoSurface::Cpu(frame));
        Ok(())
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "OpenH264Decoder::decode",
                state: "finished",
            });
        }
        if packet.codec() != Codec::H264 {
            return Err(PlatformError::Unsupported {
                what: format!("a {:?} packet for OpenH264's H.264 decoder", packet.codec()),
            });
        }
        let pts = packet.pts();
        let timebase = *self.timebase.get_or_insert(pts.timebase());
        let pts = if pts.timebase() == timebase {
            pts
        } else {
            pts.rescale(timebase, jstream_platform::media::Rounding::Nearest)
                .map_err(PlatformError::Media)?
        };
        // Never empty (`Packet::new` refuses that), so the library never takes it for
        // the end of the stream.
        let data = packet.data();
        let len = c_int::try_from(data.len()).map_err(|_| PlatformError::Unsupported {
            what: format!("a {}-byte packet for OpenH264", data.len()),
        })?;
        let mut planes = [std::ptr::null_mut(); 3];
        let mut info = BufferInfo::with_time_stamp(pts.ticks() as u64);
        let decode = self.vtbl().decode_frame_no_delay;
        // SAFETY: the decoder is live; `data` is `len` bytes of Annex B the call only
        // reads; it writes the picture's plane pointers into `planes` and describes the
        // picture in `info`, both live locals.
        let rc = unsafe {
            decode(
                self.decoder.as_ptr(),
                data.as_ptr(),
                len,
                planes.as_mut_ptr(),
                &mut info,
            )
        };
        if rc != DS_ERROR_FREE {
            return Err(os("ISVCDecoder::DecodeFrameNoDelay", rc));
        }
        self.take_picture(&info, planes)
    }

    fn poll(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        out.append(&mut self.frames);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
        if !self.finished {
            self.finished = true;
            // Flush until the library has no picture left, rather than trusting its
            // count of them, and never more than a picture buffer holds.
            for _ in 0..MAX_FLUSH {
                let mut planes = [std::ptr::null_mut(); 3];
                let mut info = BufferInfo::with_time_stamp(0);
                let flush = self.vtbl().flush_frame;
                // SAFETY: as for `DecodeFrameNoDelay` in `decode`, with no input.
                let rc = unsafe { flush(self.decoder.as_ptr(), planes.as_mut_ptr(), &mut info) };
                if rc != DS_ERROR_FREE {
                    return Err(os("ISVCDecoder::FlushFrame", rc));
                }
                if info.buffer_status != 1 {
                    break;
                }
                self.take_picture(&info, planes)?;
            }
        }
        out.append(&mut self.frames);
        Ok(())
    }
}

impl Drop for OpenH264Decoder {
    fn drop(&mut self) {
        let decoder = self.decoder.as_ptr();
        if let Some((_, destroy)) = self.api.decoder {
            // SAFETY: the decoder is live and not used again: `Uninitialize` is harmless
            // on a decoder `Initialize` refused, and `WelsDestroyDecoder` frees it.
            unsafe {
                (self.vtbl().uninitialize)(decoder);
                destroy(decoder);
            }
        }
    }
}
