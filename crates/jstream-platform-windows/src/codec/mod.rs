//! Media Foundation codecs: the [`Codecs`](jstream_platform::Codecs) backend.
//!
//! It has the AAC encoder (`aac`) and the H.264 encoder from CPU NV12 frames (`h264`).
//! Video decoders follow; until then they report `Unsupported`.

// Its platform-independent parts are tested on every platform.
#[cfg(any(windows, test))]
mod aac;
#[cfg(any(windows, test))]
mod h264;
#[cfg(windows)]
mod mf;

#[cfg(windows)]
pub use backend::MfCodecs;

#[cfg(windows)]
mod backend {
    use jstream_platform::media::Codec;
    use jstream_platform::{
        AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, Codecs, PlatformError,
        VideoDecoder, VideoDecoderConfig, VideoEncoder, VideoEncoderConfig,
    };
    use windows::Win32::Media::MediaFoundation::{
        MFAudioFormat_AAC, MFAudioFormat_PCM, MFMediaType_Audio, MFMediaType_Video,
        MFT_CATEGORY_AUDIO_ENCODER, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
        MFVideoFormat_H264, MFVideoFormat_NV12,
    };

    use super::aac::MfAacEncoder;
    use super::h264::MfH264Encoder;
    use super::mf::{self, MfRuntime};

    /// Codecs through Media Foundation, the OS codecs ADR-0003 chooses.
    ///
    /// It holds no COM objects, so it is `Send + Sync`. Each session starts Media
    /// Foundation for itself and can be driven from any thread.
    #[derive(Debug, Default)]
    pub struct MfCodecs {
        _private: (),
    }

    impl MfCodecs {
        /// The backend. Opening nothing yet, it can't fail.
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl Codecs for MfCodecs {
        fn list(&self) -> Result<Vec<CodecInfo>, PlatformError> {
            let _rt = MfRuntime::start()?;
            let sync = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER;
            let info = |found: mf::Found, codec, role, hardware| CodecInfo {
                name: found.name,
                codec,
                role,
                hardware,
            };
            let mut out = Vec::new();
            let aac = mf::enumerate(
                MFT_CATEGORY_AUDIO_ENCODER,
                sync,
                (MFMediaType_Audio, MFAudioFormat_PCM),
                (MFMediaType_Audio, MFAudioFormat_AAC),
            )?;
            // Only the first is ever opened, so only it is listed.
            out.extend(
                aac.into_iter()
                    .take(1)
                    .map(|f| info(f, Codec::Aac, CodecRole::AudioEncoder, false)),
            );
            let h264 = (
                (MFMediaType_Video, MFVideoFormat_NV12),
                (MFMediaType_Video, MFVideoFormat_H264),
            );
            for (flags, hardware) in [
                (MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, true),
                (sync, false),
            ] {
                out.extend(
                    mf::enumerate(MFT_CATEGORY_VIDEO_ENCODER, flags, h264.0, h264.1)?
                        .into_iter()
                        .map(|f| info(f, Codec::H264, CodecRole::VideoEncoder, hardware)),
                );
            }
            Ok(out)
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            Ok(Box::new(MfH264Encoder::open(config)?))
        }

        fn audio_encoder(
            &self,
            config: &AudioEncoderConfig,
        ) -> Result<Box<dyn AudioEncoder>, PlatformError> {
            Ok(Box::new(MfAacEncoder::open(config)?))
        }

        fn video_decoder(
            &self,
            config: &VideoDecoderConfig,
        ) -> Result<Box<dyn VideoDecoder>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: format!("{:?} video decoding (not implemented yet)", config.codec),
            })
        }
    }
}

/// Platform tests against the OS encoders (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because a CI runner may lack Media Foundation or a GPU encoder. Run them on a Windows
/// machine: `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
#[cfg(all(test, windows, feature = "hw-tests"))]
mod device_tests {
    use jstream_platform::media::{
        AudioBuffer, AudioFormat, ChannelLayout, Codec, MediaTime, SampleFormat, Timebase,
    };
    use jstream_platform::{AudioEncoderConfig, Codecs};

    use super::MfCodecs;

