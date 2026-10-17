//! Codecs on Linux (ADR-0060 §6, §7).
//!
//! Video encode goes through VA-API. There is no software H.264 encoder until Cisco's
//! OpenH264 binary lands (ADR-0060 question 1, answered; it follows the ADR superseding
//! ADR-0031), and no AAC encoder until the Rust AAC-LC encoder exists (question 2).
//! [`VaapiCodecs`] lists the encoders the probe finds, picks one for a configuration
//! and opens a VA-API H.264 session on it.

use jstream_platform::media::Codec;
use jstream_platform::{HardwarePreference, PlatformError, VideoEncoderConfig};

use crate::VaEncoder;

/// The VA-API encoder to open for `config`, out of `encoders` (the probe's order).
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec other than H.264 and for
/// [`HardwarePreference::Avoid`] (no software encoder until the OpenH264 download lands);
/// [`PlatformError::MissingComponent`], with the distributions' remedy, when no driver
/// encodes H.264 (ADR-0060 §6).
pub fn choose_encoder<'a>(
    encoders: &'a [VaEncoder],
    config: &VideoEncoderConfig,
) -> Result<&'a VaEncoder, PlatformError> {
    if config.codec != Codec::H264 {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{:?} encoding on Linux (H.264 only, ADR-0060)",
                config.codec
            ),
        });
    }
    if config.hardware == HardwarePreference::Avoid {
        return Err(PlatformError::Unsupported {
            what: "a software H.264 encoder on Linux (none until Cisco's OpenH264 download \
                   lands, ADR-0060 question 1)"
                .into(),
        });
    }
    encoders
        .first()
        .ok_or_else(|| PlatformError::MissingComponent {
            component: "a VA-API driver with H.264 encode",
            remedy: "on Intel, install intel-media-va-driver-non-free (Debian, Ubuntu) or \
                 intel-media-driver (Fedora); on AMD with Fedora's Mesa, install \
                 mesa-va-drivers-freeworld from RPM Fusion."
                .into(),
        })
}

#[cfg(target_os = "linux")]
pub use backend::VaapiCodecs;

#[cfg(target_os = "linux")]
mod backend {
    use jstream_platform::media::Codec;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, Codecs, PlatformError,
        VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
    };

    use super::choose_encoder;
    use crate::vaapi::VaapiH264Encoder;
    use crate::{VaEncoder, probe_encoders};

    /// How an encoder is listed and reported.
    fn info(e: &VaEncoder) -> CodecInfo {
        CodecInfo {
            name: format!("VA-API H.264 ({}, {})", e.vendor, e.node),
            codec: Codec::H264,
            role: CodecRole::VideoEncoder,
            hardware: true,
        }
    }

    /// Codecs through VA-API (ADR-0060 §6): H.264 encode sessions on the probe's
    /// encoders. CPU frames only for now.
    #[derive(Debug)]
    pub struct VaapiCodecs {
        /// What the probe found, or why it couldn't run (libva missing).
        encoders: Result<Vec<VaEncoder>, PlatformError>,
    }

    impl VaapiCodecs {
        /// Probes the render nodes once. A missing libva isn't an error here: it is
        /// reported when an encoder is asked for.
        pub fn new() -> Self {
            Self {
                encoders: probe_encoders(),
            }
        }
    }

    impl Default for VaapiCodecs {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Codecs for VaapiCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            let encoders = self.encoders.as_deref().unwrap_or_default();
            Ok(encoders.iter().map(info).collect())
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            let encoders = self.encoders.as_ref().map_err(Clone::clone)?;
            let encoder = choose_encoder(encoders, config)?;
            Ok(Box::new(VaapiH264Encoder::open(
                encoder,
                config,
                info(encoder),
            )?))
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: format!(
                    "{:?} encoding on Linux (no AAC encoder until the Rust AAC-LC encoder \
                     exists, ADR-0060 question 2)",
                    config.codec
                ),
            })
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: format!(
                    "{:?} decoding on Linux (not in ADR-0060's scope)",
                    config.codec
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{Codec, FrameSize, PixelFormat, Timebase};
    use jstream_platform::{HardwarePreference, PlatformError, RateControl, VideoEncoderConfig};

    use super::choose_encoder;
    use crate::{H264Profile, VaEncoder, VaEntrypoint};

    fn config(codec: Codec, hardware: HardwarePreference) -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec,
            size: FrameSize::new(1920, 1080).expect("valid size"),
            input_format: PixelFormat::Nv12,
            frame_period: Timebase::new(1, 60).expect("valid period"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 120,
            hardware,
        }
    }

    fn intel() -> VaEncoder {
        VaEncoder {
            node: "/dev/dri/renderD128".into(),
            vendor: "Intel iHD driver".into(),
            profile: H264Profile::High,
            entrypoint: VaEntrypoint::EncSliceLp,
        }
    }

    #[test]
    fn h264_picks_the_first_encoder() {
        let encoders = [intel()];
        let chosen = choose_encoder(&encoders, &config(Codec::H264, HardwarePreference::Prefer))
            .expect("an encoder");
        assert_eq!(chosen.node, "/dev/dri/renderD128");
        assert!(
            choose_encoder(&encoders, &config(Codec::H264, HardwarePreference::Require)).is_ok()
        );
    }

    #[test]
    fn refusals_say_why() {
        let encoders = [intel()];
        for (codec, hw) in [
            (Codec::Hevc, HardwarePreference::Prefer),
            (Codec::H264, HardwarePreference::Avoid),
        ] {
            assert!(matches!(
                choose_encoder(&encoders, &config(codec, hw)),
                Err(PlatformError::Unsupported { .. })
            ));
        }
        // No H.264 encode driver is something to install (ADR-0060 §6).
        assert!(matches!(
            choose_encoder(&[], &config(Codec::H264, HardwarePreference::Prefer)),
            Err(PlatformError::MissingComponent { .. })
        ));
    }
}
