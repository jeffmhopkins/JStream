//! Codecs on macOS: VideoToolbox for video, AudioToolbox for AAC (ADR-0063 §6, §7).
//!
//! The platform-independent part turns the engine's encoder configurations into the
//! settings a `VTCompressionSession` and an AAC `AudioConverter` are opened with, and
//! refuses what they can't do. On macOS, `VideoToolboxCodecs` lists VideoToolbox's H.264
//! and HEVC encoders and opens their sessions (`crate::videotoolbox`); the AAC converter
//! and decoding report `Unsupported` until they land.

use jstream_platform::media::{ChannelLayout, Codec, PixelFormat};
use jstream_platform::{
    AudioEncoderConfig, HardwarePreference, PlatformError, RateControl, VideoEncoderConfig,
};

use crate::fourcc::{cv_pixel_format, fourcc};

/// `kCMVideoCodecType_H264`.
pub const CM_H264: u32 = fourcc(b"avc1");
/// `kCMVideoCodecType_HEVC`.
pub const CM_HEVC: u32 = fourcc(b"hvc1");
/// `kAudioFormatMPEG4AAC`: AAC-LC.
pub const AUDIO_AAC: u32 = fourcc(b"aac ");

/// How a VideoToolbox session spends bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtBitRate {
    /// `kVTCompressionPropertyKey_ConstantBitRate`, in bits per second (macOS 13+).
    Constant(u64),
    /// `kVTCompressionPropertyKey_AverageBitRate` with a peak in
    /// `kVTCompressionPropertyKey_DataRateLimits`, both in bits per second.
    Average {
        /// The average.
        bps: u64,
        /// The peak.
        max_bps: u64,
    },
}

/// What a `VTCompressionSession` is opened with for one [`VideoEncoderConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtEncoderSettings {
    /// The Core Media codec type ([`CM_H264`] or [`CM_HEVC`]).
    pub codec_type: u32,
    /// The Core Video pixel format of the input buffers.
    pub pixel_format: u32,
    /// `kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder`.
    pub enable_hardware: bool,
    /// `kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder`.
    pub require_hardware: bool,
    /// Rate control.
    pub bit_rate: VtBitRate,
    /// `kVTCompressionPropertyKey_MaxKeyFrameInterval`, in frames.
    pub max_keyframe_interval: u32,
}

/// The VideoToolbox settings for `config`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec VideoToolbox doesn't encode (AV1, audio
/// codecs), an input format it doesn't take (P010 is HEVC only; I420 and RGBA are not
/// encoder inputs), a zero bit rate, a peak below the average, or a rate-control mode
/// added after this mapping.
pub fn vt_encoder_settings(
    config: &VideoEncoderConfig,
) -> Result<VtEncoderSettings, PlatformError> {
    let codec_type = match config.codec {
        Codec::H264 => CM_H264,
        Codec::Hevc => CM_HEVC,
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("{other:?} encoding on macOS (VideoToolbox: H.264 and HEVC)"),
            });
        }
    };
    let input_ok = match config.input_format {
        PixelFormat::Nv12 | PixelFormat::Bgra8 => true,
        PixelFormat::P010 => config.codec == Codec::Hevc,
        _ => false,
    };
    let pixel_format = cv_pixel_format(config.input_format)
        .filter(|_| input_ok)
        .ok_or_else(|| PlatformError::Unsupported {
            what: format!(
                "{:?} input to a {:?} encoder on macOS",
                config.input_format, config.codec
            ),
        })?;
    let kbps_to_bps = |kbps: u32| u64::from(kbps) * 1000;
    let bit_rate = match config.rate_control {
        RateControl::Cbr { kbps } if kbps > 0 => VtBitRate::Constant(kbps_to_bps(kbps)),
        RateControl::Vbr { kbps, max_kbps } if kbps > 0 && max_kbps >= kbps => VtBitRate::Average {
            bps: kbps_to_bps(kbps),
            max_bps: kbps_to_bps(max_kbps),
        },
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("rate control {other:?} on macOS"),
            });
        }
    };
    let (enable_hardware, require_hardware) = match config.hardware {
        HardwarePreference::Prefer => (true, false),
        HardwarePreference::Require => (true, true),
        HardwarePreference::Avoid => (false, false),
    };
    Ok(VtEncoderSettings {
        codec_type,
        pixel_format,
        enable_hardware,
        require_hardware,
        bit_rate,
        max_keyframe_interval: config.keyframe_interval,
    })
}

