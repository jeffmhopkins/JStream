//! H.264 encoding through Media Foundation encoder MFTs, from CPU NV12 frames.
//!
//! This is the CPU path: frames are copied into a Media Foundation memory buffer. ARCH's
//! GPU pipeline feeds the encoder D3D11 textures instead; that arrives with the GPU
//! frame type, and the CPU path stays as the fallback (ARCH › GPU pipeline, step 6).
//!
//! Hardware encoders are tried first and the OS software encoder after them, as
//! [`HardwarePreference`](jstream_platform::HardwarePreference) says (ADR-0003).
//! Hardware MFTs are usually asynchronous: they ask for input and announce output
//! through events, which `poll` pumps.

use std::collections::VecDeque;

use jstream_platform::media::{Codec, MediaTime, PixelFormat, Rounding, Timebase, VideoFrame};
use jstream_platform::{PlatformError, RateControl, VideoEncoderConfig};

/// What the MFT is configured with, derived from a [`VideoEncoderConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// `MF_MT_FRAME_RATE`: frames per second as numerator and denominator.
    pub(crate) fps: (u32, u32),
    /// The duration of one frame in 100 ns units, rounded.
    pub(crate) frame_hns: i64,
    /// The mean bit rate, in bits per second.
    pub(crate) bitrate: u32,
    /// The peak bit rate for VBR, in bits per second; `None` for CBR.
    pub(crate) max_bitrate: Option<u32>,
}

/// Checks a configuration and derives the MFT settings.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, an input other than NV12, odd
/// dimensions (NV12 halves them for chroma), a zero keyframe interval, a zero bit rate or
/// a VBR peak below its mean.
pub(crate) fn check(config: &VideoEncoderConfig) -> Result<Settings, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::H264 {
        return unsupported(format!("{:?} from the H.264 encoder", config.codec));
    }
    if config.input_format != PixelFormat::Nv12 {
        return unsupported(format!(
            "H.264 from {:?} (the encoder takes NV12)",
            config.input_format
        ));
    }
    let (w, h) = (config.size.width(), config.size.height());
    if w % 2 != 0 || h % 2 != 0 {
        return unsupported(format!("H.264 at an odd size, {w}x{h}"));
    }
    if config.keyframe_interval == 0 {
        return unsupported("a keyframe interval of 0".into());
    }
    let (kbps, max) = match config.rate_control {
        RateControl::Cbr { kbps } => (kbps, None),
        RateControl::Vbr { kbps, max_kbps } => (kbps, Some(max_kbps)),
        other => return unsupported(format!("rate control {other:?}")),
    };
    if kbps == 0 || max.is_some_and(|m| m < kbps) {
        return unsupported(format!("rate control {:?}", config.rate_control));
    }
    let bps = |k: u32| k.saturating_mul(1000);
    // A frame period of num/den seconds is den/num frames per second.
    let p = config.frame_period;
    let frame_hns =
        i64::try_from(u64::from(p.num()) * 10_000_000 / u64::from(p.den())).unwrap_or(i64::MAX);
    Ok(Settings {
        fps: (p.den(), p.num()),
        frame_hns,
        bitrate: bps(kbps),
        max_bitrate: max.map(bps),
    })
}

/// Copies an NV12 frame into `out` as Media Foundation's contiguous NV12: the Y plane's
/// rows, then the interleaved UV plane's rows, each row `width` bytes with no padding.
/// `out` must hold `width * height * 3 / 2` bytes.
///
/// # Errors
/// [`PlatformError::Unsupported`] when the frame isn't NV12, and
/// [`PlatformError::InvalidState`] when `out` is the wrong size.
pub(crate) fn pack_nv12(frame: &VideoFrame, out: &mut [u8]) -> Result<(), PlatformError> {
    let info = frame.info();
    if info.format != PixelFormat::Nv12 {
        return Err(PlatformError::Unsupported {
            what: format!("packing {:?} as NV12", info.format),
        });
    }
    let w = info.size.width() as usize;
    let h = info.size.height();
    if out.len() != w * h as usize * 3 / 2 {
        return Err(PlatformError::InvalidState {
            what: "NV12 packing",
            state: "the output buffer doesn't fit the frame",
        });
    }
    let mut rows = out.chunks_exact_mut(w);
    for (plane, height) in [(0, h), (1, h / 2)] {
        for y in 0..height {
            // Both exist: NV12 has two planes, and `VideoFrame` checks their sizes.
            if let (Some(src), Some(dst)) = (frame.row(plane, y), rows.next()) {
                dst.copy_from_slice(&src[..w]);
            }
        }
    }
    Ok(())
}

