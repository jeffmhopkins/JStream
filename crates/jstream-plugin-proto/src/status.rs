//! The status block at the start of the shared mapping.

use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicU32, AtomicU64};

/// [`Status::host_state`]: no host has loaded a plugin yet (a fresh or re-initialised
/// block).
pub const STATE_EMPTY: u32 = 0;
/// [`Status::host_state`]: the plugin is loaded and activated at the block's
/// configuration, [`Status::plugin_latency`] is set, and the host processes blocks.
pub const STATE_READY: u32 = 1;
/// [`Status::host_state`]: the host could not start the plugin; [`Status::host_error`]
/// says why. The host exits.
pub const STATE_FAILED: u32 = 2;
/// [`Status::host_state`]: the host saw [`Status::stop`] and is exiting.
pub const STATE_STOPPED: u32 = 3;

/// [`Status::host_error`]: no error.
pub const ERROR_NONE: u32 = 0;
/// [`Status::host_error`]: the block's configuration was missing or out of range.
pub const ERROR_BAD_CONFIG: u32 = 1;
/// [`Status::host_error`]: the plugin file could not be loaded, or has no such plugin.
pub const ERROR_LOAD: u32 = 2;
/// [`Status::host_error`]: the plugin is not a stereo audio effect.
pub const ERROR_NOT_AN_EFFECT: u32 = 3;
/// [`Status::host_error`]: the plugin refused to activate at the block's rate and block
/// size, or reported a latency over [`MAX_PLUGIN_LATENCY`](crate::MAX_PLUGIN_LATENCY).
pub const ERROR_ACTIVATE: u32 = 4;

/// The status block at offset 0 of the shared mapping.
///
/// Every field is atomic, so both processes can read and write it at any time, and every
/// bit pattern (including the OS's zero fill) is a valid value. Each field has one
/// writer: the engine's control thread, the host, or the mixer thread.
///
/// **Publishing.** The engine writes the configuration and clears everything else before
/// storing [`MAGIC`](crate::MAGIC) (release); the host reads the configuration only after
/// observing it (acquire), through [`Shared::config`](crate::Shared::config). The host
/// stores `plugin_latency` before `host_state = STATE_READY` (release).
///
/// The layout is fixed by `repr(C)` and pinned by compile-time assertions on every
/// field's offset, so it is the same on every target.
#[repr(C)]
#[derive(Debug, Default)]
pub struct Status {
    /// [`MAGIC`](crate::MAGIC) once the engine has initialised the block.
    pub magic: AtomicU32,
    /// [`VERSION`](crate::VERSION) of the engine that initialised the block.
    pub version: AtomicU32,

    // --- engine: configuration, written before the host starts ---
    /// The engine's sample rate, Hz.
    pub sample_rate: AtomicU32,
    /// Frames in every block, 1 to [`MAX_BLOCK_FRAMES`](crate::MAX_BLOCK_FRAMES).
    pub block_frames: AtomicU32,
    /// The pipeline depth, blocks, 1 to [`MAX_DEPTH`](crate::MAX_DEPTH).
    pub depth: AtomicU32,
    /// Channels per frame: always [`CHANNELS`](crate::CHANNELS).
    pub channels: AtomicU32,

    // --- host ---
    /// One of `STATE_*`.
    pub host_state: AtomicU32,
    /// One of `ERROR_*`, set with [`STATE_FAILED`].
    pub host_error: AtomicU32,
    /// The plugin's own latency, frames, as it reported it when activated.
    pub plugin_latency: AtomicU32,

    // --- engine: requests ---
    /// Set to 1 by the engine to ask the host to deactivate the plugin and exit.
    pub stop: AtomicU32,
    /// Bumped by the mixer when its chain is reset, so the host resets the plugin too.
    pub reset_seq: AtomicU64,

    // --- host ---
    /// The last `reset_seq` the host acted on.
    pub host_reset_seq: AtomicU64,
    /// Bumped by the host on every pass of its loop (liveness).
    pub heartbeat: AtomicU64,
    /// Blocks the host has processed and written.
    pub processed: AtomicU64,
    /// Blocks the host skipped: too late to be read, or rewritten while it copied them.
    pub host_skipped: AtomicU64,

    // --- mixer ---
    /// Blocks the mixer found missing and bypassed with its dry signal.
    pub mixer_misses: AtomicU64,
    /// Misses in a row, 0 after a block that arrived. The control thread marks the
    /// filter degraded when this keeps growing.
    pub mixer_miss_run: AtomicU64,
}

// The layout is the contract: any change here is a VERSION bump.
const _: () = {
    assert!(size_of::<Status>() == 96);
    assert!(align_of::<Status>() == 8);
    assert!(offset_of!(Status, magic) == 0);
    assert!(offset_of!(Status, version) == 4);
    assert!(offset_of!(Status, sample_rate) == 8);
    assert!(offset_of!(Status, block_frames) == 12);
    assert!(offset_of!(Status, depth) == 16);
    assert!(offset_of!(Status, channels) == 20);
    assert!(offset_of!(Status, host_state) == 24);
    assert!(offset_of!(Status, host_error) == 28);
    assert!(offset_of!(Status, plugin_latency) == 32);
    assert!(offset_of!(Status, stop) == 36);
    assert!(offset_of!(Status, reset_seq) == 40);
    assert!(offset_of!(Status, host_reset_seq) == 48);
    assert!(offset_of!(Status, heartbeat) == 56);
    assert!(offset_of!(Status, processed) == 64);
    assert!(offset_of!(Status, host_skipped) == 72);
    assert!(offset_of!(Status, mixer_misses) == 80);
    assert!(offset_of!(Status, mixer_miss_run) == 88);
};
