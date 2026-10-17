//! [`OpenH264Encoder`]: Cisco's OpenH264 binary as a software `VideoEncoder` (ADR-0061).
//!
//! OpenH264 encodes synchronously: each frame given to `encode` comes back as one access
//! unit (or none, for a skipped frame) before the call returns, already Annex B with start
//! codes. It never reorders, so a packet's `dts` is its `pts`. Input is CPU frames in
//! I420, passed to the library as they are, or NV12, whose chroma is split into two
//! planes first (the library takes only I420).

use std::ffi::{c_int, c_void};
use std::ptr::NonNull;
use std::sync::Arc;

use jstream_platform::media::{Codec, MediaTime, Packet, PixelFormat, Rounding, Timebase};
use jstream_platform::{
    CodecInfo, CodecRole, PlatformError, RateControl, VideoEncoder, VideoEncoderConfig,
    VideoSurface,
};

use super::ffi::{
    Api, CAMERA_VIDEO_REAL_TIME, EncParamBase, Encoder, FRAME_TYPE_IDR, FRAME_TYPE_INVALID,
    FRAME_TYPE_SKIP, FrameBsInfo, OPTION_IDR_INTERVAL, OPTION_MAX_BITRATE, OPTION_RC_FRAME_SKIP,
    OPTION_TRACE_LEVEL, RC_BITRATE_MODE, SourcePicture, VIDEO_FORMAT_I420, WELS_LOG_QUIET,
};

/// The name the encoder lists under. It names Cisco, as its licence asks.
pub const OPENH264_ENCODER_NAME: &str = "OpenH264 (Cisco Systems, software)";

/// The codec list entry for the encoder.
pub fn openh264_encoder_info() -> CodecInfo {
    CodecInfo {
        name: OPENH264_ENCODER_NAME.into(),
        codec: Codec::H264,
        role: CodecRole::VideoEncoder,
        hardware: false,
    }
}

/// One OpenH264 encoder session.
pub struct OpenH264Encoder {
    /// The library's entry points, keeping it loaded while `encoder` lives.
    api: Arc<Api>,
    encoder: NonNull<Encoder>,
    info: CodecInfo,
    config: VideoEncoderConfig,
    /// Scratch U and V planes for NV12 input, allocated once.
    chroma: [Vec<u8>; 2],
    /// Where the library describes each frame's output.
    out: Box<FrameBsInfo>,
    packets: Vec<Packet>,
    keyframe_requested: bool,
    finished: bool,
}

impl std::fmt::Debug for OpenH264Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264Encoder")
            .field("info", &self.info)
            .field("config", &self.config)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

// SAFETY: an OpenH264 encoder object has no thread affinity; it is only ever used through
// `&mut self`, so by one thread at a time, and the raw pointers in `out` point into the
// encoder's own buffers, which move with it.
unsafe impl Send for OpenH264Encoder {}

/// `config`'s frame rate, in frames per second.
fn frame_rate(period: Timebase) -> f32 {
    period.den() as f32 / period.num() as f32
}

fn os(call: &'static str, rc: c_int) -> PlatformError {
    PlatformError::Os {
        call,
        code: u32::try_from(rc).ok(),
        message: format!("OpenH264 returned {rc}"),
    }
}

/// Bits per second from kilobits, within `int`.
fn bps(kbps: u32) -> Result<c_int, PlatformError> {
    kbps.checked_mul(1000)
        .and_then(|b| c_int::try_from(b).ok())
        .filter(|&b| b > 0)
        .ok_or_else(|| PlatformError::Unsupported {
            what: format!("a bit rate of {kbps} kbps for OpenH264"),
        })
}

/// Checks `config` and builds the library's parameters for it.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec other than H.264, an input format other
/// than NV12 or I420, an odd frame size, or a bit rate that doesn't fit.
pub(super) fn params(config: &VideoEncoderConfig) -> Result<EncParamBase, PlatformError> {
    if config.codec != Codec::H264 {
        return Err(PlatformError::Unsupported {
            what: format!("{:?} encoding with OpenH264 (H.264 only)", config.codec),
        });
    }
    if !matches!(config.input_format, PixelFormat::Nv12 | PixelFormat::I420) {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{:?} input to OpenH264 (NV12 or I420 only)",
                config.input_format
            ),
        });
    }
    let (w, h) = (config.size.width(), config.size.height());
    if w % 2 != 0 || h % 2 != 0 {
        return Err(PlatformError::Unsupported {
            what: format!("an odd frame size ({w}x{h}) for OpenH264's 4:2:0 input"),
        });
    }
    let target = match config.rate_control {
        RateControl::Cbr { kbps } | RateControl::Vbr { kbps, .. } => bps(kbps)?,
        _ => {
            return Err(PlatformError::Unsupported {
                what: "this rate control with OpenH264".into(),
            });
        }
    };
    let dim = |v: u32| {
        c_int::try_from(v).map_err(|_| PlatformError::Unsupported {
            what: format!("a frame dimension of {v} for OpenH264"),
        })
    };
    Ok(EncParamBase {
        usage_type: CAMERA_VIDEO_REAL_TIME,
        pic_width: dim(w)?,
        pic_height: dim(h)?,
        target_bitrate: target,
        rc_mode: RC_BITRATE_MODE,
        max_frame_rate: frame_rate(config.frame_period),
    })
}

