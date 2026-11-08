//! The H.264 decoder: Media Foundation's H.264 decoder MFT as a `VideoDecoder`, for the
//! streams the engine encodes itself, such as the quality probe's packets (ADR-0140).
//!
//! It opens the first synchronous, in-process H.264 decoder MFT Media Foundation ranks
//! (Microsoft's in-box software decoder), with NV12 out, in system memory. Packets go in
//! as Annex B, as `jstream-media` frames them, with their `pts` as the sample time in
//! 100 ns; the MFT reorders into presentation order and carries each time to its
//! picture, which gets its packet's exact `pts` back. The decoder's pictures are padded
//! to whole macroblocks (1080 rows decode as 1088), so each is cropped to the stream's
//! display aperture (`MF_MT_MINIMUM_DISPLAY_APERTURE`) and comes out as I420 or NV12, as
//! configured. `finish` drains the MFT (`MFT_MESSAGE_COMMAND_DRAIN`).
//!
//! The platform-independent parts (the configuration check, the aperture and the
//! picture copy) are tested on every platform.

use jstream_platform::media::{
    ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, VideoFrame, VideoFrameInfo,
};
use jstream_platform::{HardwarePreference, PlatformError, VideoDecoderConfig};

/// The name the decoder lists under when the MFT gives none.
#[cfg(windows)]
const FALLBACK_NAME: &str = "Media Foundation H.264 decoder";

/// Checks `config` for the decoder.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec other than H.264, an output format other
/// than I420 or NV12, or [`HardwarePreference::Require`]: the decoder runs in software,
/// into system memory.
pub(crate) fn check(config: &VideoDecoderConfig) -> Result<(), PlatformError> {
    if config.codec != jstream_platform::media::Codec::H264 {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{:?} decoding on Windows (H.264 only, ADR-0140)",
                config.codec
            ),
        });
    }
    if !matches!(config.output_format, PixelFormat::I420 | PixelFormat::Nv12) {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{:?} output from the H.264 decoder (I420 or NV12 only)",
                config.output_format
            ),
        });
    }
    if config.hardware == HardwarePreference::Require {
        return Err(PlatformError::Unsupported {
            what: "a hardware H.264 decoder (Windows decodes the engine's own streams in \
                   software)"
                .into(),
        });
    }
    Ok(())
}

/// The picture inside a decoded buffer: its offset and size, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Aperture {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) size: FrameSize,
}

impl Aperture {
    /// The aperture an `MFVideoArea` blob describes (`OffsetX` and `OffsetY` as an
    /// `MFOffset` each, a 16-bit fraction then a 16-bit whole value, then `Area` as two
    /// 32-bit values), when it lies inside a `coded` buffer; otherwise the whole buffer.
    pub(crate) fn from_blob(blob: Option<&[u8]>, coded: FrameSize) -> Self {
        let whole = Self {
            x: 0,
            y: 0,
            size: coded,
        };
        let Some(b) = blob.filter(|b| b.len() >= 16) else {
            return whole;
        };
        let i16_at = |at: usize| i16::from_le_bytes([b[at], b[at + 1]]);
        let i32_at = |at: usize| i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let (x, y, w, h) = (i16_at(2), i16_at(6), i32_at(8), i32_at(12));
        let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
            u32::try_from(x),
            u32::try_from(y),
            u32::try_from(w),
            u32::try_from(h),
        ) else {
            return whole;
        };
        match FrameSize::new(w, h) {
            Ok(size)
                if x.checked_add(w).is_some_and(|r| r <= coded.width())
                    && y.checked_add(h).is_some_and(|b| b <= coded.height()) =>
            {
                Self { x, y, size }
            }
            _ => whole,
        }
    }
}

