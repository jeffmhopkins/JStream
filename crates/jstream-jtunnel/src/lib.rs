//! The JTunnel wire format and discovery records, for receiving a DAW's audio and MIDI
//! over the network (docs/adr/0120-controller-input-midi-and-one-binding-system.md).
//!
//! Docs: crates/jstream-jtunnel/README.md#purpose, docs/ARCHITECTURE.md#crate-map, docs/adr/0120-controller-input-midi-and-one-binding-system.md
//!
//! JTunnel is a CLAP plugin that sends a DAW track's audio and MIDI to other JTunnel
//! instances on the LAN, in small UDP datagrams, and finds them with mDNS. JStream
//! receives it natively, with no plugin. This crate is the format and nothing else: it
//! opens no socket and starts no thread, so the engine's receive thread, the tests and
//! the fuzzer all drive the same code.
//!
//! - [`wire`]: audio, subscribe and bye packets (version 2), their frames and events
//!   (MIDI messages as [`jstream_midi::Message`], SysEx pieces, note expressions), and
//!   24-bit PCM.
//! - [`SysexAssembler`]: whole SysEx messages from their pieces, all or nothing.
//! - [`SeqTracker`]: which frames are new, which are redundant copies, and how many never
//!   came.
//! - [`dnssd`]: the mDNS query that finds senders, the announcements they answer with,
//!   and the [`Directory`](dnssd::Directory) of senders seen.
//!
//! ```
//! use jstream_jtunnel::wire::{self, Packet};
//!
//! let mut buf = [0; 64];
//! let n = wire::write_subscribe(&mut buf, b"Studio").unwrap_or(0);
//! assert_eq!(wire::parse(&buf[..n]), Ok(Packet::Subscribe { name: b"Studio" }));
//! ```

#![forbid(unsafe_code)]

pub mod dnssd;
mod seq;
mod sysex;
pub mod wire;

pub use seq::{SEQ_WINDOW, SeqTracker, SeqVerdict};
pub use sysex::SysexAssembler;
