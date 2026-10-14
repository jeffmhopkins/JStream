//! Sharing sessions between outputs (docs/ARCHITECTURE.md#data-model: outputs with the
//! same video mix and encoder profile share one encoder session).

use std::sync::{Arc, Weak};

use crate::EncoderSettings;

/// What makes two outputs' encodes the same: the mix (or audio bus) they encode, and the
/// encoder settings. `M` is the engine's mix or bus ID; the protocol has no encoder
/// profiles yet, so `settings` stands in for one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionKey<M> {
    /// The video mix or audio bus.
    pub mix: M,
    /// The encoder settings.
    pub settings: EncoderSettings,
}

/// Sessions by key, held weakly: a session lives as long as an output holds its `Arc`,
/// and the registry hands the same one to every output that asks for its key meanwhile.
///
/// Not synchronised; the engine keeps it on the control plane, where outputs are
/// started and stopped. To end a shared session cleanly, the output that releases it
/// last calls `finish` (it can tell with [`Arc::into_inner`]); dropping the last `Arc`
/// without finishing closes the session without draining it.
#[derive(Debug)]
pub struct SessionRegistry<K, S> {
    entries: Vec<(K, Weak<S>)>,
}

impl<K, S> Default for SessionRegistry<K, S> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K: PartialEq, S> SessionRegistry<K, S> {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The live session for `key`, if there is one.
    pub fn get(&self, key: &K) -> Option<Arc<S>> {
        self.entries
            .iter()
            .filter(|(k, _)| k == key)
            .find_map(|(_, s)| s.upgrade())
    }

    /// The live session for `key`, or a new one from `open`, registered under `key`.
    ///
    /// # Errors
    /// Whatever `open` returns; nothing is registered then.
    pub fn get_or_open<E>(
        &mut self,
        key: K,
        open: impl FnOnce() -> Result<S, E>,
    ) -> Result<Arc<S>, E> {
        self.entries.retain(|(_, s)| s.strong_count() > 0);
        if let Some(session) = self.get(&key) {
            return Ok(session);
        }
        let session = Arc::new(open()?);
        self.entries.push((key, Arc::downgrade(&session)));
        Ok(session)
    }

    /// How many registered sessions are still alive.
    pub fn len(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, s)| s.strong_count() > 0)
            .count()
    }

    /// Whether no registered session is alive.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