/// What an AAC-LC `AudioConverter` is opened with for one [`AudioEncoderConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AacSettings {
    /// The output format ID, [`AUDIO_AAC`].
    pub format_id: u32,
    /// Frames per second, the input's.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u32,
    /// `kAudioConverterEncodeBitRate`, in bits per second.
    pub bit_rate: u32,
}

/// The AudioToolbox settings for `config` (ADR-0063 §7): AAC-LC, mono or stereo, at
/// 44.1 or 48 kHz.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, sample rate or channel layout, or a
/// bit rate outside 32 to 320 kbps.
pub fn aac_settings(config: &AudioEncoderConfig) -> Result<AacSettings, PlatformError> {
    let format = config.input_format;
    if config.codec != Codec::Aac {
        return Err(PlatformError::Unsupported {
            what: format!("{:?} audio encoding on macOS (AAC only)", config.codec),
        });
    }
    let channels = match format.layout() {
        ChannelLayout::Mono => 1,
        ChannelLayout::Stereo => 2,
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("{other:?} AAC on macOS (mono or stereo)"),
            });
        }
    };
    if !matches!(format.sample_rate(), 44_100 | 48_000) {
        return Err(PlatformError::Unsupported {
            what: format!(
                "AAC at {} Hz on macOS (44.1 or 48 kHz)",
                format.sample_rate()
            ),
        });
    }
    if !(32..=320).contains(&config.kbps) {
        return Err(PlatformError::Unsupported {
            what: format!("AAC at {} kbps on macOS (32 to 320)", config.kbps),
        });
    }
    Ok(AacSettings {
        format_id: AUDIO_AAC,
        sample_rate: format.sample_rate(),
        channels,
        bit_rate: config.kbps * 1000,
    })
}

#[cfg(target_os = "macos")]
pub use backend::VideoToolboxCodecs;

