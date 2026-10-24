//! Codecs on macOS: VideoToolbox for video, AudioToolbox for AAC (ADR-0063 §6, §7).
//!
//! The platform-independent part turns the engine's encoder and decoder configurations
//! into the settings a `VTCompressionSession`, a `VTDecompressionSession` and an AAC
//! `AudioConverter` are opened with, and refuses what they can't do. On macOS,
//! `VideoToolboxCodecs` lists VideoToolbox's H.264 and HEVC encoders and opens their
//! sessions (`crate::videotoolbox`), opens H.264 and HEVC decoders on CPU frames
//! (`crate::vtdecoder`), and lists and opens AudioToolbox's AAC-LC converter
//! (`crate::audiotoolbox`).

use jstream_platform::media::{ChannelLayout, Codec, PixelFormat};
use jstream_platform::{
    AudioEncoderConfig, CodecInfo, CodecRole, HardwarePreference, PlatformError, RateControl,
    VideoDecoderConfig, VideoEncoderConfig,
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

/// What a `VTDecompressionSession` is opened with for one [`VideoDecoderConfig`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtDecoderSettings {
    /// The Core Media codec type ([`CM_H264`] or [`CM_HEVC`]).
    pub codec_type: u32,
    /// The Core Video pixel format the decoded frames are delivered in.
    pub pixel_format: u32,
    /// `kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder`.
    pub enable_hardware: bool,
    /// `kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder`.
    pub require_hardware: bool,
}

/// The VideoToolbox settings for `config`: H.264 or HEVC, decoded to NV12 or BGRA (and
/// P010 from HEVC, for 10-bit streams), the formats the compositor takes.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a codec VideoToolbox doesn't decode here (AV1,
/// audio codecs) or an output format it isn't asked for (I420, RGBA, P010 from H.264).
pub fn vt_decoder_settings(
    config: &VideoDecoderConfig,
) -> Result<VtDecoderSettings, PlatformError> {
    let codec_type = match config.codec {
        Codec::H264 => CM_H264,
        Codec::Hevc => CM_HEVC,
        other => {
            return Err(PlatformError::Unsupported {
                what: format!("{other:?} decoding on macOS (VideoToolbox: H.264 and HEVC)"),
            });
        }
    };
    let output_ok = match config.output_format {
        PixelFormat::Nv12 | PixelFormat::Bgra8 => true,
        PixelFormat::P010 => config.codec == Codec::Hevc,
        _ => false,
    };
    let pixel_format = cv_pixel_format(config.output_format)
        .filter(|_| output_ok)
        .ok_or_else(|| PlatformError::Unsupported {
            what: format!(
                "{:?} output from a {:?} decoder on macOS",
                config.output_format, config.codec
            ),
        })?;
    let (enable_hardware, require_hardware) = match config.hardware {
        HardwarePreference::Prefer => (true, false),
        HardwarePreference::Require => (true, true),
        HardwarePreference::Avoid => (false, false),
    };
    Ok(VtDecoderSettings {
        codec_type,
        pixel_format,
        enable_hardware,
        require_hardware,
    })
}

/// `kVTCouldNotFindVideoDecoderErr`: no decoder matches the specification.
const VT_NO_DECODER: i32 = -12906;
/// `kVTVideoDecoderNotAvailableNowErr`: the decoder exists but can't be had now.
const VT_DECODER_NOT_AVAILABLE_NOW: i32 = -12913;

/// The name a VideoToolbox decoder reports, in [`vt_decoder_list`] and once opened:
/// `VideoToolbox H.264 decoder (hardware)`, or `(software)` for Apple's software one.
pub fn vt_decoder_name(codec: Codec, hardware: bool) -> String {
    let codec = match codec {
        Codec::Hevc => "HEVC",
        _ => "H.264",
    };
    let kind = if hardware { "hardware" } else { "software" };
    format!("VideoToolbox {codec} decoder ({kind})")
}