/// Copies the picture `aperture` names out of an NV12 buffer of `coded` rows of
/// `stride` bytes (luma, then interleaved chroma), into a tightly packed frame of
/// `format` (I420 or NV12).
///
/// # Errors
/// [`PlatformError::Os`] when the buffer is too small for its size and stride, or the
/// aperture's offset is odd (4:2:0 chroma can't be split there).
pub(crate) fn picture(
    data: &[u8],
    stride: usize,
    coded: FrameSize,
    aperture: Aperture,
    format: PixelFormat,
    color: ColorInfo,
    pts: MediaTime,
) -> Result<VideoFrame, PlatformError> {
    let bad = |message: String| PlatformError::Os {
        call: "IMFTransform::ProcessOutput",
        code: None,
        message,
    };
    let rows = coded.height() as usize;
    let chroma_rows = rows.div_ceil(2);
    let (x, y) = (aperture.x as usize, aperture.y as usize);
    let (w, h) = (
        aperture.size.width() as usize,
        aperture.size.height() as usize,
    );
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if x % 2 != 0 || y % 2 != 0 {
        return Err(bad(format!("an aperture at an odd offset ({x}, {y})")));
    }
    let needed = stride
        .checked_mul(rows + chroma_rows)
        .ok_or_else(|| bad("an output buffer too large to address".into()))?;
    if stride < coded.width() as usize || data.len() < needed {
        return Err(bad(format!(
            "an NV12 output of {} bytes for {}x{} at stride {stride}",
            data.len(),
            coded.width(),
            rows
        )));
    }
    let (luma, chroma) = data.split_at(stride * rows);
    let mut out_y = Vec::with_capacity(w * h);
    for row in y..y + h {
        out_y.extend_from_slice(&luma[row * stride + x..][..w]);
    }
    let uv_rows = (y / 2..y / 2 + ch).map(|row| &chroma[row * stride + x..][..cw * 2]);
    let planes = if format == PixelFormat::Nv12 {
        let mut uv = Vec::with_capacity(cw * ch * 2);
        for row in uv_rows {
            uv.extend_from_slice(row);
        }
        vec![Plane::new(out_y, w), Plane::new(uv, cw * 2)]
    } else {
        let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
        for row in uv_rows {
            for pair in row.chunks_exact(2) {
                u.push(pair[0]);
                v.push(pair[1]);
            }
        }
        vec![Plane::new(out_y, w), Plane::new(u, cw), Plane::new(v, cw)]
    };
    let info = VideoFrameInfo {
        size: aperture.size,
        format,
        color,
        pts,
    };
    Ok(VideoFrame::new(info, planes)?)
}

#[cfg(windows)]
pub(crate) use session::MfH264Decoder;

#[cfg(windows)]
mod session {
    use std::collections::BTreeMap;
    use std::mem::ManuallyDrop;

