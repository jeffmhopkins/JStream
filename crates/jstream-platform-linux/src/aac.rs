//! AAC on Linux: JStream's own AAC-LC encoder (`jstream-aac`) behind the
//! `AudioEncoder` trait (docs/adr/0061-source-types-and-decoding-scope-v2.md, within
//! docs/adr/0062-aac-lc-encoder-patent-check.md's limits).
//!
//! It is built only with the `aac-encoder` feature, on by default. ADR-0062's re-check
//! clause switches the encoder off by dropping that feature from the defaults: the
//! dependency then leaves the build, and `VaapiCodecs` reports AAC as `Unsupported`.
//!
//! Like the Media Foundation encoder, it emits one raw access unit (1024 frames, no ADTS)
//! per packet, and counts packet times from the first buffer's `pts`. The muxers write
//! the `AudioSpecificConfig` from the format (`jstream-mux`'s `aac_lc`), which is the
//! one the encoder signals. The decoder's output lags by `jstream_aac::DELAY` frames, the
//! usual AAC priming, as with the operating systems' encoders.

use jstream_aac::{Config, Encoder, FRAME_LEN};
use jstream_platform::media::{
    AudioBuffer, AudioFormat, ChannelLayout, Codec, MediaTime, Packet, Rounding, SampleFormat,
};
use jstream_platform::{AudioEncoder, AudioEncoderConfig, CodecInfo, CodecRole, PlatformError};

/// The name the encoder lists under.
pub const AAC_ENCODER_NAME: &str = "JStream AAC-LC (Rust)";

/// The codec list entry for the encoder.
pub fn aac_encoder_info() -> CodecInfo {
    CodecInfo {
        name: AAC_ENCODER_NAME.into(),
        codec: Codec::Aac,
        role: CodecRole::AudioEncoder,
        hardware: false,
    }
}

/// The encoder's configuration for `config`.
///
/// # Errors
/// [`PlatformError::Unsupported`] for another codec, a sample format other than `f32`
/// (interleaved or planar), a sample rate other than 44.1 or
/// 48 kHz, a layout other than mono or stereo, or a bit rate outside 16 to 256 kbps per
/// channel.
pub fn aac_config(config: &AudioEncoderConfig) -> Result<Config, PlatformError> {
    let unsupported = |what: String| Err(PlatformError::Unsupported { what });
    if config.codec != Codec::Aac {
        return unsupported(format!("{:?} from the AAC encoder", config.codec));
    }
    let f = config.input_format;
    if !matches!(
        f.sample_format(),
        SampleFormat::F32Interleaved | SampleFormat::F32Planar
    ) {
        return unsupported(format!("AAC input in {:?}", f.sample_format()));
    }
    let channels = match f.layout() {
        ChannelLayout::Mono => 1,
        ChannelLayout::Stereo => 2,
        other => return unsupported(format!("AAC in {other:?} on Linux (mono or stereo)")),
    };
    let enc = Config::new(f.sample_rate(), channels, config.kbps.saturating_mul(1000));
    // Opening checks the rest; the error says what is out of range.
    match Encoder::new(enc) {
        Ok(_) => Ok(enc),
        Err(e) => unsupported(e.to_string()),
    }
}

/// One AAC encoder session.
pub struct RustAacEncoder {
    encoder: Encoder,
    info: CodecInfo,
    format: AudioFormat,
    /// The first buffer's `pts`, in the input's sample time base.
    start: Option<MediaTime>,
    /// Access units given a `pts` so far.
    emitted: u64,
    unit: Vec<u8>,
    pending: Vec<Packet>,
    finished: bool,
}

impl RustAacEncoder {
    /// Opens a session for `config`.
    ///
    /// # Errors
    /// As [`aac_config`].
    pub fn open(config: &AudioEncoderConfig) -> Result<Self, PlatformError> {
        let enc = aac_config(config)?;
        let encoder = Encoder::new(enc).map_err(|e| PlatformError::Unsupported {
            what: e.to_string(),
        })?;
        Ok(Self {
            encoder,
            info: aac_encoder_info(),
            format: config.input_format,
            start: None,
            emitted: 0,
            unit: Vec::with_capacity(768 * usize::from(enc.channels)),
            pending: Vec::new(),
            finished: false,
        })
    }

    /// Moves every access unit the encoder has ready into `pending`.
    fn collect(&mut self) -> Result<(), PlatformError> {
        while self.encoder.encode_frame(&mut self.unit) {
            let Some(start) = self.start else {
                return Err(PlatformError::InvalidState {
                    what: "AAC packet timing",
                    state: "the encoder produced output before any input",
                });
            };
            let offset = self
                .emitted
                .checked_mul(FRAME_LEN as u64)
                .and_then(|o| i64::try_from(o).ok())
                .ok_or(PlatformError::InvalidState {
                    what: "AAC packet timing",
                    state: "the stream is too long",
                })?;
            let pts = start.checked_add(MediaTime::new(offset, start.timebase()))?;
            self.emitted += 1;
            self.pending.push(Packet::new(
                Codec::Aac,
                true,
                pts,
                pts,
                self.unit.as_slice(),
            )?);
            self.unit.clear();
        }
        Ok(())
    }
}

