//! JStream's AAC-LC encoder: safe Rust, encode only, for Linux's AAC path
//! (docs/adr/0061-source-types-and-decoding-scope-v2.md), within the limits of
//! docs/adr/0062-aac-lc-encoder-patent-check.md.
//!
//! Docs: crates/jstream-aac/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0061-source-types-and-decoding-scope-v2.md, docs/adr/0062-aac-lc-encoder-patent-check.md
//!
//! It produces the MPEG-4 AAC-LC object type (audio object type 2) with only tools of
//! the MPEG-2 AAC LC profile (ISO/IEC 13818-7, 1997): the MDCT filterbank with sine and
//! KBD windows, block switching, TNS, M/S and intensity stereo, scalefactors, quantization
//! and Huffman coding. It never emits PNS, SBR, parametric stereo or any later tool, and
//! there is no decoder. Each encoder technique names its pre-2003 source where it is
//! implemented: the psychoacoustic model and attack detector in `psy`, the quantization
//! loops in `quant`, rate control and M/S in `encoder`, TNS in `tns`, intensity stereo in
//! `intensity`, sectioning in `huffman`, the filterbank in `mdct`.
//!
//! JStream holds no AAC patent licence. Via Licensing Alliance (Via LA) administers the
//! AAC patent pool; ADR-0062 records what is and isn't known about the patents.
//!
//! [`Encoder`] takes 44.1 or 48 kHz mono or stereo `f32` samples and gives one raw access
//! unit (a `raw_data_block`, no ADTS header) per [`FRAME_LEN`] samples, with the
//! [`AudioSpecificConfig`](Encoder::audio_specific_config) a container carries ahead of
//! them. The decoded output lags the input by [`DELAY`] samples.
//!
//! ```
//! use jstream_aac::{Config, Encoder};
//!
//! let mut enc = Encoder::new(Config::new(48_000, 2, 128_000))?;
//! assert_eq!(enc.audio_specific_config(), [0x11, 0x90]);
//! let tone: Vec<f32> = (0..4800)
//!     .flat_map(|i| {
//!         let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
//!         [s, s]
//!     })
//!     .collect();
//! enc.push_interleaved(&tone)?;
//! enc.finish();
//! let mut units = Vec::new();
//! let mut au = Vec::new();
//! while enc.encode_frame(&mut au) {
//!     units.push(std::mem::take(&mut au));
//! }
//! assert_eq!(units.len(), 4800usize.div_ceil(1024) + 1);
//! # Ok::<(), jstream_aac::Error>(())
//! ```

#![forbid(unsafe_code)]

mod bits;
mod encoder;
mod huffman;
mod intensity;
mod mdct;
mod psy;
mod quant;
mod tables;
mod tns;

pub use encoder::Encoder;
pub use mdct::WindowShape;

/// Samples per channel in one access unit.
pub const FRAME_LEN: usize = 1024;

/// Samples per channel by which the decoded output lags the input: one frame, the
/// filterbank's overlap. The first access unit decodes to silence.
pub const DELAY: usize = FRAME_LEN;

/// Sample rates the encoder takes.
pub const SAMPLE_RATES: [u32; 2] = [44_100, 48_000];

/// The bit rates the encoder takes, in bits per second per channel.
pub const BIT_RATES_PER_CHANNEL: std::ops::RangeInclusive<u32> = 16_000..=256_000;

/// What to encode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// 44 100 or 48 000 Hz.
    pub sample_rate: u32,
    /// 1 (mono) or 2 (stereo).
    pub channels: u8,
    /// The target bit rate in bits per second, for all channels together: 16 to 256 kbps
    /// per channel. The encoder never exceeds it on average; on simple signals it spends
    /// less.
    pub bit_rate: u32,
    /// The window shape of every frame.
    pub window_shape: WindowShape,
    /// Whether windows whose spectrum predicts well across frequency are coded with
    /// temporal noise shaping, so their quantization noise follows the signal's envelope
    /// in time. On by default.
    pub tns: bool,
    /// Whether a stereo stream codes upper bands whose channels differ mainly in level
    /// once, as intensity stereo, where the error is masked. Only at 64 kbps per channel
    /// and below, from 6 kHz up at 32 kbps and from 10 kHz up at 64 kbps. On by default.
    pub intensity: bool,
}

impl Config {
    /// A configuration with the sine window, temporal noise shaping and intensity stereo.
    pub fn new(sample_rate: u32, channels: u8, bit_rate: u32) -> Self {
        Self {
            sample_rate,
            channels,
            bit_rate,
            window_shape: WindowShape::Sine,
            tns: true,
            intensity: true,
        }
    }

    fn check(&self) -> Result<(), Error> {
        if !SAMPLE_RATES.contains(&self.sample_rate) {
            return Err(Error::SampleRate(self.sample_rate));
        }
        if !(1..=2).contains(&self.channels) {
            return Err(Error::Channels(self.channels));
        }
        let per_channel = self.bit_rate / u32::from(self.channels);
        if !BIT_RATES_PER_CHANNEL.contains(&per_channel) {
            return Err(Error::BitRate {
                bit_rate: self.bit_rate,
                channels: self.channels,
            });
        }
        Ok(())
    }
}

/// Why the encoder refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The sample rate isn't 44.1 or 48 kHz.
    #[error("AAC-LC encoding at {0} Hz is not supported (44100 or 48000 only)")]
    SampleRate(u32),
    /// The channel count isn't 1 or 2.
    #[error("AAC-LC encoding of {0} channels is not supported (mono or stereo only)")]
    Channels(u8),
    /// The bit rate is outside 16 to 256 kbps per channel.
    #[error(
        "AAC-LC encoding at {bit_rate} b/s for {channels} channels is not supported (16 to 256 kbps per channel)"
    )]
    BitRate {
        /// The bit rate asked for.
        bit_rate: u32,
        /// The channel count.
        channels: u8,
    },
    /// Samples came after [`Encoder::finish`].
    #[error("the AAC encoder is finished and takes no more samples")]
    Finished,
    /// A buffer that isn't a whole number of sample frames.
    #[error("{samples} samples is not a whole number of {channels}-channel frames")]
    PartialFrame {
        /// The samples given.
        samples: usize,
        /// The channel count.
        channels: u8,
    },
}

#[cfg(test)]
mod tests;