/// The frames in flight: the time each was sent to the encoder with (100 ns units) and
/// the `pts` it came with, so each packet gets back its frame's exact `pts` however the
/// frame's time base rounds to 100 ns.
#[derive(Debug, Default)]
pub(crate) struct InFlight {
    frames: VecDeque<(i64, MediaTime)>,
}

/// 100 ns ticks, the unit of Media Foundation sample times.
pub(crate) fn hns() -> Timebase {
    crate::clock::timebase()
}

impl InFlight {
    /// Records a frame and returns its sample time in 100 ns units.
    pub(crate) fn send(&mut self, pts: MediaTime) -> Result<i64, PlatformError> {
        let t = pts.rescale(hns(), Rounding::Nearest)?.ticks();
        self.frames.push_back((t, pts));
        Ok(t)
    }

    /// The `pts` of the frame an output sample with this time encodes. Frames before it
    /// that the encoder dropped are forgotten. A time the map doesn't know (or none) is
    /// returned as is, in 100 ns units.
    pub(crate) fn receive(&mut self, time: Option<i64>) -> MediaTime {
        let Some(t) = time else {
            return match self.frames.pop_front() {
                Some((_, pts)) => pts,
                None => MediaTime::zero(hns()),
            };
        };
        while let Some(&(sent, pts)) = self.frames.front() {
            if sent > t {
                break;
            }
            self.frames.pop_front();
            if sent == t {
                return pts;
            }
        }
        MediaTime::new(t, hns())
    }

    /// Frames sent and not yet received.
    pub(crate) fn len(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(windows)]
pub(crate) use encoder::MfH264Encoder;

#[cfg(windows)]
mod encoder {
    use std::time::{Duration, Instant};

