//! The JStream real-time audio mixer. Each input (a microphone, desktop loopback) is
//! downmixed to stereo and adaptively resampled from its device's rate and clock onto
//! the engine's time line on its own audio thread; the [`Mixer`] applies each input's
//! fader, sums the inputs through a send matrix into stereo buses (the master and up to
//! 15 more, each with its fader) in fixed-size blocks stamped on the engine clock, and
//! meters inputs and buses. Layer 2: no I/O, no platform code; inputs plug into the
//! `jstream-platform` audio handler.
//!
//! ```
//! use jstream_audio::{Mixer, MixerConfig};
//! use jstream_media::{AudioFormat, ChannelLayout, MediaTime, SampleFormat, Timebase};
//!
//! let mut mixer = Mixer::new(MixerConfig::default())?;
//! let mic = AudioFormat::new(44_100, ChannelLayout::Mono, SampleFormat::F32Interleaved)
//!     .expect("valid format");
//! // `producer` goes to `AudioIo::open_input` as the stream's handler.
//! let (producer, mic_control) = mixer.add_input(mic)?;
//! mic_control.set_gain_db(-6.0);
//! # drop(producer);
//! let mut block = vec![0.0; 2 * mixer.config().block_frames];
//! let now = MediaTime::new(1_000_000, Timebase::MICROS);
//! while let Some(b) = mixer.poll(now, &mut block)? {
//!     // Hand `block` (interleaved stereo at `b.pts`) to the encoder.
//!     # let _ = b;
//! }
//! # Ok::<(), jstream_audio::AudioError>(())
//! ```
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-audio/README.md#purpose, crates/jstream-audio/README.md#realtime, crates/jstream-audio/README.md#meters, crates/jstream-audio/README.md#filters, docs/adr/0028-audio-model-v2.md, docs/adr/0014-media-clock.md

#![forbid(unsafe_code)]

mod error;
pub mod filter;
mod handle;
pub mod meter;
mod rt;

pub use error::AudioError;
pub use handle::{BusControl, BusId, BusStats, InputControl, InputId, InputStats};
pub use rt::input::InputProducer;
pub use rt::mixer::{Block, Mixer, MixerConfig, MixerControl};
pub use rt::shared::MeterReading;