impl AudioEncoder for RustAacEncoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    fn encode(&mut self, buffer: &AudioBuffer) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "AAC encode",
                state: "the encoder is finished",
            });
        }
        if buffer.format() != self.format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "audio in {:?} (configured for {:?})",
                    buffer.format(),
                    self.format
                ),
            });
        }
        if buffer.frames() == 0 {
            return Ok(());
        }
        if self.start.is_none() {
            self.start = Some(
                buffer
                    .pts()
                    .rescale(self.format.timebase(), Rounding::Nearest)?,
            );
        }
        let pushed = if self.format.sample_format().is_planar() {
            self.encoder.push_planar(buffer.samples())
        } else {
            self.encoder.push_interleaved(buffer.samples())
        };
        pushed.map_err(|e| PlatformError::InvalidState {
            what: "AAC encode",
            state: match e {
                jstream_aac::Error::Finished => "the encoder is finished",
                _ => "the buffer isn't whole frames",
            },
        })?;
        self.collect()
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        out.append(&mut self.pending);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        if !self.finished {
            self.finished = true;
            self.encoder.finish();
            self.collect()?;
        }
        self.poll(out)
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{
        AudioBuffer, AudioFormat, ChannelLayout, Codec, MediaTime, SampleFormat, Timebase,
    };
    use jstream_platform::{AudioEncoder, AudioEncoderConfig, PlatformError};

    use super::{AAC_ENCODER_NAME, RustAacEncoder, aac_config};

    fn config(
        rate: u32,
        layout: ChannelLayout,
        format: SampleFormat,
        kbps: u32,
    ) -> AudioEncoderConfig {
        AudioEncoderConfig {
            codec: Codec::Aac,
            input_format: AudioFormat::new(rate, layout, format).expect("valid format"),
            kbps,
        }
    }

    #[test]
    fn accepts_streaming_configurations_and_refuses_others() {
        for kbps in [96, 128, 160, 192, 256, 320] {
            assert!(
                aac_config(&config(
                    48_000,
                    ChannelLayout::Stereo,
                    SampleFormat::F32Interleaved,
                    kbps
                ))
                .is_ok(),
                "{kbps}"
            );
        }
        assert!(
            aac_config(&config(
                44_100,
                ChannelLayout::Mono,
                SampleFormat::F32Planar,
                64
            ))
            .is_ok()
        );
        for bad in [
            config(
                32_000,
                ChannelLayout::Stereo,
                SampleFormat::F32Interleaved,
                128,
            ),
            config(
                48_000,
                ChannelLayout::Surround51,
                SampleFormat::F32Interleaved,
                384,
            ),
            config(
                48_000,
                ChannelLayout::Stereo,
                SampleFormat::F32Interleaved,
                8,
            ),
            config(
                48_000,
                ChannelLayout::Stereo,
                SampleFormat::F32Interleaved,
                4000,
            ),
            AudioEncoderConfig {
                codec: Codec::H264,
                ..config(
                    48_000,
                    ChannelLayout::Stereo,
                    SampleFormat::F32Interleaved,
                    128,
                )
            },
        ] {
            assert!(matches!(
                aac_config(&bad),
                Err(PlatformError::Unsupported { .. })
            ));
        }
    }

    /// Packets are raw access units, one per 1024 frames, timed from the first buffer:
    /// the 960-frame buffers a 20 ms mixer tick gives come out as 1024-frame packets.
    #[test]
    fn packets_are_timed_from_the_first_buffer() {
        let cfg = config(
            48_000,
            ChannelLayout::Stereo,
            SampleFormat::F32Interleaved,
            128,
        );
        let mut enc = RustAacEncoder::open(&cfg).expect("opens");
        assert_eq!(enc.info().name, AAC_ENCODER_NAME);
        let fmt = cfg.input_format;
        let mut out = Vec::new();
        // Start at 1 s on a nanosecond clock.
        let start = MediaTime::new(1_000_000_000, Timebase::NANOS);
        for tick in 0..10u32 {
            let pts = MediaTime::new(
                1_000_000_000 + i64::from(tick) * 20_000_000,
                Timebase::NANOS,
            );
            let samples: Vec<f32> = (0..960 * 2)
                .map(|i| ((i + tick as usize * 1920) as f32 * 0.01).sin() * 0.3)
                .collect();
            let buf = AudioBuffer::new(fmt, pts, samples).expect("buffer");
            enc.encode(&buf).expect("encodes");
            enc.poll(&mut out).expect("polls");
        }
        enc.finish(&mut out).expect("finishes");
        // 9600 frames: 10 units of 1024, and one more for the priming delay.
        assert_eq!(out.len(), 9600usize.div_ceil(1024) + 1);
        let tb = fmt.timebase();
        let first = start
            .rescale(tb, jstream_platform::media::Rounding::Nearest)
            .expect("rescale");
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.codec(), Codec::Aac);
            assert!(p.is_keyframe());
            assert_eq!(p.pts().ticks(), first.ticks() + i as i64 * 1024);
            assert_eq!(p.pts(), p.dts());
            assert!(!p.data().is_empty() && p.data().len() <= 768 * 2);
            // A raw CPE, not an ADTS header (0xFFF syncword).
            assert_eq!(p.data()[0] >> 5, 1);
        }
        // Finished: no more input.
        let buf = AudioBuffer::silence(fmt, start, 10).expect("buffer");
        assert!(matches!(
            enc.encode(&buf),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    #[test]
    fn refuses_buffers_of_another_format() {
        let cfg = config(
            48_000,
            ChannelLayout::Stereo,
            SampleFormat::F32Interleaved,
            128,
        );
        let mut enc = RustAacEncoder::open(&cfg).expect("opens");
        let other = AudioFormat::new(44_100, ChannelLayout::Stereo, SampleFormat::F32Interleaved)
            .expect("format");
        let buf = AudioBuffer::silence(other, MediaTime::zero(Timebase::NANOS), 100).expect("buf");
        assert!(matches!(
            enc.encode(&buf),
            Err(PlatformError::Unsupported { .. })
        ));
    }
}