impl OpenH264Encoder {
    /// Opens an encoder on `api` for `config`.
    ///
    /// # Errors
    /// As for [`params`], and [`PlatformError::Os`] when the library refuses to create or
    /// configure the encoder.
    pub(super) fn open(api: Arc<Api>, config: &VideoEncoderConfig) -> Result<Self, PlatformError> {
        let base = params(config)?;
        let mut raw: *mut Encoder = std::ptr::null_mut();
        // SAFETY: `WelsCreateSVCEncoder` writes an encoder pointer through its argument,
        // which points at a live local.
        let rc = unsafe { (api.create)(&mut raw) };
        let encoder = NonNull::new(raw)
            .filter(|_| rc == 0)
            .ok_or_else(|| os("WelsCreateSVCEncoder", rc))?;
        let chroma_len = (config.size.width() / 2) as usize * (config.size.height() / 2) as usize;
        let chroma = if config.input_format == PixelFormat::Nv12 {
            [vec![0; chroma_len], vec![0; chroma_len]]
        } else {
            [Vec::new(), Vec::new()]
        };
        // From here on, `Drop` uninitialises and destroys the encoder.
        let mut this = Self {
            api,
            encoder,
            info: openh264_encoder_info(),
            config: *config,
            chroma,
            out: FrameBsInfo::zeroed(),
            packets: Vec::new(),
            keyframe_requested: false,
            finished: false,
        };
        let mut quiet = WELS_LOG_QUIET;
        this.set_option(OPTION_TRACE_LEVEL, (&raw mut quiet).cast())?;
        // SAFETY: `encoder` is the live encoder `WelsCreateSVCEncoder` returned, and
        // `base` is a valid `SEncParamBase` the call only reads.
        let rc = unsafe { (this.vtbl().initialize)(this.encoder.as_ptr(), &base) };
        if rc != 0 {
            return Err(os("ISVCEncoder::Initialize", rc));
        }
        let mut idr = c_int::try_from(config.keyframe_interval).unwrap_or(c_int::MAX);
        this.set_option(OPTION_IDR_INTERVAL, (&raw mut idr).cast())?;
        // A live stream keeps every frame: rate control spends fewer bits rather than
        // dropping frames.
        let mut skip = false;
        this.set_option(OPTION_RC_FRAME_SKIP, (&raw mut skip).cast())?;
        if let RateControl::Vbr { max_kbps, .. } = config.rate_control {
            let mut max = bps(max_kbps)?;
            this.set_option(OPTION_MAX_BITRATE, (&raw mut max).cast())?;
        }
        Ok(this)
    }

    fn vtbl(&self) -> &super::ffi::EncoderVtbl {
        // SAFETY: an `ISVCEncoder*` points at the vtable pointer, which the library set
        // to its static vtable when it created the encoder; both outlive `self`.
        unsafe { &**self.encoder.as_ptr() }
    }

    fn set_option(&mut self, option: c_int, value: *mut c_void) -> Result<(), PlatformError> {
        // SAFETY: the encoder is live, and `value` points at the type `option` takes
        // (each caller passes the one its constant's comment names).
        let rc = unsafe { (self.vtbl().set_option)(self.encoder.as_ptr(), option, value) };
        if rc == 0 {
            Ok(())
        } else {
            Err(os("ISVCEncoder::SetOption", rc))
        }
    }

