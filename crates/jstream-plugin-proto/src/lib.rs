//! The cross-process contract between the engine's mixer and the out-of-process CLAP
//! plugin host (`jstream-plugin-host`, ADR-0035): the shared-memory block one CLAP filter
//! instance uses, made of a status block, an input and an output audio ring, and a
//! parameter-event ring each way. Layer 0: no dependencies, no `unsafe`, no OS bindings,
//! so the mixer, the host and the tests share one definition and build on any host.
//!
//! **Audio hand-off per block `n`** (0-based, counted by the mixer from the filter's
//! first block): the mixer writes its dry block `n` into [`Shared::input`] and, in the
//! same block, reads the processed block `n − depth` from [`Shared::output`]. It never
//! waits: when that block isn't there it passes its own delayed dry signal instead
//! and counts a miss in the status block. The host, woken by its own timer, reads each
//! new input block, runs the plugin over it and writes the result under the same block
//! number. [`first_useful`] tells it which blocks the mixer can still read, so a host
//! that fell behind skips ahead instead of producing blocks nobody reads.
//!
//! **Tearing.** Every field is atomic, so either side can read any slot at any time
//! without undefined behaviour. A slot carries a sequence word written last on publish
//! and cleared first on overwrite, and a reader re-checks it after copying, so a copy that
//! raced a rewrite is rejected rather than used ([`BlockRing::read`]). The ring has
//! [`SLOTS`] slots and the depth is at most [`MAX_DEPTH`], so in normal running neither
//! side ever rewrites a slot the other is reading.
//!
//! **Trust.** The block is shared with another process. Each side treats what the other
//! wrote as untrusted input: a slot's frame count must match the reader's buffer,
//! parameter rings with impossible indices read as empty, and the engine checks the
//! host's reported latency against its own limits.
//!
//! **Compatibility:** a host uses a block only when [`Shared::config`] returns one,
//! which checks [`MAGIC`] and [`VERSION`]. Any change to the layout or to the meaning of
//! a field bumps [`VERSION`].
//!
//! Naming and mapping the block, starting the host process and watching it need OS APIs
//! and `unsafe`, so they live in `jstream-platform-windows` (engine side) and the host.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-plugin-proto/README.md#purpose, docs/adr/0035-clap-plugins-out-of-process.md

#![forbid(unsafe_code)]

mod params;
mod ring;
mod shared;
mod status;

pub use params::{ParamEvent, ParamRing};
pub use ring::{BlockRing, Slot, first_useful};
pub use shared::{Config, ConfigError, Shared};
pub use status::{
    ERROR_ACTIVATE, ERROR_BAD_CONFIG, ERROR_LOAD, ERROR_NONE, ERROR_NOT_AN_EFFECT, STATE_EMPTY,
    STATE_FAILED, STATE_READY, STATE_STOPPED, Status,
};

/// `JSPH` in little-endian byte order, so a zeroed or foreign mapping is never mistaken
/// for a plugin-host block.
pub const MAGIC: u32 = u32::from_le_bytes(*b"JSPH");

/// The contract's version. Bumped on any change to the layout or the meaning of a field.
pub const VERSION: u32 = 1;

/// Channels in every audio block: interleaved stereo, as the mixer's.
pub const CHANNELS: usize = 2;

/// The largest block, frames, a slot holds. A mixer with larger blocks can't use CLAP
/// filters.
pub const MAX_BLOCK_FRAMES: usize = 2048;

/// Slots in each audio ring.
pub const SLOTS: usize = 8;

/// The pipeline depth, in blocks, the engine uses unless told otherwise (ADR-0035: two
/// engine blocks).
pub const DEFAULT_DEPTH: u32 = 2;

/// The deepest pipeline allowed. Two slots stay spare, so the host's block in progress
/// and the mixer's newest block never share a slot with the block the mixer reads.
pub const MAX_DEPTH: u32 = SLOTS as u32 - 2;

/// Events each parameter ring holds.
pub const PARAM_SLOTS: usize = 256;

/// The most latency, frames, a host may report for its plugin. A plugin reporting more
/// fails to start.
pub const MAX_PLUGIN_LATENCY: u32 = 48_000;

/// A host whose heartbeat hasn't moved for this many milliseconds is treated as hung.
pub const LIVENESS_MS: u64 = 1000;

// Two slots stay spare, and the default depth is allowed.
const _: () = {
    assert!(DEFAULT_DEPTH >= 1 && DEFAULT_DEPTH <= MAX_DEPTH);
    assert!(MAX_DEPTH as usize + 2 == SLOTS);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_spells_jsph() {
        assert_eq!(MAGIC.to_le_bytes(), *b"JSPH");
        assert_ne!(MAGIC, 0);
    }
}
