//! Codecs on Linux (ADR-0060 §6, §7).
//!
//! Video encode goes through VA-API. Software H.264 is Cisco's OpenH264 binary, once the
//! user has downloaded it (`crate::openh264`, ADR-0061): it serves
//! [`HardwarePreference::Avoid`], and `Prefer` on a machine with no VA-API encoder.
//! Audio encode is JStream's own AAC-LC encoder (`crate::aac`, ADR-0061 and ADR-0062),
//! unless the `aac-encoder` feature is off.
//! [`VaapiCodecs`] lists the encoders the probe finds, picks one for a configuration
//! and opens a VA-API H.264 session on it. On the engine's GPU context
//! (`Codecs::on_context`) it keeps the encoders on that device's render node, whose
//! sessions take the context's encode targets.

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
    use std::os::unix::fs::MetadataExt;
    use std::sync::Arc;

    use jstream_platform::media::Codec;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, Codecs, GpuContext,
        HardwarePreference, PlatformError, VideoDecoder, VideoDecoderConfig, VideoEncoder,
        VideoEncoderConfig,
    };

    use super::choose_encoder;
    use crate::gpu::Shared;
    use crate::gpu::export::device_number;
    use crate::openh264::{
        OPENH264_COMPONENT, OpenH264Store, openh264_encoder_info, wants_software,
    };
    use crate::vaapi::VaapiH264Encoder;
    use crate::{VaEncoder, probe_encoders};

    /// The `(major, minor)` of the device file at `node`.
    fn node_number(node: &str) -> Option<(u64, u64)> {
        std::fs::metadata(node)
            .ok()
            .map(|m| device_number(m.rdev()))
    }

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
    /// encoders, on CPU frames, or on the GPU context's encode targets for the backend
    /// [`Codecs::on_context`] returns. And the Rust AAC-LC encoder.
    #[derive(Clone, Debug)]
    pub struct VaapiCodecs {
        /// What the probe found, or why it couldn't run (libva missing).
        encoders: Result<Vec<VaEncoder>, PlatformError>,
        /// Where Cisco's OpenH264 is kept once downloaded; `None` without a data folder.
        openh264: Option<OpenH264Store>,
        /// On the GPU context: its state. Encoders then take its encode targets only.
        gpu: Option<Arc<Shared>>,
    }

    impl VaapiCodecs {
        /// Probes the render nodes once. A missing libva isn't an error here: it is
        /// reported when an encoder is asked for.
        pub fn new() -> Self {
            Self {
                encoders: probe_encoders(),
                openh264: OpenH264Store::in_data_dir(),
                gpu: None,
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
            let video = encoders.iter().map(info).chain(software);
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
            if self.gpu.is_some() && config.hardware == HardwarePreference::Avoid {
                // OpenH264 reads CPU frames; the engine opens it on the CPU backend.
                return Err(PlatformError::Unsupported {
                    what: "a software H.264 encoder on GPU frames".into(),
                });
            }
            if self.gpu.is_none() && wants_software(hardware, config) {
                return self.software_encoder(config, hardware);
            }
            let encoders = self.encoders.as_ref().map_err(Clone::clone)?;
            let encoder = choose_encoder(encoders, config)?;
            Ok(Box::new(VaapiH264Encoder::open_on(
                encoder,
                config,
                info(encoder),
                self.gpu.clone(),
            )?))
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

        /// These codecs on `context`, when it can export encode targets and a VA-API
        /// encoder runs on its device: the encoders whose render node is the context's
        /// (VA-API and Vulkan must share the GPU for the DMA-BUF to import). `Ok(None)`
        /// otherwise, and the engine keeps CPU frames.
        fn on_context(
            &self,
            context: &GpuContext,
        ) -> Result<Option<Arc<dyn Codecs>>, PlatformError> {
            let shared = Shared::of(context, "VaapiCodecs::on_context")?;
            let Some(node) = shared.export.as_ref().map(|e| e.render_node) else {
                return Ok(None);
            };
            let Ok(encoders) = &self.encoders else {
                return Ok(None);
            };
            let on_device: Vec<VaEncoder> = encoders
                .iter()
                .filter(|e| node_number(&e.node) == Some(node))
                .cloned()
                .collect();
            if on_device.is_empty() {
                return Ok(None);
            }
            Ok(Some(Arc::new(Self {
                encoders: Ok(on_device),
                openh264: self.openh264.clone(),
                gpu: Some(shared),
            })))
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
