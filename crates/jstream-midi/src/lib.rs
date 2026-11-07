//! JStream's MIDI model for controller input
//! (docs/adr/0122-controller-input-midi-and-one-binding-system.md).
//!
//! Docs: crates/jstream-midi/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0122-controller-input-midi-and-one-binding-system.md
//!
//! Two parts, both free of I/O, allocation and locks, so a MIDI input thread can run them
//! on every event:
//!
//! - [`Message`]: a MIDI 1.0 channel, system common or real-time message, parsed from the
//!   bytes a transport delivers (JTunnel, a local MIDI port). SysEx is not a [`Message`]:
//!   transports carry it in their own framing, and [`message_len`] reports 0 for its
//!   status bytes.
//! - [`Tempo`]: the tempo and transport a MIDI clock gives. Clock ticks (24 per quarter
//!   note), start, stop, continue and song position, each stamped with the media-clock
//!   time it maps to (ADR-0014), become a BPM, a beat and a bar, which the engine reads
//!   at a render tick's time.
//!
//! ```
//! use jstream_midi::{Message, Tempo};
//!
//! let mut tempo = Tempo::new(4);
//! tempo.apply(&Message::parse(&[0xFA])?, 0);
//! // 120 BPM: a tick every 1/48 s.
//! for tick in 0..=48_u64 {
//!     tempo.apply(&Message::TimingClock, tick * 1_000_000_000 / 48);
//! }
//! let now = tempo.at(1_000_000_000);
//! assert!((now.bpm.unwrap_or(0.0) - 120.0).abs() < 0.01);
//! assert!((now.beat - 2.0).abs() < 1e-6);
//! # Ok::<(), jstream_midi::ParseError>(())
//! ```

#![forbid(unsafe_code)]

mod message;
mod tempo;

pub use message::{Channel, Message, ParseError, message_len};
pub use tempo::{TICKS_PER_BEAT, Tempo, TempoNow};