/// The decoders `Codecs::list` reports on macOS: for H.264 and HEVC, a hardware one
/// when `hardware_supported` (`VTIsHardwareDecodeSupported`) says so for its Core Media
/// codec type, and Apple's software one, which is always there.
pub fn vt_decoder_list(hardware_supported: impl Fn(u32) -> bool) -> Vec<CodecInfo> {
    let mut list = Vec::new();
    for (codec, codec_type) in [(Codec::H264, CM_H264), (Codec::Hevc, CM_HEVC)] {
        let kinds: &[bool] = if hardware_supported(codec_type) {
            &[true, false]
        } else {
            &[false]
        };
        for &hardware in kinds {
            list.push(CodecInfo {
                name: vt_decoder_name(codec, hardware),
                codec,
                role: CodecRole::VideoDecoder,
                hardware,
            });
        }
    }
    list
}

/// Whether a decoder for `settings` will use hardware, as far as is known before its
/// session opens, given `VTIsHardwareDecodeSupported` for its codec type (`supported`).
/// The open session says for certain.
///
/// # Errors
/// [`PlatformError::Unsupported`] when the preference requires hardware and this Mac
/// has no hardware decoder for the codec, so `HardwarePreference::Require` fails when
/// the decoder is made, not at its first keyframe.
pub fn vt_decoder_hardware(
    settings: &VtDecoderSettings,
    codec: Codec,
    supported: bool,
) -> Result<bool, PlatformError> {
    if settings.require_hardware && !supported {
        let name = vt_decoder_name(codec, true);
        return Err(PlatformError::Unsupported {
            what: format!("a {name} on this Mac (VTIsHardwareDecodeSupported is false)"),
        });
    }
    Ok(settings.enable_hardware && supported)
}

