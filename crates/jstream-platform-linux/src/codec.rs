//! Codecs on Linux (ADR-0060 §6, §7).
//!
//! Video encode goes through VA-API. Software H.264 is Cisco's OpenH264 binary, once the
//! user has downloaded it (`crate::openh264`, ADR-0061): it serves
//! [`HardwarePreference::Avoid`], and `Prefer` on a machine with no VA-API encoder. Audio encode is JStream's own AAC-LC
//! encoder (`crate::aac`, ADR-0061 and ADR-0062), unless the `aac-encoder` feature is off.
//! For now, [`VaapiCodecs`] lists the encoders the probe finds and picks one for a
//! configuration; opening a video session itself reports `Unsupported` until the VA-API
//! encode path lands.

use jstream_platform::media::Codec;
use jstream_platform::{HardwarePreference, PlatformError, VideoEncoderConfig};

use crate::VaEncoder;

/// The VA-API encoder to open for `config`, out of `encoders` (the probe's order).
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec other than H.264 and for
/// [`HardwarePreference::Avoid`] (software H.264 is OpenH264's, not VA-API's);
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
            what: "a software H.264 encoder through VA-API (software H.264 on Linux is \
                   Cisco's OpenH264, ADR-0061)"
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
    use crate::openh264::{
        OPENH264_COMPONENT, OpenH264Store, openh264_encoder_info, wants_software,
    };
    use crate::{VaEncoder, probe_encoders};

    /// Codecs through VA-API (ADR-0060 §6), and the Rust AAC-LC encoder. For now it lists
    /// and chooses video encoders, and video sessions report `Unsupported`.
    #[derive(Debug)]
    pub struct VaapiCodecs {
        /// What the probe found, or why it couldn't run (libva missing).
        encoders: Result<Vec<VaEncoder>, PlatformError>,
        /// Where Cisco's OpenH264 is kept once downloaded; `None` without a data folder.
        openh264: Option<OpenH264Store>,
    }

    impl VaapiCodecs {
        /// Probes the render nodes once. A missing libva isn't an error here: it is
        /// reported when an encoder is asked for.
        pub fn new() -> Self {
            Self {
                encoders: probe_encoders(),
                openh264: OpenH264Store::in_data_dir(),
            }
        }

        /// The same backend with Cisco's OpenH264 kept in `store`, not the data folder.
        pub fn with_openh264(mut self, store: OpenH264Store) -> Self {
            self.openh264 = Some(store);
            self
        }

        /// The software H.264 encoder (ADR-0061): Cisco's OpenH264, if downloaded.
        fn software_encoder(
            &self,
            config: &VideoEncoderConfig,
            hardware: usize,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            let mut result = match &self.openh264 {
                Some(store) => store.video_encoder(config),
                None => Err(PlatformError::MissingComponent {
                    component: OPENH264_COMPONENT,
                    remedy: "JStream has no data folder to download it to".into(),
                }),
            };
            // With no hardware encoder either, say both ways out (ADR-0060 §6).
            if hardware == 0
                && let Err(PlatformError::MissingComponent { remedy, .. }) = &mut result
            {
                remedy.push_str(
                    "; or, for hardware encoding, install a VA-API driver with H.264 encode",
                );
            }
            result
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
            let software = self
                .openh264
                .as_ref()
                .filter(|s| s.is_installed())
                .map(|_| openh264_encoder_info());
            let video = encoders
                .iter()
                .map(|e| CodecInfo {
                    name: format!("VA-API H.264 ({}, {})", e.vendor, e.node),
                    codec: Codec::H264,
                    role: CodecRole::VideoEncoder,
                    hardware: true,
                })
                .chain(software);
            #[cfg(feature = "aac-encoder")]
            let list = video.chain([crate::aac::aac_encoder_info()]).collect();
            #[cfg(not(feature = "aac-encoder"))]
            let list = video.collect();
            Ok(list)
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            let hardware = self.encoders.as_ref().map_or(0, Vec::len);
            if wants_software(hardware, config) {
                return self.software_encoder(config, hardware);
            }
            let encoders = self.encoders.as_ref().map_err(Clone::clone)?;
            let encoder = choose_encoder(encoders, config)?;
            Err(PlatformError::Unsupported {
                what: format!(
                    "a VA-API H.264 session on {} (encode sessions are not built yet, \
                     ADR-0060)",
                    encoder.node
                ),
            })
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            #[cfg(feature = "aac-encoder")]
            {
                crate::aac::RustAacEncoder::open(config)
                    .map(|e| Box::new(e) as Box<dyn AudioEncoder>)
            }
            #[cfg(not(feature = "aac-encoder"))]
            {
                Err(PlatformError::Unsupported {
                    what: format!(
                        "{:?} encoding on Linux (this build has the AAC encoder switched \
                         off, ADR-0062)",
                        config.codec
                    ),
                })
            }
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
