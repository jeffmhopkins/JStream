//! The JStream engine. [`Engine`] owns the collection's scene graph (`jstream-graph`)
//! and executes protocol calls against it: atomic edit batches with `expected_rev` and
//! idempotency keys, queries, and output actions, with every event fanned out to every
//! subscription. [`InProcessControl`] serves it through the `Control` trait in the same
//! process, one session per [`Engine::connect`] (ADR-0001). Layer 5.
//!
//! This first cut is the control plane only. Outputs are a placeholder state machine
//! until the media pipeline lands, and there is no undo, audit log, persistence,
//! thread or clock yet.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-core/README.md#purpose, crates/jstream-core/README.md#design, docs/adr/0001-library-first-engine.md, docs/adr/0013-command-model.md

#![forbid(unsafe_code)]

mod engine;
mod events;
mod outputs;
mod session;
mod snapshot;
#[cfg(test)]
mod tests;

pub use engine::Engine;
pub use session::InProcessControl;
pub use snapshot::SourceFrameError;

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, ignoring poisoning. Every change under the engine's locks is computed
/// before it is committed (`Graph::apply` works on a copy and replaces the state only on
/// success), so a panic while a lock is held leaves the data consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// The test below sits at the crate root because RULES.md names it by path (R-API-004):
// `jstream_core::stale_expected_rev_is_rejected`.

/// R-API-004: a batch whose `expected_rev` is stale fails with `rev_conflict`, carrying
/// both revisions, and applies nothing; the same batch at the current `rev` applies.
#[cfg(test)]
#[test]
fn stale_expected_rev_is_rejected() {
    tests::check_stale_expected_rev_is_rejected();
}