    /// Encodes `frame` (already checked) and queues its packet.
    fn encode_cpu(
        &mut self,
        frame: &jstream_platform::media::VideoFrame,
    ) -> Result<(), PlatformError> {
        let info = *frame.info();
        let (w, h) = (info.size.width() as usize, info.size.height() as usize);
        let plane = |i: usize| {
            frame.plane(i).ok_or(PlatformError::Unsupported {
                what: "a frame missing a plane".into(),
            })
        };
        let stride = |s: usize| {
            c_int::try_from(s).map_err(|_| PlatformError::Unsupported {
                what: format!("a row stride of {s} bytes for OpenH264"),
            })
        };
        let y = plane(0)?;
        let mut pic = SourcePicture {
            color_format: VIDEO_FORMAT_I420,
            stride: [stride(y.stride())?, 0, 0, 0],
            data: [
                y.data().as_ptr().cast_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ],
            pic_width: c_int::try_from(w).unwrap_or(c_int::MAX),
            pic_height: c_int::try_from(h).unwrap_or(c_int::MAX),
            time_stamp: info
                .pts
                .rescale(Timebase::MILLIS, Rounding::Nearest)
                .map(MediaTime::ticks)
                .unwrap_or(0),
            psnr_y: false,
            psnr_u: false,
            psnr_v: false,
        };
        if info.format == PixelFormat::Nv12 {
            let uv = plane(1)?;
            let (cw, ch) = (w / 2, h / 2);
            let [u, v] = &mut self.chroma;
            for row in 0..ch {
                let src = uv
                    .data()
                    .get(row * uv.stride()..row * uv.stride() + cw * 2)
                    .ok_or(PlatformError::Unsupported {
                        what: "an NV12 chroma plane shorter than its frame".into(),
                    })?;
                let (u_row, v_row) = (&mut u[row * cw..][..cw], &mut v[row * cw..][..cw]);
                for ((pair, u), v) in src.chunks_exact(2).zip(u_row).zip(v_row) {
                    *u = pair[0];
                    *v = pair[1];
                }
            }
            let cs = stride(cw)?;
            pic.stride[1] = cs;
            pic.stride[2] = cs;
            pic.data[1] = u.as_mut_ptr();
            pic.data[2] = v.as_mut_ptr();
        } else {
            let (u, v) = (plane(1)?, plane(2)?);
            pic.stride[1] = stride(u.stride())?;
            pic.stride[2] = stride(v.stride())?;
            pic.data[1] = u.data().as_ptr().cast_mut();
            pic.data[2] = v.data().as_ptr().cast_mut();
        }

        if self.keyframe_requested {
            // SAFETY: the encoder is live; the call takes a plain `bool`.
            let rc = unsafe { (self.vtbl().force_intra_frame)(self.encoder.as_ptr(), true) };
            if rc != 0 {
                return Err(os("ISVCEncoder::ForceIntraFrame", rc));
            }
            self.keyframe_requested = false;
        }
        let encoder = self.encoder.as_ptr();
        let encode_frame = self.vtbl().encode_frame;
        // SAFETY: the encoder is live. `pic`'s planes are `frame`'s (or the chroma
        // scratch), which outlive the call, each holding `pic_height` (or half as many)
        // rows of its stride, as `VideoFrame` guarantees and the copy above made; the
        // library only reads them. It fills `out`, whose layer pointers then point into
        // its own buffers until the next call on this encoder.
        let rc = unsafe { encode_frame(encoder, &pic, &mut *self.out) };
        if rc != 0 {
            return Err(os("ISVCEncoder::EncodeFrame", rc));
        }
        let frame_type = self.out.frame_type;
        if frame_type == FRAME_TYPE_SKIP || frame_type == FRAME_TYPE_INVALID {
            return Ok(());
        }
        let mut data =
            Vec::with_capacity(usize::try_from(self.out.frame_size_in_bytes).unwrap_or(0));
        let layers = usize::try_from(self.out.layer_num)
            .unwrap_or(0)
            .min(self.out.layer_info.len());
        for layer in &self.out.layer_info[..layers] {
            let nals = usize::try_from(layer.nal_count).unwrap_or(0);
            if nals == 0 || layer.nal_length_in_byte.is_null() || layer.bs_buf.is_null() {
                continue;
            }
            // SAFETY: the library wrote `nal_count` lengths at `pNalLengthInByte`, and
            // `pBsBuf` holds those NAL units back to back; both stay valid until the
            // next call on this encoder, and are only read here.
            let lengths = unsafe { std::slice::from_raw_parts(layer.nal_length_in_byte, nals) };
            let len: usize = lengths
                .iter()
                .map(|&l| usize::try_from(l).unwrap_or(0))
                .sum();
            // SAFETY: as above: `len` bytes at `pBsBuf` are the layer's bitstream.
            data.extend_from_slice(unsafe { std::slice::from_raw_parts(layer.bs_buf, len) });
        }
        if data.is_empty() {
            return Ok(());
        }
        let packet = Packet::new(
            Codec::H264,
            frame_type == FRAME_TYPE_IDR,
            info.pts,
            info.pts,
            data,
        )?;
        self.packets.push(packet);
        Ok(())
    }
}

impl VideoEncoder for OpenH264Encoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn can_accept(&self) -> bool {
        !self.finished
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "OpenH264Encoder::encode",
                state: "finished",
            });
        }
        let VideoSurface::Cpu(frame) = frame else {
            return Err(PlatformError::Unsupported {
                what: "GPU frames for OpenH264 (CPU frames only)".into(),
            });
        };
        let info = frame.info();
        if info.size != self.config.size || info.format != self.config.input_format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {}x{} {:?} frame for an OpenH264 session opened for {}x{} {:?}",
                    info.size.width(),
                    info.size.height(),
                    info.format,
                    self.config.size.width(),
                    self.config.size.height(),
                    self.config.input_format
                ),
            });
        }
        self.encode_cpu(&frame)
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.append(&mut self.packets);
        Ok(())
    }

    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "OpenH264Encoder::request_keyframe",
                state: "finished",
            });
        }
        self.keyframe_requested = true;
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        // Nothing is held back: every frame's packet is out when `encode` returns.
        self.finished = true;
        out.append(&mut self.packets);
        Ok(())
    }
}

impl Drop for OpenH264Encoder {
    fn drop(&mut self) {
        let encoder = self.encoder.as_ptr();
        // SAFETY: the encoder is live and not used again: `Uninitialize` is harmless on
        // an encoder `Initialize` refused, and `WelsDestroySVCEncoder` frees it.
        unsafe {
            (self.vtbl().uninitialize)(encoder);
            (self.api.destroy)(encoder);
        }
    }
}

#[cfg(test)]
mod tests;
