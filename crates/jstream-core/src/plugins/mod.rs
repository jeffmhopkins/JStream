//! CLAP plugins hosted out of process (ADR-0035): the filter kinds a scan finds, and
//! the supervisor that keeps each filter's host process running, and a CLAP filter in
//! a source's audio chain. And, with the `wasm-plugins` feature, the sandboxed WASM
//! automation plugins the engine runs in process (ADR-0069, ADR-0075).
//!
//! # Behaviour
//!
//! **CLAP plugins** (`plugins/`, ADR-0035):
//! - `Engine::scan_clap_plugins(Arc<dyn PluginHosts>)` scans the CLAP search directories
//!   (recursively, for `.clap` entries, each scanned in its own host process) and adds
//!   every audio effect found to the graph's kinds as an audio-filter kind,
//!   `clap/<plugin id>` (`CLAP_KIND_PREFIX`), so `kind.list` and `kind.describe` report
//!   it. Its settings are the plugin's parameters, by CLAP id in decimal, all live: a
//!   stepped one is an `int`, any other a `float`, both with the plugin's range; hidden,
//!   read-only and bypass parameters and ones without a finite range are left out. When
//!   two files offer one plugin id, the first found wins. A plugin's names reach clients
//!   through `plugin_text` (#351, #977): control and bidirectional formatting characters
//!   become spaces, trimmed, at most 128 characters. The scan returns a `ClapScan`
//!   with each kind's file and plugin id, and the files that failed. Kinds stay until
//!   the engine ends. It is slow, so a host calls it on a worker thread; the engine's
//!   lock is held only to add the kinds and hand the audio thread a `ClapCatalog` (the
//!   hosts, and each kind's file and plugin id, added to earlier scans').
//! - `HostSupervisor` keeps one CLAP filter instance's host process running, on a
//!   control thread, driven by `tick(now)`, which returns `HostEvent`s. A host is
//!   `Ready` (with its block and latency) once its block says so. One that exits, isn't
//!   ready within `start_timeout` (it may load the plugin without beating until then),
//!   or, once ready, stops its heartbeat for `LIVENESS_MS` has `Crashed`: it is killed and restarted after a wait that doubles from `first_retry`
//!   up to `max_retry`. After `max_crashes` crashes with no stable run (`stable_after`)
//!   between them, or at once when the host marks a failure a restart can't fix (load,
//!   not an effect, activate, configuration), it is `Disabled` until `enable`. The
//!   supervisor asks for the plugin's state whenever the plugin marks it changed, keeps
//!   the latest (`saved_state`, `StateSaved`) and restarts with it. A run of
//!   `degraded_after` consecutive late blocks in the mixer is `Degraded`, and its end
//!   `Recovered`; the mixer counts them in the engine's memory (`EngineBlock`), where
//!   the host can't reset them. What the host sends back is checked first (#351):
//!   parameter descriptions with a non-finite or inverted range, a default outside it,
//!   or a repeated id are dropped from `Params`; a parameter change, from a control
//!   message or the block's `params_out` ring (read only once the run's first `Params`
//!   has arrived), must name a parameter of the last `Params` and be finite and within its range to become `ParamChanged`; and a saved
//!   state over `MAX_STATE` (2 MiB) is ignored. Each dropped reply counts in `rejected`,
//!   and the first of each host run is an `Error`. `stop` asks the host to stop and
//!   kills it after `STOP_GRACE`.
//! - A CLAP filter runs as `plugins::filter::ClapFilter`, one host per filter. In a
//!   source's audio chain the input's `filters::Chain` owns it (see `audio_sources`). On
//!   the master stage's audio chain or an output's (ADR-0080), the program stage's
//!   `jstream-audio-sources` thread owns it (`filters::ClapHosts`), and the routing puts
//!   the filter on its host's block once that is ready (see the crate README's Audio routing): on the
//!   master bus's copy of the mixer's master chain, and on each other bus that feeds an
//!   output (an output tap reads it) its copy on a host of that bus's own (ADR-0086).
//!   A bus's copy delays its dry signal by the filter's latency while its host starts
//!   and once the bus feeds no output. A collection holding one
//!   loads before the scan describes its kind (the graph keeps such a filter verbatim),
//!   and the filter is bypassed until then.
//! - **Not yet:** writing the plugin's own parameter changes and saved state back to the
//!   filter's settings (the supervisor keeps the state for its restarts only, so a new
//!   host after an engine restart starts from the plugin's defaults and the filter's
//!   settings); a new mixer filter when a restarted plugin reports a new latency; the
//!   plugin's editor; rescanning; and stopping a removed filter's host off the
//!   `jstream-audio-sources` thread (now each stop waits up to `STOP_GRACE` there).

#[cfg(feature = "wasm-plugins")]
pub(crate) mod automation;
pub(crate) mod filter;
pub mod kinds;
pub mod supervisor;