/// Whether a `VTDecompressionSessionCreate` status means no decoder matched the
/// specification, which under `HardwarePreference::Require` is no hardware decoder for
/// this stream (its profile or size, say). Any other status is the call failing, such as
/// on a bad parameter set, and is reported as that.
pub fn vt_no_decoder(status: i32) -> bool {
    matches!(status, VT_NO_DECODER | VT_DECODER_NOT_AVAILABLE_NOW)
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

    use super::vt_decoder_list;
    use crate::audiotoolbox::{AtAacEncoder, aac_encoder_info};
    use crate::videotoolbox::{VtEncoder, list_encoders};
    use crate::vtdecoder::{VtDecoder, hardware_decode_supported};

    /// Codecs through VideoToolbox and AudioToolbox (ADR-0063 §6, §7). It lists and opens
    /// VideoToolbox's H.264 and HEVC encoders, which take CPU frames for now, opens its
    /// H.264 and HEVC decoders, which deliver CPU frames, and lists and opens
    /// AudioToolbox's AAC-LC encoder. VideoToolbox has no list of decoders, so the list
    /// has the H.264 and HEVC ones: a hardware one where `VTIsHardwareDecodeSupported`
    /// says there is one, and Apple's software one ([`vt_decoder_list`]).
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
            let mut list = list_encoders()?;
            list.extend(vt_decoder_list(hardware_decode_supported));
            list.push(aac_encoder_info());
            Ok(list)
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
            Ok(Box::new(AtAacEncoder::open(config)?))
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Ok(Box::new(VtDecoder::open(config)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        AudioFormat, ChannelLayout, Codec, FrameSize, PixelFormat, SampleFormat, Timebase,
    };
    use jstream_platform::{
        AudioEncoderConfig, HardwarePreference, PlatformError, RateControl, VideoDecoderConfig,
        VideoEncoderConfig,
    };

    use super::{
        AUDIO_AAC, CM_H264, CM_HEVC, VtBitRate, aac_settings, vt_decoder_hardware, vt_decoder_list,
        vt_decoder_settings, vt_encoder_settings, vt_no_decoder,
    };
    use crate::fourcc::{CV_420V, CV_BGRA, CV_X420};

    /// The statuses `vt_no_decoder` matches are VideoToolbox's own (#657 review N2).
    #[cfg(target_os = "macos")]
    #[test]
    fn no_decoder_statuses_match_videotoolbox() {
        use objc2_video_toolbox::{
            kVTCouldNotFindVideoDecoderErr, kVTVideoDecoderNotAvailableNowErr,
        };
        assert_eq!(super::VT_NO_DECODER, kVTCouldNotFindVideoDecoderErr);
        assert_eq!(
            super::VT_DECODER_NOT_AVAILABLE_NOW,
            kVTVideoDecoderNotAvailableNowErr
        );
    }

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

    fn decoder(
        codec: Codec,
        output_format: PixelFormat,
        hardware: HardwarePreference,
    ) -> VideoDecoderConfig {
        VideoDecoderConfig {
            codec,
            output_format,
            hardware,
        }
    }

    #[test]
    fn decoders_map_to_a_decompression_session() {
        let s = vt_decoder_settings(&decoder(
            Codec::H264,
            PixelFormat::Nv12,
            HardwarePreference::Prefer,
        ))
        .expect("settings");
        assert_eq!((s.codec_type, s.pixel_format), (CM_H264, CV_420V));
        assert!(s.enable_hardware && !s.require_hardware);
        let s = vt_decoder_settings(&decoder(
            Codec::Hevc,
            PixelFormat::P010,
            HardwarePreference::Require,
        ))
        .expect("settings");
        assert_eq!((s.codec_type, s.pixel_format), (CM_HEVC, CV_X420));
        assert!(s.enable_hardware && s.require_hardware);
        let s = vt_decoder_settings(&decoder(
            Codec::H264,
            PixelFormat::Bgra8,
            HardwarePreference::Avoid,
        ))
        .expect("settings");
        assert_eq!(s.pixel_format, CV_BGRA);
        assert!(!s.enable_hardware && !s.require_hardware);
        for (codec, format) in [
            (Codec::Av1, PixelFormat::Nv12),
            (Codec::Aac, PixelFormat::Nv12),
            (Codec::H264, PixelFormat::P010),
            (Codec::H264, PixelFormat::I420),
            (Codec::Hevc, PixelFormat::Rgba8),
        ] {
            assert!(
                matches!(
                    vt_decoder_settings(&decoder(codec, format, HardwarePreference::Prefer)),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{codec:?} to {format:?}"
            );
        }
    }

    #[test]
    fn decoders_are_listed_by_what_has_hardware() {
        let names = |list: Vec<jstream_platform::CodecInfo>| {
            list.into_iter()
                .map(|info| {
                    assert_eq!(info.role, jstream_platform::CodecRole::VideoDecoder);
                    assert_eq!(info.name.contains("(hardware)"), info.hardware);
                    (info.codec, info.hardware)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(vt_decoder_list(|_| true)),
            [
                (Codec::H264, true),
                (Codec::H264, false),
                (Codec::Hevc, true),
                (Codec::Hevc, false)
            ]
        );
        assert_eq!(
            names(vt_decoder_list(|t| t == CM_H264)),
            [
                (Codec::H264, true),
                (Codec::H264, false),
                (Codec::Hevc, false)
            ]
        );
        assert_eq!(
            names(vt_decoder_list(|_| false)),
            [(Codec::H264, false), (Codec::Hevc, false)]
        );
    }

    #[test]
    fn a_required_hardware_decoder_is_checked_when_made() {
        let settings = |hardware| {
            vt_decoder_settings(&decoder(Codec::Hevc, PixelFormat::Nv12, hardware))
                .expect("settings")
        };
        let require = settings(HardwarePreference::Require);
        assert!(matches!(
            vt_decoder_hardware(&require, Codec::Hevc, false),
            Err(PlatformError::Unsupported { what }) if what.contains("HEVC decoder (hardware)")
        ));
        assert!(vt_decoder_hardware(&require, Codec::Hevc, true).expect("hardware"));
        let prefer = settings(HardwarePreference::Prefer);
        assert!(vt_decoder_hardware(&prefer, Codec::Hevc, true).expect("prefer"));
        assert!(!vt_decoder_hardware(&prefer, Codec::Hevc, false).expect("software"));
        let avoid = settings(HardwarePreference::Avoid);
        assert!(!vt_decoder_hardware(&avoid, Codec::Hevc, true).expect("avoid"));
        // Only a missing decoder is "no hardware decoder"; a bad parameter set is not.
        assert!(vt_no_decoder(-12906) && vt_no_decoder(-12913));
        assert!(!vt_no_decoder(-12909) && !vt_no_decoder(0));
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
