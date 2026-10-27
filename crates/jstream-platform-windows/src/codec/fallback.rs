//! The software fallback for a hardware video encoder that refuses its first frame
//! (#443, case 3 of its decision).
//!
//! A hardware encoder MFT can accept NV12 media types when it is configured and then
//! refuse system-memory samples at its first `ProcessInput`. With
//! [`HardwarePreference::Prefer`](jstream_platform::HardwarePreference::Prefer), that
//! refusal reopens the encoder on the OS software MFT and gives it the same frame.
//! Nothing has reached a muxer yet, so the stream sees one decoder configuration.
//!
//! The fallback is armed only until the hardware encoder takes a frame. A refusal after
//! that fails the session as before: switching encoders mid-stream would change the
//! codec configuration under a live stream. The fallback is never silent (ADR-0080): it
//! logs a warning naming both encoders and the refusal, and
//! [`info`](VideoEncoder::info) reports the software encoder from then on, which the
//! engine's `output.status` reads as `encoder.hardware`.
//!
//! The logic is platform-independent and tested with fake encoders on every platform;
//! `MfCodecs` wraps the Media Foundation encoder in it.

use jstream_platform::media::Packet;
use jstream_platform::{
    CodecInfo, HardwarePreference, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
};

/// The `call` of the error `MfVideoEncoder::encode` returns when the MFT refuses a
/// sample at `ProcessInput`.
pub(crate) const PROCESS_INPUT: &str = "IMFTransform::ProcessInput (video)";

/// Opens the software encoder to fall back to, with the hardware encoder's configuration
/// set to avoid hardware.
pub(crate) type OpenSoftware =
    Box<dyn FnOnce(&VideoEncoderConfig) -> Result<Box<dyn VideoEncoder>, PlatformError> + Send>;

/// A hardware video encoder that falls back to a software one when it refuses its first
/// frame at `ProcessInput`. Every other call goes to the encoder in use.
pub(crate) struct FirstFrameFallback {
    encoder: Box<dyn VideoEncoder>,
    config: VideoEncoderConfig,
    /// Opens the software encoder. `None` once the hardware encoder took a frame, or
    /// once the fallback has happened.
    software: Option<OpenSoftware>,
}

impl FirstFrameFallback {
    /// Wraps `hardware`, opened with `config`, so that a refusal of its first frame
    /// reopens the session through `software`.
    pub(crate) fn new(
        hardware: Box<dyn VideoEncoder>,
        config: VideoEncoderConfig,
        software: OpenSoftware,
    ) -> Self {
        Self {
            encoder: hardware,
            config,
            software: Some(software),
        }
    }

    /// Whether to wrap an encoder opened with `config` whose info is `info`: a hardware
    /// encoder opened with `Prefer` on the CPU path. `Require` has no software to fall
    /// back to, and `Avoid` opened software already.
    pub(crate) fn applies(config: &VideoEncoderConfig, info: &CodecInfo) -> bool {
        config.hardware == HardwarePreference::Prefer && info.hardware
    }
}

/// Whether `err` is the MFT refusing a sample at `ProcessInput`.
fn refused_input(err: &PlatformError) -> bool {
    matches!(err, PlatformError::Os { call, .. } if *call == PROCESS_INPUT)
}

impl VideoEncoder for FirstFrameFallback {
    fn info(&self) -> &CodecInfo {
        self.encoder.info()
    }