    use jstream_platform::media::{
        Codec, FrameSize, MediaTime, Packet, PixelFormat, Rounding, VideoFrame,
    };
    use jstream_platform::{
        CodecInfo, CodecRole, PlatformError, VideoDecoder, VideoDecoderConfig, VideoSurface,
    };
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaType, IMFSample, MF_E_NO_MORE_TYPES, MF_E_NOTACCEPTING,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE,
        MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MFCreateMediaType,
        MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFT_CATEGORY_VIDEO_DECODER,
        MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
        MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
    };

    use super::{Aperture, FALLBACK_NAME, check, picture};
    use crate::codec::mf::{self, MfRuntime, Mft, sample_from};
    use crate::codec::video::hns;
    use crate::com::{OsResult, os_error};
    use crate::video_device::format::color;
    use crate::video_device::source::{type_info, with_bytes};

    /// One H.264 decoder session on Media Foundation's decoder MFT.
    pub(crate) struct MfH264Decoder {
        mft: Mft,
        info: CodecInfo,
        format: PixelFormat,
        /// The output type's buffer size, row stride and picture, read at each change.
        coded: FrameSize,
        stride: usize,
        aperture: Aperture,
        color: jstream_platform::media::ColorInfo,
        provides_samples: bool,
        buffer_size: u32,
        reusable: Option<IMFSample>,
        /// Each packet's `pts`, by the sample time it went in with.
        times: BTreeMap<i64, MediaTime>,
        frames: Vec<VideoSurface>,
        finished: bool,
        /// Declared last: Media Foundation outlives the transform.
        _rt: MfRuntime,
    }

    // SAFETY: the MFT is a synchronous, in-process one (`SYNCMFT | LOCALMFT`), and
    // Microsoft's H.264 decoder is free-threaded; the session keeps the process MTA alive
    // (`MfRuntime`), so whichever thread drives it is an MTA member. It is used only
    // through `&mut self`, so by one thread at a time.
    unsafe impl Send for MfH264Decoder {}

    impl std::fmt::Debug for MfH264Decoder {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MfH264Decoder")
                .field("info", &self.info)
                .field("format", &self.format)
                .field("finished", &self.finished)
                .finish_non_exhaustive()
        }
    }

    impl MfH264Decoder {
        /// Opens the decoder for `config`.
        ///
        /// # Errors
        /// As for [`check`]; [`PlatformError::Unsupported`] when no H.264 decoder MFT is
        /// installed (a Windows N edition without the Media Feature Pack), and
        /// [`PlatformError::Os`] when Media Foundation refuses.
        pub(crate) fn open(config: &VideoDecoderConfig) -> Result<Self, PlatformError> {
            check(config)?;
            let rt = MfRuntime::start()?;
            let found = mf::enumerate(
                MFT_CATEGORY_VIDEO_DECODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                (MFMediaType_Video, MFVideoFormat_H264),
                (MFMediaType_Video, MFVideoFormat_NV12),
            )?;
            let found = found
                .into_iter()
                .next()
                .ok_or_else(|| PlatformError::Unsupported {
                    what: "H.264 decoding (no Media Foundation H.264 decoder is installed; \
                           on a Windows N edition, install the Media Feature Pack)"
                        .into(),
                })?;
            let name = if found.name.is_empty() {
                FALLBACK_NAME.to_owned()
            } else {
                found.name.clone()
            };
            let mft = found.activate()?;
            // SAFETY: plain COM calls on live objects.
            unsafe {
                let input = MFCreateMediaType().os("MFCreateMediaType")?;
                input
                    .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                    .os("IMFMediaType::SetGUID")?;
                input
                    .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                    .os("IMFMediaType::SetGUID")?;
                mft.transform
                    .SetInputType(0, &input, 0)
                    .os("IMFTransform::SetInputType")?;
            }
            let mut this = Self {
                mft,
                info: CodecInfo {
                    name,
                    codec: Codec::H264,
                    role: CodecRole::VideoDecoder,
                    hardware: false,
                },
                format: config.output_format,
                coded: FrameSize::new(16, 16)?,
                stride: 16,
                aperture: Aperture {
                    x: 0,
                    y: 0,
                    size: FrameSize::new(16, 16)?,
                },
                color: jstream_platform::media::ColorInfo::default(),
                provides_samples: false,
                buffer_size: 0,
                reusable: None,
                times: BTreeMap::new(),
                frames: Vec::new(),
                finished: false,
                _rt: rt,
            };
            this.set_output()?;
            // SAFETY: plain COM calls on a configured transform.
            unsafe {
                this.mft
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .os("IMFTransform::ProcessMessage")?;
                this.mft
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .os("IMFTransform::ProcessMessage")?;
            }
            Ok(this)
        }

        /// Sets the NV12 output type the decoder offers, then reads its picture and the
        /// output stream's requirements.
        fn set_output(&mut self) -> Result<(), PlatformError> {
            let transform = &self.mft.transform;
            let mut chosen: Option<IMFMediaType> = None;
            for i in 0.. {
                // SAFETY: plain COM call on a transform with its input type set.
                let t = match unsafe { transform.GetOutputAvailableType(0, i) } {
                    Ok(t) => t,
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => break,
                    Err(e) => return Err(os_error("IMFTransform::GetOutputAvailableType", &e)),
                };
                // SAFETY: plain attribute read.
                let subtype = unsafe { t.GetGUID(&MF_MT_SUBTYPE) };
                if subtype.is_ok_and(|g| g == MFVideoFormat_NV12) {
                    chosen = Some(t);
                    break;
                }
            }
            let t = chosen.ok_or_else(|| PlatformError::Unsupported {
                what: "an H.264 decoder without NV12 output".into(),
            })?;
            // SAFETY: plain COM calls on live objects.
            let info = unsafe {
                transform
                    .SetOutputType(0, &t, 0)
                    .os("IMFTransform::SetOutputType")?;
                transform
                    .GetOutputStreamInfo(0)
                    .os("IMFTransform::GetOutputStreamInfo")?
            };
            // SAFETY: plain attribute read.
            let packed = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }.unwrap_or(0);
            let (w, h) = ((packed >> 32) as u32, packed as u32);
            if let Ok(coded) = FrameSize::new(w.max(2), h.max(2)) {
                self.coded = coded;
            }
            let mut blob = [0u8; 16];
            let mut len = 0u32;
            // SAFETY: the blob read writes at most `blob.len()` bytes into it, and its
            // length into `len`.
            let aperture =
                unsafe { t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, Some(&mut len)) }
                    .ok()
                    .map(|()| &blob[..(len as usize).min(blob.len())]);
            self.aperture = Aperture::from_blob(aperture, self.coded);
            let type_info = type_info(&t);
            self.stride = type_info
                .stride
                .and_then(|s| usize::try_from(s).ok())
                .filter(|&s| s >= self.coded.width() as usize)
                .unwrap_or(self.coded.width() as usize);
            self.color = color(
                type_info.matrix,
                type_info.range,
                self.aperture.size.height(),
                false,
            );
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            self.provides_samples = info.dwFlags & provides != 0;
            let rows = self.coded.height() as usize;
            let needed = u32::try_from(self.stride * (rows + rows.div_ceil(2))).unwrap_or(0);
            self.buffer_size = info.cbSize.max(needed);
            self.reusable = None;
            Ok(())
        }

        /// Takes every picture the decoder has ready.
        fn drain_output(&mut self) -> Result<(), PlatformError> {
            const MAX_CHANGES: u32 = 4;
            let mut changes = 0;
            loop {
                let sample = if self.provides_samples {
                    None
                } else {
                    Some(self.output_sample()?)
                };
                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                // SAFETY: `buffers` is a valid one-element array and `status` a valid
                // out-pointer.
                let result = unsafe {
                    self.mft
                        .transform
                        .ProcessOutput(0, &mut buffers, &mut status)
                };
                // SAFETY: each field is taken exactly once and `buffers` is not used
                // again, so the references in it are released exactly once.
                let (sample, _events) = unsafe {
                    (
                        ManuallyDrop::take(&mut buffers[0].pSample),
                        ManuallyDrop::take(&mut buffers[0].pEvents),
                    )
                };
                match result {
                    Ok(()) => {
                        let sample = sample.ok_or(PlatformError::Os {
                            call: "IMFTransform::ProcessOutput",
                            code: None,
                            message: "succeeded without a sample".into(),
                        })?;
                        let frame = self.frame_of(&sample)?;
                        self.frames.push(VideoSurface::Cpu(frame));
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        changes += 1;
                        if changes > MAX_CHANGES {
                            return Err(PlatformError::Os {
                                call: "IMFTransform::ProcessOutput",
                                code: None,
                                message: "the H.264 decoder's output kept changing".into(),
                            });
                        }
                        self.set_output()?;
                    }
                    Err(e) => return Err(os_error("IMFTransform::ProcessOutput", &e)),
                }
            }
        }

        /// The picture in `sample`, cropped and in the configured format, with its
        /// packet's `pts`.
        fn frame_of(&mut self, sample: &IMFSample) -> Result<VideoFrame, PlatformError> {
            // SAFETY: plain attribute read on a live sample.
            let time = unsafe { sample.GetSampleTime() }.ok();
            let pts = match time {
                Some(t) => self
                    .times
                    .remove(&t)
                    .unwrap_or_else(|| MediaTime::new(t, hns())),
                None => self
                    .times
                    .pop_first()
                    .map_or_else(|| MediaTime::zero(hns()), |(_, pts)| pts),
            };
            // Times the decoder skipped past (a broken packet) are forgotten.
            if let Some(t) = time {
                self.times.retain(|&sent, _| sent > t);
            }
            let (stride, coded, aperture, format, color) = (
                self.stride,
                self.coded,
                self.aperture,
                self.format,
                self.color,
            );
            with_bytes(sample, |bytes| {
                picture(bytes, stride, coded, aperture, format, color, pts)
            })?
        }

        /// The reusable output sample, emptied, created on first use.
        fn output_sample(&mut self) -> Result<IMFSample, PlatformError> {
            let sample = match &self.reusable {
                Some(s) => s.clone(),
                None => {
                    // SAFETY: plain constructor and method calls on live objects.
                    let s = unsafe {
                        let s = MFCreateSample().os("MFCreateSample")?;
                        s.AddBuffer(
                            &MFCreateMemoryBuffer(self.buffer_size).os("MFCreateMemoryBuffer")?,
                        )
                        .os("IMFSample::AddBuffer")?;
                        s
                    };
                    self.reusable = Some(s.clone());
                    s
                }
            };
            // SAFETY: plain COM calls on the sample's own buffer; emptying it lets the
            // MFT write from the start.
            unsafe {
                sample
                    .GetBufferByIndex(0)
                    .and_then(|b| b.SetCurrentLength(0))
                    .os("IMFMediaBuffer::SetCurrentLength")?;
            }
            Ok(sample)
        }
    }

    impl VideoDecoder for MfH264Decoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        fn can_accept(&self) -> bool {
            !self.finished
        }

        fn decode(&mut self, packet: &Packet) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "MfH264Decoder::decode",
                    state: "finished",
                });
            }
            if packet.codec() != Codec::H264 {
                return Err(PlatformError::Unsupported {
                    what: format!("a {:?} packet for the H.264 decoder", packet.codec()),
                });
            }
            let time = packet.pts().rescale(hns(), Rounding::Nearest)?.ticks();
            let input = sample_from(packet.data(), time, 0)?;
            // SAFETY: plain COM call on a streaming transform.
            let mut fed = unsafe { self.mft.transform.ProcessInput(0, &input, 0) };
            if fed.as_ref().is_err_and(|e| e.code() == MF_E_NOTACCEPTING) {
                // Pictures from earlier packets are waiting: take them, then feed again.
                self.drain_output()?;
                // SAFETY: as above.
                fed = unsafe { self.mft.transform.ProcessInput(0, &input, 0) };
            }
            fed.os("IMFTransform::ProcessInput")?;
            self.times.insert(time, packet.pts());
            self.drain_output()
        }

        fn poll(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
            out.append(&mut self.frames);
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<VideoSurface>) -> Result<(), PlatformError> {
            if !self.finished {
                self.finished = true;
                // SAFETY: plain COM calls on a streaming transform.
                unsafe {
                    self.mft
                        .transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                        .os("IMFTransform::ProcessMessage")?;
                    self.mft
                        .transform
                        .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                        .os("IMFTransform::ProcessMessage")?;
                }
                self.drain_output()?;
            }
            out.append(&mut self.frames);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, PlatformError, VideoDecoderConfig};

    use super::{Aperture, check, picture};

    fn size(w: u32, h: u32) -> FrameSize {
        FrameSize::new(w, h).expect("valid size")
    }

    #[test]
    fn takes_h264_to_i420_or_nv12_in_software() {
        let config = |codec, output_format, hardware| VideoDecoderConfig {
            codec,
            output_format,
            hardware,
        };
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            for hardware in [HardwarePreference::Prefer, HardwarePreference::Avoid] {
                check(&config(Codec::H264, format, hardware)).expect("supported");
            }
        }
        for refused in [
            config(Codec::Hevc, PixelFormat::Nv12, HardwarePreference::Prefer),
            config(Codec::H264, PixelFormat::Bgra8, HardwarePreference::Prefer),
            config(Codec::H264, PixelFormat::Nv12, HardwarePreference::Require),
        ] {
            assert!(
                matches!(check(&refused), Err(PlatformError::Unsupported { .. })),
                "{refused:?}"
            );
        }
    }

    /// An `MFVideoArea` blob for an aperture at (`x`, `y`) of `w` by `h`.
    fn blob(x: i16, y: i16, w: i32, h: i32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(0u16.to_le_bytes());
        b.extend(x.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b.extend(y.to_le_bytes());
        b.extend(w.to_le_bytes());
        b.extend(h.to_le_bytes());
        b
    }

    #[test]
    fn reads_the_display_aperture_inside_the_buffer() {
        let coded = size(1920, 1088);
        let a = Aperture::from_blob(Some(&blob(0, 0, 1920, 1080)), coded);
        assert_eq!(
            a,
            Aperture {
                x: 0,
                y: 0,
                size: size(1920, 1080)
            }
        );
        let whole = Aperture {
            x: 0,
            y: 0,
            size: coded,
        };
        assert_eq!(Aperture::from_blob(None, coded), whole);
        assert_eq!(Aperture::from_blob(Some(&[0; 8]), coded), whole);
        assert_eq!(
            Aperture::from_blob(Some(&blob(0, 0, 1920, 1100)), coded),
            whole
        );
        assert_eq!(
            Aperture::from_blob(Some(&blob(-2, 0, 64, 64)), coded),
            whole
        );
        assert_eq!(Aperture::from_blob(Some(&blob(0, 0, 0, 64)), coded), whole);
    }

    /// An NV12 buffer of `coded` at `stride` whose samples say where they are: luma
    /// `x + y`, chroma U `x` and V `y` (in chroma samples), all wrapping.
    fn buffer(coded: FrameSize, stride: usize) -> Vec<u8> {
        let rows = coded.height() as usize;
        let mut data = vec![0u8; stride * (rows + rows / 2)];
        for y in 0..rows {
            for x in 0..coded.width() as usize {
                data[y * stride + x] = (x + y) as u8;
            }
        }
        for y in 0..rows / 2 {
            for x in 0..coded.width() as usize / 2 {
                data[(rows + y) * stride + 2 * x] = x as u8;
                data[(rows + y) * stride + 2 * x + 1] = y as u8;
            }
        }
        data
    }

    #[test]
    fn crops_the_picture_into_i420_or_nv12() {
        let coded = size(32, 24);
        let data = buffer(coded, 40);
        let aperture = Aperture {
            x: 2,
            y: 4,
            size: size(16, 8),
        };
        let pts = MediaTime::new(7, Timebase::MILLIS);
        let i420 = picture(
            &data,
            40,
            coded,
            aperture,
            PixelFormat::I420,
            ColorInfo::default(),
            pts,
        )
        .expect("I420");
        assert_eq!(i420.info().size, size(16, 8));
        assert_eq!(i420.info().pts, pts);
        assert_eq!(i420.row(0, 0).expect("row")[0], 6, "luma at (2, 4)");
        assert_eq!(
            i420.row(1, 0).expect("row")[..2],
            [1, 2],
            "U from chroma x 1"
        );
        assert_eq!(i420.row(2, 1).expect("row")[0], 3, "V from chroma y 3");
        let nv12 = picture(
            &data,
            40,
            coded,
            aperture,
            PixelFormat::Nv12,
            ColorInfo::default(),
            pts,
        )
        .expect("NV12");
        assert_eq!(nv12.row(1, 1).expect("row")[..4], [1, 3, 2, 3]);
    }

    #[test]
    fn refuses_a_short_buffer_or_an_odd_aperture() {
        let coded = size(32, 24);
        let data = buffer(coded, 32);
        let whole = Aperture {
            x: 0,
            y: 0,
            size: coded,
        };
        let pts = MediaTime::new(0, Timebase::MILLIS);
        let run = |data: &[u8], stride, aperture| {
            picture(
                data,
                stride,
                coded,
                aperture,
                PixelFormat::I420,
                ColorInfo::default(),
                pts,
            )
        };
        assert!(run(&data, 32, whole).is_ok());
        assert!(run(&data[..data.len() - 1], 32, whole).is_err());
        assert!(
            run(&data, 16, whole).is_err(),
            "a stride narrower than a row"
        );
        let odd = Aperture {
            x: 1,
            y: 0,
            size: size(8, 8),
        };
        assert!(run(&data, 32, odd).is_err());
    }
}