#[cfg(target_os = "macos")]
mod backend {
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecInfo, Codecs, PlatformError, VideoDecoder,
        VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
    };

    use super::aac_settings;
    use crate::videotoolbox::{VtEncoder, list_encoders};

    fn not_built(what: &str) -> PlatformError {
        PlatformError::Unsupported {
            what: format!("{what} on macOS (not built yet, ADR-0063)"),
        }
    }

    /// Codecs through VideoToolbox and AudioToolbox (ADR-0063 §6, §7). It lists and opens
    /// VideoToolbox's H.264 and HEVC encoders, which take CPU frames for now. The AAC
    /// converter and decoding report `Unsupported` until they land.
    #[derive(Debug, Default)]
    pub struct VideoToolboxCodecs;

    impl VideoToolboxCodecs {
        /// The backend.
        pub fn new() -> Self {
            Self
        }
    }

    impl Codecs for VideoToolboxCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            list_encoders()
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            Ok(Box::new(VtEncoder::open(config)?))
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            aac_settings(config)?;
            Err(not_built("an AudioToolbox AAC converter"))
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Err(not_built(&format!(
                "{:?} decoding through VideoToolbox",
                config.codec
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        AudioFormat, ChannelLayout, Codec, FrameSize, PixelFormat, SampleFormat, Timebase,
    };
    use jstream_platform::{
        AudioEncoderConfig, HardwarePreference, PlatformError, RateControl, VideoEncoderConfig,
    };

    use super::{AUDIO_AAC, CM_H264, CM_HEVC, VtBitRate, aac_settings, vt_encoder_settings};
    use crate::fourcc::{CV_420V, CV_X420};

    fn video(codec: Codec, input_format: PixelFormat) -> VideoEncoderConfig {
        VideoEncoderConfig {
            codec,
            size: FrameSize::new(1920, 1080).expect("valid size"),
            input_format,
            frame_period: Timebase::new(1, 60).expect("valid period"),
            rate_control: RateControl::Cbr { kbps: 6000 },
            keyframe_interval: 120,
            hardware: HardwarePreference::Prefer,
        }
    }

    fn audio(rate: u32, layout: ChannelLayout, kbps: u32) -> AudioEncoderConfig {
        AudioEncoderConfig {
            codec: Codec::Aac,
            input_format: AudioFormat::new(rate, layout, SampleFormat::F32Interleaved)
                .expect("valid format"),
            kbps,
        }
    }

    #[test]
    fn h264_at_1080p60_maps_to_a_cbr_hardware_session() {
        let s = vt_encoder_settings(&video(Codec::H264, PixelFormat::Nv12)).expect("settings");
        assert_eq!(s.codec_type, CM_H264);
        assert_eq!(s.pixel_format, CV_420V);
        assert!(s.enable_hardware && !s.require_hardware);
        assert_eq!(s.bit_rate, VtBitRate::Constant(6_000_000));
        assert_eq!(s.max_keyframe_interval, 120);
    }

    #[test]
    fn hardware_preferences_map_to_the_encoder_specification() {
        for (pref, enable, require) in [
            (HardwarePreference::Prefer, true, false),
            (HardwarePreference::Require, true, true),
            (HardwarePreference::Avoid, false, false),
        ] {
            let mut c = video(Codec::Hevc, PixelFormat::Nv12);
            c.hardware = pref;
            let s = vt_encoder_settings(&c).expect("settings");
            assert_eq!(s.codec_type, CM_HEVC);
            assert_eq!((s.enable_hardware, s.require_hardware), (enable, require));
        }
    }

    #[test]
    fn vbr_carries_its_peak() {
        let mut c = video(Codec::H264, PixelFormat::Bgra8);
        c.rate_control = RateControl::Vbr {
            kbps: 4000,
            max_kbps: 8000,
        };
        let s = vt_encoder_settings(&c).expect("settings");
        assert_eq!(
            s.bit_rate,
            VtBitRate::Average {
                bps: 4_000_000,
                max_bps: 8_000_000
            }
        );
    }

    #[test]
    fn what_videotoolbox_cant_do_is_refused() {
        let p010_hevc = vt_encoder_settings(&video(Codec::Hevc, PixelFormat::P010));
        assert_eq!(p010_hevc.map(|s| s.pixel_format).ok(), Some(CV_X420));
        let mut zero = video(Codec::H264, PixelFormat::Nv12);
        zero.rate_control = RateControl::Cbr { kbps: 0 };
        let mut inverted = video(Codec::H264, PixelFormat::Nv12);
        inverted.rate_control = RateControl::Vbr {
            kbps: 8000,
            max_kbps: 4000,
        };
        for c in [
            video(Codec::Av1, PixelFormat::Nv12),
            video(Codec::Aac, PixelFormat::Nv12),
            video(Codec::H264, PixelFormat::P010),
            video(Codec::H264, PixelFormat::I420),
            video(Codec::Hevc, PixelFormat::Rgba8),
            zero,
            inverted,
        ] {
            assert!(
                matches!(
                    vt_encoder_settings(&c),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{c:?}"
            );
        }
    }

    #[test]
    fn aac_maps_to_an_audio_converter() {
        let s = aac_settings(&audio(48_000, ChannelLayout::Stereo, 160)).expect("settings");
        assert_eq!(s.format_id, AUDIO_AAC);
        assert_eq!(
            (s.sample_rate, s.channels, s.bit_rate),
            (48_000, 2, 160_000)
        );
        assert!(aac_settings(&audio(44_100, ChannelLayout::Mono, 64)).is_ok());
        let mut not_aac = audio(48_000, ChannelLayout::Stereo, 160);
        not_aac.codec = Codec::H264;
        for c in [
            audio(96_000, ChannelLayout::Stereo, 160),
            audio(48_000, ChannelLayout::Surround51, 320),
            audio(48_000, ChannelLayout::Stereo, 16),
            audio(48_000, ChannelLayout::Stereo, 512),
            not_aac,
        ] {
            assert!(
                matches!(aac_settings(&c), Err(PlatformError::Unsupported { .. })),
                "{c:?}"
            );
        }
    }
}