    #[test]
    #[ignore = "needs Media Foundation's AAC encoder"]
    fn encodes_a_second_of_tone_to_aac_access_units() {
        let codecs = MfCodecs::new();
        assert!(!codecs.list().expect("list").is_empty());
        let format = AudioFormat::new(48_000, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let mut enc = codecs
            .audio_encoder(&AudioEncoderConfig {
                codec: Codec::Aac,
                input_format: format,
                kbps: 128,
            })
            .expect("open AAC encoder");
        // One second as 100 buffers of 480 frames, starting at 2 s on a 100 ns clock.
        let mut out = Vec::new();
        let mut phase = 0.0f32;
        for i in 0..100i64 {
            let mut samples = Vec::with_capacity(960);
            for _ in 0..480 {
                let v = 0.25 * (phase * std::f32::consts::TAU).sin();
                samples.extend([v, v]);
                phase = (phase + 440.0 / 48_000.0).fract();
            }
            let pts = MediaTime::new(
                20_000_000 + i * 100_000,
                Timebase::new(1, 10_000_000).expect("tb"),
            );
            let buf = AudioBuffer::new(format, pts, samples).expect("buffer");
            enc.encode(&buf).expect("encode");
            enc.poll(&mut out).expect("poll");
        }
        enc.finish(&mut out).expect("finish");
        // 48000 frames is 46.875 access units, so 47 once padded; the MFT may hold back
        // or add a priming unit, so allow one either way.
        assert!((46..=48).contains(&out.len()), "{} packets", out.len());
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.codec(), Codec::Aac);
            assert!(!p.data().is_empty());
            assert_eq!(p.pts().ticks(), 96_000 + 1024 * i as i64, "packet {i}");
            assert_eq!(p.pts().timebase(), format.timebase());
        }
        assert!(
            enc.encode(
                &AudioBuffer::silence(format, MediaTime::zero(format.timebase()), 10).expect("buf")
            )
            .is_err()
        );
    }

    fn encode_two_seconds(hardware: jstream_platform::HardwarePreference) {
        use jstream_platform::media::{
            ColorInfo, FrameSize, PixelFormat, VideoFrame, VideoFrameInfo,
        };
        use jstream_platform::{RateControl, VideoEncoderConfig, VideoSurface};

        let codecs = MfCodecs::new();
        let size = FrameSize::new(1280, 720).expect("size");
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec: Codec::H264,
                size,
                input_format: PixelFormat::Nv12,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control: RateControl::Cbr { kbps: 3000 },
                keyframe_interval: 30,
                hardware,
            })
            .expect("open H.264 encoder");
        assert_eq!(
            enc.info().hardware,
            hardware == jstream_platform::HardwarePreference::Require
        );
        let tb = Timebase::MPEG_90K;
        let mut out = Vec::new();
        let mut sent = 0;
        for i in 0..60i64 {
            let info = VideoFrameInfo {
                size,
                format: PixelFormat::Nv12,
                color: ColorInfo::default(),
                pts: MediaTime::new(i * 3000, tb),
            };
            let mut frame = VideoFrame::zeroed(info).expect("frame");
            // A moving gradient, so the encoder has something to code.
            for y in 0..720u32 {
                if let Some(row) = frame.row_mut(0, y) {
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = (x as i64 + y as i64 + i * 4) as u8;
                    }
                }
            }
            for y in 0..360u32 {
                if let Some(row) = frame.row_mut(1, y) {
                    row.fill(128);
                }
            }
            // An asynchronous encoder may not want input yet: poll until it does.
            for _ in 0..1000 {
                enc.poll(&mut out).expect("poll");
                if enc.can_accept() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            enc.encode(VideoSurface::Cpu(frame)).expect("encode");
            sent += 1;
            enc.poll(&mut out).expect("poll");
        }
        enc.finish(&mut out).expect("finish");
        assert!(
            out.len() >= sent - 2,
            "{} packets for {sent} frames",
            out.len()
        );
        assert!(out[0].is_keyframe(), "the first packet is a keyframe");
        assert_eq!(out[0].pts(), MediaTime::new(0, tb));
        for w in out.windows(2) {
            assert!(w[0].pts().ticks() < w[1].pts().ticks(), "pts increase");
        }
        for p in &out {
            assert_eq!(p.codec(), Codec::H264);
            assert_eq!(p.pts(), p.dts());
            // Annex B: a start code first.
            assert!(p.data().starts_with(&[0, 0, 1]) || p.data().starts_with(&[0, 0, 0, 1]));
        }
        assert!(
            out.iter().filter(|p| p.is_keyframe()).count() >= 2,
            "keyframe every 30"
        );
    }

    #[test]
    #[ignore = "needs Media Foundation's software H.264 encoder"]
    fn encodes_h264_in_software() {
        encode_two_seconds(jstream_platform::HardwarePreference::Avoid);
    }

    #[test]
    #[ignore = "needs a GPU with a hardware H.264 encoder"]
    fn encodes_h264_in_hardware() {
        encode_two_seconds(jstream_platform::HardwarePreference::Require);
    }
}
