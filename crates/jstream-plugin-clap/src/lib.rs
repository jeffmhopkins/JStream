//! A safe wrapper over the CLAP audio plugin ABI, for
//! JStream's out-of-process plugin host (ADR-0035). Layer 2 and `unsafe`: the plugin is
//! a C ABI in a native library, reached through the `clap-sys` bindings
//! (R-UNSAFE-004). Only `jstream-plugin-host` links this crate, which lint-deps checks,
//! so third-party native code never loads into the engine's process.
//!
//! - [`Library`]: a loaded plugin file (`.clap`), its entry initialised, and the
//!   plugins it offers ([`PluginInfo`]).
//! - [`Plugin`]: one plugin instance on the host's main thread: its parameters,
//!   latency and saved state, and activation at a sample rate and block size, which
//!   checks it is a stereo audio effect.
//! - [`Processor`]: the active instance on the host's audio thread: deinterleaves a
//!   block, passes parameter changes in as events, runs the plugin, interleaves its
//!   output and collects the parameter changes it made itself. It never allocates.
//!
//! Every callback the plugin can call into JStream catches panics (R-UNSAFE-005), and
//! everything the plugin returns (strings, counts, port layouts) is checked before use.
//! A plugin is still native code running with the user's rights: hosting it out of
//! process isolates its crashes, not its behaviour.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-plugin-clap/README.md#purpose, docs/adr/0035-clap-plugins-out-of-process.md

mod error;
mod host;
mod library;
mod plugin;
mod process;
mod strings;
#[cfg(any(test, feature = "test-plugin"))]
mod testplugin;

/// A CLAP plugin file built into this crate against the raw ABI, for the plugin host's
/// tests (feature `test-plugin`, never in a release build): a stereo gain effect with a
/// parameter, latency and saved state, an instrument, a mono effect, a sidechain effect
/// and one with too many ports.
#[cfg(feature = "test-plugin")]
pub mod test_plugin {
    use std::sync::Arc;

    pub use crate::testplugin::{GAIN_ID, MONO_ID, SIDECHAIN_ID, SYNTH_ID, WIDE_ID};
    use crate::{ClapError, Library};

    /// The gain effect's gain parameter (0 to 2, default 1). For each change of it the
    /// plugin receives, it reports parameter [`ECHO_PARAM`] with the same value.
    pub const GAIN_PARAM: u32 = 7;
    /// The parameter the gain effect reports back.
    pub const ECHO_PARAM: u32 = 8;
    /// The gain effect's latency, frames.
    pub const LATENCY: u32 = 32;

    /// The built-in file, loaded.
    ///
    /// # Errors
    /// Never in practice: its entry is valid.
    pub fn library() -> Result<Arc<Library>, ClapError> {
        Library::from_static(&crate::testplugin::ENTRY)
    }
}

pub use error::ClapError;
pub use jstream_plugin_proto::ParamEvent;
pub use library::{Library, PluginInfo};
pub use plugin::{MAX_PORT_CHANNELS, MAX_PORTS, ParamInfo, Plugin};
pub use process::Processor;

// Counts allocations inside `assert_no_alloc` scopes in this crate's tests (R-RT-001's
// check for the processor).
#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

/// The largest saved state, bytes, the host accepts from or gives to a plugin
/// (ROADMAP M3: a CLAP filter's `state` may be 2 MiB).
pub const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;

/// Parameter events one block can carry each way. More are dropped and counted by the
/// caller.
pub const MAX_BLOCK_EVENTS: usize = jstream_plugin_proto::PARAM_SLOTS;
