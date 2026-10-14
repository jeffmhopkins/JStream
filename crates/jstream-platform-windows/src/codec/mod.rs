//! Media Foundation codecs: the [`Codecs`](jstream_platform::Codecs) backend.
//!
//! This first cut has the AAC encoder (`aac`). Video encoders and decoders follow; until
//! then they report `Unsupported`.

// Its platform-independent parts are tested on every platform.
#[cfg(any(windows, test))]
mod aac;
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
        MFAudioFormat_AAC, MFAudioFormat_PCM, MFMediaType_Audio, MFT_CATEGORY_AUDIO_ENCODER,
        MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    };

    use super::aac::MfAacEncoder;
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
            let aac = mf::enumerate(
                MFT_CATEGORY_AUDIO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                (MFMediaType_Audio, MFAudioFormat_PCM),
                (MFMediaType_Audio, MFAudioFormat_AAC),
            )?;
            Ok(aac
                .into_iter()
                .take(1)
                .map(|found| CodecInfo {
                    name: found.name,
                    codec: Codec::Aac,
                    role: CodecRole::AudioEncoder,
                    hardware: false,
                })
                .collect())
        }

        fn video_encoder(
            &self,
            config: &VideoEncoderConfig,
        ) -> Result<Box<dyn VideoEncoder>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: format!("{:?} video encoding (not implemented yet)", config.codec),
            })
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

/// Platform tests against the OS AAC encoder (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because a CI runner may lack Media Foundation. Run them on a Windows
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
}
