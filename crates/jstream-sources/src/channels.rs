//! Channels' values (ADR-0183): one lock-free cell per channel, which the engine's
//! channel ticker stores and the render thread, value bindings and other channels load.
//!
//! Planning asks for a channel's [`ChannelCell`] by ID with [`ChannelLevels::cell`]; the
//! same ID shares one cell. A cell reads `None` until the engine follows a channel with
//! that ID, and again once the collection no longer holds it, so a reader tells a channel
//! at 0 from one that doesn't exist. Loading a cell is one atomic load: no lock and no
//! allocation ([R-RT-003](../../../docs/RULES.md#R-RT-003)). It is runtime state: never
//! saved, never an edit.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The most cells kept for IDs no channel has, so a list naming many missing channels
/// can't grow the store without bound. Past it, a new ID's cell is not kept and reads
/// `None` for ever.
pub const MAX_CHANNEL_CELLS: usize = 1024;

/// The bits a cell holds while no channel has its ID: a NaN no value is.
const ABSENT: u32 = 0x7fc0_dead;

/// One channel's value, as `f32` bits.
#[derive(Debug)]
pub struct ChannelCell(AtomicU32);

impl Default for ChannelCell {
    fn default() -> Self {
        Self(AtomicU32::new(ABSENT))
    }
}

impl ChannelCell {
    /// The channel's value now, 0 to 1, or `None` while the collection holds no channel
    /// with this ID. One atomic load.
    pub fn read(&self) -> Option<f32> {
        let bits = self.0.load(Ordering::Relaxed);
        (bits != ABSENT).then(|| f32::from_bits(bits))
    }

    /// Stores `value`, kept within 0 to 1 (a value that isn't finite is 0).
    pub fn store(&self, value: f32) {
        let value = if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }

    /// Marks it as no channel's: it reads `None`.
    pub fn clear(&self) {
        self.0.store(ABSENT, Ordering::Relaxed);
    }
}

/// Every channel's cell, by ID. Cloning it shares it.
#[derive(Clone, Debug, Default)]
pub struct ChannelLevels {
    cells: Arc<Mutex<BTreeMap<String, Arc<ChannelCell>>>>,
}

impl ChannelLevels {
    /// None yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Arc<ChannelCell>>> {
        self.cells.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cell of the channel with `channel_id`, the same one each time while someone
    /// holds it. Takes the store's lock briefly, as a leaf.
    pub fn cell(&self, channel_id: &str) -> Arc<ChannelCell> {
        let mut cells = self.lock();
        if let Some(cell) = cells.get(channel_id) {
            return Arc::clone(cell);
        }
        // Forget the cells nobody holds any more.
        cells.retain(|_, cell| Arc::strong_count(cell) > 1);
        let cell = Arc::new(ChannelCell::default());
        if cells.len() < MAX_CHANNEL_CELLS {
            cells.insert(channel_id.to_owned(), Arc::clone(&cell));
        }
        cell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_is_shared_by_id_and_reads_none_until_stored() {
        let levels = ChannelLevels::new();
        let a = levels.cell("power");
        let again = levels.cell("power");
        assert!(Arc::ptr_eq(&a, &again));
        assert_eq!(a.read(), None);
        a.store(0.25);
        assert_eq!(again.read(), Some(0.25));
        a.store(3.0);
        assert_eq!(a.read(), Some(1.0), "kept within 0 to 1");
        a.store(f32::NAN);
        assert_eq!(a.read(), Some(0.0));
        a.clear();
        assert_eq!(a.read(), None);
        drop((a, again));
        let fresh = levels.cell("power");
        assert_eq!(fresh.read(), None, "a forgotten cell starts afresh");
    }
}