    use jstream_platform::media::{Codec, Packet, PixelFormat};
    use jstream_platform::{
        CodecInfo, CodecRole, HardwarePreference, PlatformError, VideoEncoder, VideoEncoderConfig,
        VideoSurface,
    };
    use windows::Win32::Media::MediaFoundation::{
        CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
        CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
        CODECAPI_AVEncMPVGOPSize, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaEventGenerator,
        IMFMediaType, IMFTransform, METransformDrainComplete, METransformHaveOutput,
        METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_E_NOTACCEPTING, MF_EVENT_FLAG_NO_WAIT,
        MF_EVENT_TYPE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
        MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
        MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_YUV_MATRIX, MF_TRANSFORM_ASYNC, MF_TRANSFORM_ASYNC_UNLOCK,
        MFCreateMediaType, MFMediaType_Video, MFNominalRange_16_235, MFT_CATEGORY_VIDEO_ENCODER,
        MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
        MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFVideoFormat_H264,
        MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoTransferMatrix_BT709,
        eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR,
        eAVEncH264VProfile_High,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{GUID, Interface};

    use super::{InFlight, Settings, check, pack_nv12};
    use crate::codec::mf::{self, Found, MfRuntime, Mft, Output};
    use crate::com::{OsResult, os_error};

    /// How long `finish` waits for an asynchronous encoder to drain.
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

    /// One H.264 encoder session on a Media Foundation encoder MFT.
    pub(crate) struct MfH264Encoder {
        // Fields drop in order: COM objects before the runtime that keeps MF started.
        mft: Mft,
        /// The event queue of an asynchronous MFT; `None` for a synchronous one.
        events: Option<IMFMediaEventGenerator>,
        output: Output,
        info: CodecInfo,
        config: VideoEncoderConfig,
        settings: Settings,
        /// Inputs an asynchronous MFT has asked for and not yet received.
        need_input: u32,
        in_flight: InFlight,
        /// Staging for one packed NV12 frame.
        nv12: Vec<u8>,
        pending: Vec<Packet>,
        drained: bool,
        finished: bool,
        _mf: MfRuntime,
    }

    // SAFETY: the session is owned and driven by one thread at a time (`&mut self` for
    // every call except `can_accept`, which reads a plain field). Media Foundation
    // encoder MFTs are in-proc, free-threaded objects, and `MfRuntime` keeps the process
    // MTA alive, so a thread that never initialised COM may call them as an implicit MTA
    // member.
    unsafe impl Send for MfH264Encoder {}

    impl MfH264Encoder {
        /// Opens an H.264 encoder that takes NV12: a hardware MFT first (unless the
        /// preference avoids hardware), then the software one (unless it requires
        /// hardware). The first that accepts the configuration wins.
        pub(crate) fn open(config: &VideoEncoderConfig) -> Result<Self, PlatformError> {
            let settings = check(config)?;
            let rt = MfRuntime::start()?;
            let io = (
                (MFMediaType_Video, MFVideoFormat_NV12),
                (MFMediaType_Video, MFVideoFormat_H264),
            );
            let mut candidates = Vec::new();
            if config.hardware != HardwarePreference::Avoid {
                let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
                for found in mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, io.0, io.1)? {
                    candidates.push((found, true));
                }
            }
            if config.hardware != HardwarePreference::Require {
                let flags =
                    MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER;
                for found in mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, io.0, io.1)? {
                    candidates.push((found, false));
                }
            }
            let mut refusals = Vec::new();
            for (found, hardware) in candidates {
                match Self::configure(&found, config, settings) {
                    Ok((mft, events, output)) => {
                        let size = config.size;
                        let frame_bytes = size.width() as usize * size.height() as usize * 3 / 2;
                        return Ok(Self {
                            mft,
                            events,
                            output,
                            info: CodecInfo {
                                name: found.name,
                                codec: Codec::H264,
                                role: CodecRole::VideoEncoder,
                                hardware,
                            },
                            config: *config,
                            settings,
                            need_input: 0,
                            in_flight: InFlight::default(),
                            nv12: vec![0; frame_bytes],
                            pending: Vec::new(),
                            drained: false,
                            finished: false,
                            _mf: rt,
                        });
                    }
                    Err(e) => refusals.push(format!("{}: {e}", found.name)),
                }
            }
            Err(PlatformError::Unsupported {
                what: if refusals.is_empty() {
                    format!(
                        "H.264 encoding with {:?}: no matching encoder MFT is installed",
                        config.hardware
                    )
                } else {
                    format!(
                        "H.264 encoding at {}x{}: every encoder refused ({})",
                        config.size.width(),
                        config.size.height(),
                        refusals.join("; ")
                    )
                },
            })
        }

        /// Activates and configures one candidate, and starts it streaming.
        fn configure(
            found: &Found,
            config: &VideoEncoderConfig,
            s: Settings,
        ) -> Result<(Mft, Option<IMFMediaEventGenerator>, Output), PlatformError> {
            let mft = found.activate()?;
            let transform = &mft.transform;
            // SAFETY: plain COM calls on a live transform.
            let attrs = unsafe { transform.GetAttributes() }.os("IMFTransform::GetAttributes")?;
            // SAFETY: as above.
            let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
            if is_async {
                // SAFETY: as above; an async MFT refuses every call until unlocked.
                unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                    .os("MF_TRANSFORM_ASYNC_UNLOCK")?;
            }
            // Best effort: an encoder without ICodecAPI, or one that refuses a setting,
            // still encodes with its defaults and the media type's bit rate.
            if let Ok(api) = transform.cast::<ICodecAPI>() {
                let mode = match s.max_bitrate {
                    None => eAVEncCommonRateControlMode_CBR.0,
                    Some(_) => eAVEncCommonRateControlMode_PeakConstrainedVBR.0,
                };
                let _ = set_u32(&api, &CODECAPI_AVEncCommonRateControlMode, mode as u32);
                let _ = set_u32(&api, &CODECAPI_AVEncCommonMeanBitRate, s.bitrate);
                if let Some(max) = s.max_bitrate {
                    let _ = set_u32(&api, &CODECAPI_AVEncCommonMaxBitRate, max);
                }
                let _ = set_u32(&api, &CODECAPI_AVEncMPVGOPSize, config.keyframe_interval);
                // No B-frames, so packets come out in presentation order and dts == pts.
                let _ = set_u32(&api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
                let _ = set_bool(&api, &CODECAPI_AVLowLatencyMode, true);
            }
            let (w, h) = (config.size.width(), config.size.height());
            // Encoders take the output type first.
            // SAFETY: plain COM calls on live objects created here.
            unsafe {
                let out = MFCreateMediaType().os("MFCreateMediaType")?;
                video_type(&out, &MFVideoFormat_H264, w, h, s.fps)?;
                out.SetUINT32(&MF_MT_AVG_BITRATE, s.bitrate)
                    .and_then(|()| {
                        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
                    })
                    .os("IMFMediaType::Set* (H.264 output)")?;
                transform
                    .SetOutputType(0, &out, 0)
                    .os("IMFTransform::SetOutputType (H.264)")?;
                let input = nv12_input_type(transform)?;
                video_type(&input, &MFVideoFormat_NV12, w, h, s.fps)?;
                // BT.709 limited range, the colour ARCH's pipeline encodes in (S1).
                input
                    .SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
                    .and_then(|()| {
                        input.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)
                    })
                    .os("IMFMediaType::Set* (NV12 input)")?;
                transform
                    .SetInputType(0, &input, 0)
                    .os("IMFTransform::SetInputType (NV12)")?;
            }
            // An access unit never exceeds an uncompressed frame.
            let output = Output::new(transform, w * h * 3 / 2)?;
            let events = if is_async {
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .os("async MFT as IMFMediaEventGenerator")?,
                )
            } else {
                None
            };
            // SAFETY: plain COM calls on a configured transform.
            unsafe {
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .and_then(|()| transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                    .os("IMFTransform::ProcessMessage (start)")?;
            }
            Ok((mft, events, output))
        }

