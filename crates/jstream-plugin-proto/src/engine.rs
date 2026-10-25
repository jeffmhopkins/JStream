//! What the engine keeps about one block in its own memory, never in the mapping: the
//! host process can write anything in the mapping, so a count the engine acts on can't
//! live there (#351).

use std::sync::atomic::AtomicU64;

use crate::Shared;

/// The mixer's misses for one CLAP filter: blocks the host's output wasn't there for and
/// the mixer passed its dry signal instead. The mixer thread writes them and the control
/// thread reads them, both in the engine's process.
#[derive(Debug, Default)]
pub struct MixerMisses {
    /// Blocks the mixer found missing and bypassed with its dry signal.
    pub total: AtomicU64,
    /// Misses in a row, 0 after a block that arrived. The control thread marks the
    /// filter degraded when this keeps growing.
    pub run: AtomicU64,
}

/// One CLAP filter's block as the engine holds it: the mapping shared with the host,
/// and the [`MixerMisses`] beside it in the engine's own memory. The mixer's filter and
/// the control thread share one, so both see the same counts.
pub struct EngineBlock {
    mapping: Box<dyn AsRef<Shared> + Send + Sync>,
    misses: MixerMisses,
}

impl std::fmt::Debug for EngineBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineBlock")
            .field("misses", &self.misses)
            .finish_non_exhaustive()
    }
}

impl EngineBlock {
    /// The engine's side of `mapping` (an OS mapping, or a heap block in tests), with no
    /// misses counted.
    pub fn new(mapping: impl AsRef<Shared> + Send + Sync + 'static) -> Self {
        Self {
            mapping: Box::new(mapping),
            misses: MixerMisses::default(),
        }
    }

    /// The mixer's misses on this block.
    pub fn misses(&self) -> &MixerMisses {
        &self.misses
    }
}

impl AsRef<Shared> for EngineBlock {
    fn as_ref(&self) -> &Shared {
        (*self.mapping).as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    /// The host owns everything in the mapping, misses included if they were there:
    /// rewriting every status word leaves the engine's count alone.
    #[test]
    fn the_host_cannot_reach_the_miss_counts() {
        let shared: Arc<Shared> = Shared::new_boxed().into();
        let block = EngineBlock::new(Arc::clone(&shared));
        block.misses().total.store(7, Ordering::Relaxed);
        block.misses().run.store(3, Ordering::Relaxed);
        let s = &shared.status;
        for word in [
            &s.reset_seq,
            &s.host_reset_seq,
            &s.heartbeat,
            &s.processed,
            &s.host_skipped,
        ] {
            word.store(0, Ordering::Relaxed);
        }
        assert_eq!(block.misses().total.load(Ordering::Relaxed), 7);
        assert_eq!(block.misses().run.load(Ordering::Relaxed), 3);
        assert!(std::ptr::eq(block.as_ref(), &*shared));
    }
}