    fn can_accept(&self) -> bool {
        self.encoder.can_accept()
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let Some(open) = self.software.take() else {
            return self.encoder.encode(frame);
        };
        // Kept to give the software encoder, until the hardware one has taken a frame.
        let retry = frame.clone();
        let refusal = match self.encoder.encode(frame) {
            // The first frame is in: from here on a refusal fails the session.
            Ok(()) => return Ok(()),
            Err(err) if refused_input(&err) => err,
            Err(err) => {
                // Busy, or a frame the configuration doesn't take: still armed.
                self.software = Some(open);
                return Err(err);
            }
        };
        let config = VideoEncoderConfig {
            hardware: HardwarePreference::Avoid,
            ..self.config
        };
        let software = open(&config).map_err(|err| PlatformError::Unsupported {
            what: format!(
                "{} refused the first frame ({refusal}), and no software encoder opened: {err}",
                self.encoder.info().name
            ),
        })?;
        tracing::warn!(
            hardware = %self.encoder.info().name,
            software = %software.info().name,
            error = %refusal,
            "the hardware video encoder refused the first frame; encoding in software"
        );
        // The hardware session is dropped before the software one takes the frame.
        self.encoder = software;
        self.encoder.encode(retry)
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.encoder.poll(out)
    }

    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        self.encoder.request_keyframe()
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        // Finished before any frame: nothing to fall back for.
        self.software = None;
        self.encoder.finish(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, Packet, PixelFormat, Timebase, VideoFrame,
        VideoFrameInfo,
    };
    use jstream_platform::{
        CodecInfo, CodecRole, HardwarePreference, PlatformError, RateControl, VideoEncoder,
        VideoEncoderConfig, VideoSurface,
    };

    use super::{FirstFrameFallback, OpenSoftware, PROCESS_INPUT};