        /// Handles every queued event of an asynchronous MFT without blocking, or pulls
        /// every ready output of a synchronous one.
        fn pump(&mut self) -> Result<(), PlatformError> {
            let Some(events) = self.events.clone() else {
                return self.collect_all();
            };
            loop {
                // SAFETY: plain COM call on a live event generator.
                let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(e) => e,
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                    Err(e) => return Err(os_error("IMFMediaEventGenerator::GetEvent", &e)),
                };
                // SAFETY: plain COM call on a live event.
                let kind =
                    MF_EVENT_TYPE(unsafe { event.GetType() }.os("IMFMediaEvent::GetType")? as i32);
                if kind == METransformNeedInput {
                    self.need_input = self.need_input.saturating_add(1);
                } else if kind == METransformHaveOutput {
                    self.collect_one()?;
                } else if kind == METransformDrainComplete {
                    self.drained = true;
                }
            }
        }

        /// Pulls output until the MFT needs more input (synchronous MFTs).
        fn collect_all(&mut self) -> Result<(), PlatformError> {
            while self.collect_one()? {}
            Ok(())
        }

        /// Pulls one output sample into `pending`; false when there was none.
        fn collect_one(&mut self) -> Result<bool, PlatformError> {
            let Some(sample) = self.output.pull(&self.mft.transform)? else {
                return Ok(false);
            };
            let pts = self.in_flight.receive(sample.time);
            self.pending.push(Packet::new(
                Codec::H264,
                sample.keyframe,
                pts,
                pts,
                sample.data,
            )?);
            Ok(true)
        }
    }

    impl VideoEncoder for MfH264Encoder {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        /// For an asynchronous MFT this reflects the input requests seen by the last
        /// `encode` or `poll`, so the encode thread calls `poll` every tick.
        fn can_accept(&self) -> bool {
            !self.finished && (self.events.is_none() || self.need_input > 0)
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            if self.finished {
                return Err(PlatformError::InvalidState {
                    what: "H.264 encode",
                    state: "the encoder is finished",
                });
            }
            if self.events.is_some() {
                self.pump()?;
            }
            if !self.can_accept() {
                return Err(PlatformError::Busy {
                    what: "H.264 encoder",
                });
            }
            let frame = match frame {
                VideoSurface::Cpu(f) => f,
                _ => {
                    return Err(PlatformError::Unsupported {
                        what: "a non-CPU frame on the CPU H.264 path".into(),
                    });
                }
            };
            let info = frame.info();
            if info.size != self.config.size || info.format != PixelFormat::Nv12 {
                return Err(PlatformError::Unsupported {
                    what: format!(
                        "a {:?} {}x{} frame (configured for NV12 {}x{})",
                        info.format,
                        info.size.width(),
                        info.size.height(),
                        self.config.size.width(),
                        self.config.size.height()
                    ),
                });
            }
            pack_nv12(&frame, &mut self.nv12)?;
            let time = self.in_flight.send(info.pts)?;
            let sample = mf::sample_from(&self.nv12, time, self.settings.frame_hns)?;
            // SAFETY: plain COM call; the sample is a live object built above.
            match unsafe { self.mft.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING && self.events.is_none() => {
                    // A synchronous MFT only refuses input while it holds output.
                    self.collect_all()?;
                    // SAFETY: as above.
                    unsafe { self.mft.transform.ProcessInput(0, &sample, 0) }
                        .os("IMFTransform::ProcessInput (H.264)")?;
                }
                Err(e) => return Err(os_error("IMFTransform::ProcessInput (H.264)", &e)),
            }
            self.need_input = self.need_input.saturating_sub(1);
            self.pump()
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished || !self.drained {
                self.pump()?;
            }
            out.append(&mut self.pending);
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            if !self.finished {
                self.finished = true;
                // SAFETY: plain COM calls on a streaming transform.
                unsafe {
                    self.mft
                        .transform
                        .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                        .and_then(|()| {
                            self.mft
                                .transform
                                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                        })
                        .os("IMFTransform::ProcessMessage (drain)")?;
                }
                if self.events.is_none() {
                    self.collect_all()?;
                    self.drained = true;
                } else {
                    let deadline = Instant::now() + DRAIN_TIMEOUT;
                    while !self.drained {
                        self.pump()?;
                        if Instant::now() > deadline {
                            out.append(&mut self.pending);
                            return Err(PlatformError::Os {
                                call: "MFT_MESSAGE_COMMAND_DRAIN",
                                code: None,
                                message: format!(
                                    "the encoder didn't drain within {} s ({} frames still in flight)",
                                    DRAIN_TIMEOUT.as_secs(),
                                    self.in_flight.len()
                                ),
                            });
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            out.append(&mut self.pending);
            Ok(())
        }
    }

    /// The MFT's own NV12 input type, which carries any attributes it needs; a fresh
    /// one if it lists none.
    fn nv12_input_type(transform: &IMFTransform) -> Result<IMFMediaType, PlatformError> {
        for i in 0.. {
            // SAFETY: plain COM call; the index only selects an entry.
            let Ok(t) = (unsafe { transform.GetInputAvailableType(0, i) }) else {
                break;
            };
            // SAFETY: plain COM call on a live media type.
            if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_NV12) {
                return Ok(t);
            }
        }
        // SAFETY: plain constructor call.
        unsafe { MFCreateMediaType() }.os("MFCreateMediaType")
    }

    /// Sets the attributes every video media type here carries.
    ///
    /// # Safety
    /// Plain COM calls on a live media type; marked `unsafe` only because the callers'
    /// blocks already are.
    unsafe fn video_type(
        t: &IMFMediaType,
        subtype: &GUID,
        w: u32,
        h: u32,
        fps: (u32, u32),
    ) -> Result<(), PlatformError> {
        let pack = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
        // SAFETY: plain COM calls on a live media type.
        unsafe {
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|()| t.SetGUID(&MF_MT_SUBTYPE, subtype))
                .and_then(|()| t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)))
                .and_then(|()| t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps.0, fps.1)))
                .and_then(|()| t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)))
                .and_then(|()| {
                    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
        }
        .os("IMFMediaType::Set* (video)")
    }

    /// Sets an `ICodecAPI` value as a `VT_UI4` VARIANT.
    fn set_u32(api: &ICodecAPI, key: &GUID, value: u32) -> windows::core::Result<()> {
        // SAFETY: plain COM call; the VARIANT outlives it.
        unsafe { api.SetValue(key, &VARIANT::from(value)) }
    }

    /// Sets an `ICodecAPI` value as a `VT_BOOL` VARIANT.
    fn set_bool(api: &ICodecAPI, key: &GUID, value: bool) -> windows::core::Result<()> {
        // SAFETY: plain COM call; the VARIANT outlives it.
        unsafe { api.SetValue(key, &VARIANT::from(value)) }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Plane, Timebase, VideoFrame,
        VideoFrameInfo,
    };
    use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

    use super::{InFlight, check, hns, pack_nv12};

    fn config() -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::H264,
            size: FrameSize::new(1920, 1080).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1001, 30_000).expect("29.97"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 60,
            hardware: HardwarePreference::Prefer,
        }
    }

    #[test]
    fn derives_mft_settings() {
        let s = check(&config()).expect("valid");
        assert_eq!(s.fps, (30_000, 1001));
        assert_eq!(s.frame_hns, 333_666);
        assert_eq!(s.bitrate, 6_000_000);
        assert_eq!(s.max_bitrate, None);
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 8000,
        };
        c.frame_period = Timebase::new(1, 60).expect("60");
        let s = check(&c).expect("valid");
        assert_eq!((s.fps, s.frame_hns), ((60, 1), 166_666));
        assert_eq!(s.max_bitrate, Some(8_000_000));
    }

    #[test]
    fn refuses_what_the_path_cant_encode() {
        let refused =
            |c: VideoEncoderConfig| matches!(check(&c), Err(PlatformError::Unsupported { .. }));
        let mut c = config();
        c.codec = Codec::Aac;
        assert!(refused(c));
        let mut c = config();
        c.input_format = PixelFormat::Bgra8;
        assert!(refused(c));
        let mut c = config();
        c.size = FrameSize::new(1921, 1080).expect("size");
        assert!(refused(c));
        let mut c = config();
        c.keyframe_interval = 0;
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 3000,
        };
        assert!(refused(c));
        let mut c = config();
        c.rate_control = RateControl::Cbr { kbps: 0 };
        assert!(refused(c));
    }

    fn nv12(w: u32, h: u32, stride: usize) -> VideoFrame {
        let info = VideoFrameInfo {
            size: FrameSize::new(w, h).expect("size"),
            format: PixelFormat::Nv12,
            color: ColorInfo::default(),
            pts: MediaTime::zero(Timebase::NANOS),
        };
        // Row `y` of plane `p` is filled with `p * 100 + y`, padding with 0xEE.
        let plane = |p: u8, rows: u32| {
            let mut data = vec![0xEE; stride * rows as usize];
            for y in 0..rows as usize {
                data[y * stride..y * stride + w as usize].fill(p * 100 + y as u8);
            }
            Plane::new(data, stride)
        };
        VideoFrame::new(info, vec![plane(0, h), plane(1, h / 2)]).expect("frame")
    }

    #[test]
    fn packs_nv12_without_row_padding() {
        let frame = nv12(4, 4, 8);
        let mut out = vec![0; 4 * 4 * 3 / 2];
        pack_nv12(&frame, &mut out).expect("pack");
        assert_eq!(
            out,
            [
                0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, // Y
                100, 100, 100, 100, 101, 101, 101, 101, // UV
            ]
        );
        let mut wrong = vec![0; 5];
        assert!(pack_nv12(&frame, &mut wrong).is_err());
    }

    #[test]
    fn packets_get_their_frames_exact_pts_back() {
        let mut f = InFlight::default();
        let tb = Timebase::MPEG_90K;
        // 1/90000 s doesn't divide 100 ns evenly for every value; the map keeps the
        // original.
        let a = MediaTime::new(3_003, tb);
        let b = MediaTime::new(6_006, tb);
        let c = MediaTime::new(9_009, tb);
        let ta = f.send(a).expect("send");
        let tb_ = f.send(b).expect("send");
        let tc = f.send(c).expect("send");
        assert_eq!(f.len(), 3);
        assert_eq!(f.receive(Some(ta)), a);
        // The encoder dropped `b`: receiving `c` forgets it.
        assert_eq!(f.receive(Some(tc)), c);
        assert_eq!(f.len(), 0);
        let _ = tb_;
        // An unknown time comes back as 100 ns ticks.
        let odd = f.receive(Some(42));
        assert_eq!((odd.ticks(), odd.timebase()), (42, hns()));
        // No time at all takes the oldest frame in flight.
        let d = MediaTime::new(1, tb);
        f.send(d).expect("send");
        assert_eq!(f.receive(None), d);
    }
}
