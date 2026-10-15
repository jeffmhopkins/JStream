//! The JStream engine. [`Engine`] owns the collection's scene graph (`jstream-graph`)
//! and executes protocol calls against it: atomic edit batches with `expected_rev` and
//! idempotency keys, queries, and output actions, with every event fanned out to every
//! subscription. [`InProcessControl`] serves it through the `Control` trait in the same
//! process, one session per [`Engine::connect`] (ADR-0001). Layer 5.
//!
//! Without platform backends, outputs are a placeholder state machine.
//! [`Engine::with_backends`](engine::Engine::with_backends) and
//! [`Engine::with_publish`](engine::Engine::with_publish) (or
//! [`Engine::with_publish_opener`](engine::Engine::with_publish_opener)) run the media pipeline on
//! `output.start` instead. That pipeline registers the drop sources a `stats.tick`
//! sums and emits one about once a second while it runs. There is no undo, audit log
//! or persistence yet.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, docs/ARCHITECTURE.md#threading, crates/jstream-core/README.md#purpose, crates/jstream-core/README.md#design, docs/adr/0001-library-first-engine.md, docs/adr/0013-command-model.md

#![forbid(unsafe_code)]

mod drops;
mod engine;
mod events;
mod kinds;
mod outputs;
mod runtime;
mod session;
mod snapshot;
#[cfg(test)]
mod tests;

pub use drops::{
    AudioBusDrops, AudioEncodeDrops, AudioInputDrops, DropSnapshot, DropSource, DropSourceId,
    FnDropSource, VideoEncodeDrops, audio_bus_drops, audio_encode_drops, audio_input_drops,
    capture_drops, output_packet_drops, render_tick_drops, video_encode_drops,
};
pub use engine::Engine;
pub use runtime::{
    LinkState, LivePublish, OpenPublish, OutputStats, PublishError, RecordingPublish, RtmpPublish,
    connect_rtmp,
};
pub use session::InProcessControl;
pub use snapshot::SourceFrameError;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, ignoring poisoning. Every change under the engine's locks is computed
/// before it is committed (`Graph::apply` works on a copy and replaces the state only on
/// success), so a panic while a lock is held leaves the data consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// The tests below sit at the crate root because RULES.md names them by path:
// `jstream_core::stale_expected_rev_is_rejected` (R-API-004) and
// `jstream_core::secrets_are_redacted` (R-SEC-001).

/// R-API-004: a batch whose `expected_rev` is stale fails with `rev_conflict`, carrying
/// both revisions, and applies nothing; the same batch at the current `rev` applies.
#[cfg(test)]
#[test]
fn stale_expected_rev_is_rejected() {
    tests::check_stale_expected_rev_is_rejected();
}

/// R-SEC-001: a stream key set through the protocol reaches the credential store and
/// nothing else the engine sends or logs.
#[cfg(test)]
#[test]
fn secrets_are_redacted() {
    tests::check_secrets_are_redacted();
}