    fn config() -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec: Codec::H264,
            size: FrameSize::new(16, 16).expect("size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1, 30).expect("30 fps"),
            rate_control: RateControl::Cbr { kbps: 1000 },
            keyframe_interval: 30,
            hardware: HardwarePreference::Prefer,
        }
    }

    fn frame(i: i64) -> VideoSurface {
        VideoSurface::Cpu(
            VideoFrame::zeroed(VideoFrameInfo {
                size: FrameSize::new(16, 16).expect("size"),
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: MediaTime::new(i, Timebase::new(1, 30).expect("tb")),
            })
            .expect("frame"),
        )
    }

    /// What a fake encoder answers to each `encode`, in order; `Ok` after the list.
    #[derive(Clone, Copy)]
    enum Answer {
        Take,
        Refuse,
        Busy,
    }

    /// A fake encoder: answers `encode` from its script and makes a packet of each frame
    /// it takes.
    struct Fake {
        info: CodecInfo,
        script: Vec<Answer>,
        pending: Vec<Packet>,
        finished: bool,
    }

    impl Fake {
        fn new(name: &str, hardware: bool, script: &[Answer]) -> Box<Self> {
            Box::new(Self {
                info: CodecInfo {
                    name: name.into(),
                    codec: Codec::H264,
                    role: CodecRole::VideoEncoder,
                    hardware,
                },
                script: script.iter().rev().copied().collect(),
                pending: Vec::new(),
                finished: false,
            })
        }
    }

    impl VideoEncoder for Fake {
        fn info(&self) -> &CodecInfo {
            &self.info
        }

        fn can_accept(&self) -> bool {
            !self.finished
        }

        fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
            match self.script.pop().unwrap_or(Answer::Take) {
                Answer::Take => {
                    let pts = frame.info().pts;
                    let data = vec![0, 0, 0, 1, 0x65, u8::from(self.info.hardware)];
                    self.pending
                        .push(Packet::new(Codec::H264, true, pts, pts, data).expect("packet"));
                    Ok(())
                }
                Answer::Refuse => Err(PlatformError::Os {
                    call: PROCESS_INPUT,
                    code: Some(0xC00D_36B5),
                    message: "the sample was refused".into(),
                }),
                Answer::Busy => Err(PlatformError::Busy {
                    what: "video encoder",
                }),
            }
        }

        fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            out.append(&mut self.pending);
            Ok(())
        }

        fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
            self.finished = true;
            self.poll(out)
        }
    }

    /// Opens a software fake that takes every frame, counting the opens and checking
    /// the configuration avoids hardware.
    fn software(opens: &Arc<AtomicUsize>) -> OpenSoftware {
        let opens = Arc::clone(opens);
        Box::new(move |config: &VideoEncoderConfig| {
            assert_eq!(config.hardware, HardwarePreference::Avoid);
            assert_eq!(
                VideoEncoderConfig {
                    hardware: HardwarePreference::Prefer,
                    ..*config
                },
                self::config()
            );
            opens.fetch_add(1, Ordering::Relaxed);
            Ok(Fake::new("software", false, &[]) as Box<dyn VideoEncoder>)
        })
    }

    fn wrapped(script: &[Answer], opens: &Arc<AtomicUsize>) -> FirstFrameFallback {
        FirstFrameFallback::new(
            Fake::new("hardware", true, script),
            config(),
            software(opens),
        )
    }

    #[test]
    fn a_refused_first_frame_reopens_in_software_and_resubmits_it() {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut enc = wrapped(&[Answer::Refuse], &opens);
        assert!(enc.info().hardware);
        enc.encode(frame(0)).expect("the software encoder takes it");
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        // Reported, not silent: the info is the software encoder's.
        assert!(!enc.info().hardware);
        assert_eq!(enc.info().name, "software");
        enc.encode(frame(1)).expect("encode");
        let mut out = Vec::new();
        enc.finish(&mut out).expect("finish");
        // Both frames, the refused one first, all from the software encoder.
        let pts: Vec<_> = out.iter().map(|p| p.pts().ticks()).collect();
        assert_eq!(pts, [0, 1]);
        assert!(out.iter().all(|p| p.data()[5] == 0));
    }

    #[test]
    fn a_refusal_after_the_first_frame_fails() {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut enc = wrapped(&[Answer::Take, Answer::Refuse], &opens);
        enc.encode(frame(0)).expect("taken");
        let err = enc.encode(frame(1)).expect_err("refused mid-stream");
        assert!(matches!(err, PlatformError::Os { call, .. } if call == PROCESS_INPUT));
        assert_eq!(opens.load(Ordering::Relaxed), 0);
        assert!(enc.info().hardware);
    }

    #[test]
    fn a_busy_first_frame_keeps_the_fallback_armed() {
        let opens = Arc::new(AtomicUsize::new(0));
        let mut enc = wrapped(&[Answer::Busy, Answer::Refuse], &opens);
        assert!(matches!(
            enc.encode(frame(0)),
            Err(PlatformError::Busy { .. })
        ));
        assert!(enc.info().hardware);
        // The first frame the hardware encoder sees is still the first it is asked to take.
        enc.encode(frame(1)).expect("falls back");
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert!(!enc.info().hardware);
    }

    #[test]
    fn a_software_open_failure_names_both_refusals() {
        let mut enc = FirstFrameFallback::new(
            Fake::new("hardware", true, &[Answer::Refuse]),
            config(),
            Box::new(|_: &VideoEncoderConfig| {
                Err(PlatformError::Unsupported {
                    what: "no software encoder".into(),
                })
            }),
        );
        let err = enc.encode(frame(0)).expect_err("nothing to fall back to");
        let text = err.to_string();
        assert!(text.contains("hardware refused the first frame"), "{text}");
        assert!(text.contains("no software encoder"), "{text}");
    }

    #[test]
    fn only_a_preferred_hardware_encoder_is_wrapped() {
        let info = |hardware| CodecInfo {
            name: "x".into(),
            codec: Codec::H264,
            role: CodecRole::VideoEncoder,
            hardware,
        };
        let with = |hardware| VideoEncoderConfig {
            hardware,
            ..config()
        };
        assert!(FirstFrameFallback::applies(
            &with(HardwarePreference::Prefer),
            &info(true)
        ));
        // Prefer that opened software already, Require and Avoid have nothing to do.
        assert!(!FirstFrameFallback::applies(
            &with(HardwarePreference::Prefer),
            &info(false)
        ));
        assert!(!FirstFrameFallback::applies(
            &with(HardwarePreference::Require),
            &info(true)
        ));
        assert!(!FirstFrameFallback::applies(
            &with(HardwarePreference::Avoid),
            &info(false)
        ));
    }
}
