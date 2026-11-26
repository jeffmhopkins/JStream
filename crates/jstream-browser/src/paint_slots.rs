//! Which of a page's slots the helper paints into next (Windows' shared textures, Linux
//! and macOS's slot files): the next one the engine isn't holding, and whether a paint
//! was skipped because it held them all. A page that only paints on change, as a page
//! fed by the page API does, would otherwise keep showing the frame before the skipped
//! paint until its next change, so the release that frees a slot after a skip asks for
//! the page to paint again (ADR-0252).
//!
//! Docs: crates/jstream-browser/README.md#purpose, docs/adr/0252-browser-paints-keep-up-on-a-busy-gpu.md

/// A page's `N` slots: which the engine holds, which is next, and whether a paint was
/// skipped since the last that went through.
#[derive(Debug)]
pub(crate) struct PaintSlots<const N: usize> {
    /// Slots the engine hasn't released.
    busy: [bool; N],
    next: usize,
    /// A paint was skipped as the engine held every slot, and none went through since.
    skipped: bool,
}

impl<const N: usize> Default for PaintSlots<N> {
    fn default() -> Self {
        Self {
            busy: [false; N],
            next: 0,
            skipped: false,
        }
    }
}

impl<const N: usize> PaintSlots<N> {
    /// The slot a paint goes into: the next the engine isn't holding. `None` when it
    /// holds them all, and the paint is skipped.
    pub(crate) fn free(&mut self) -> Option<usize> {
        let free = (0..N)
            .map(|i| (self.next + i) % N)
            .find(|&slot| !self.busy[slot]);
        if free.is_none() {
            self.skipped = true;
        }
        free
    }

    /// A paint went to the engine in `slot`, which it now holds.
    pub(crate) fn sent(&mut self, slot: usize) {
        if let Some(busy) = self.busy.get_mut(slot) {
            *busy = true;
        }
        self.next = slot + 1;
        self.skipped = false;
    }

    /// The engine is done with `slot`. True when a paint was skipped since the last that
    /// went through: the page should paint again. Each skip asks once.
    pub(crate) fn release(&mut self, slot: u8) -> bool {
        if let Some(busy) = self.busy.get_mut(usize::from(slot)) {
            *busy = false;
        }
        std::mem::take(&mut self.skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::PaintSlots;

    #[test]
    fn paints_go_round_the_slots_the_engine_isnt_holding() {
        let mut slots = PaintSlots::<3>::default();
        for want in [0, 1, 2] {
            let slot = slots.free().expect("a free slot");
            assert_eq!(slot, want);
            slots.sent(slot);
        }
        assert!(!slots.release(1), "nothing was skipped");
        assert_eq!(slots.free(), Some(1));
    }

    /// A paint skipped while the engine held every slot is asked for again when it frees
    /// one, so a page that only paints on change doesn't keep its old frame.
    #[test]
    fn a_release_after_a_skipped_paint_asks_for_a_repaint_once() {
        let mut slots = PaintSlots::<3>::default();
        for _ in 0..3 {
            let slot = slots.free().expect("a free slot");
            slots.sent(slot);
        }
        assert_eq!(slots.free(), None, "the engine holds every slot");
        assert!(slots.release(0), "the skipped paint is asked for again");
        assert!(!slots.release(1), "once");
    }

    #[test]
    fn a_release_of_a_slot_out_of_range_is_ignored() {
        let mut slots = PaintSlots::<3>::default();
        assert!(!slots.release(7));
        assert_eq!(slots.free(), Some(0));
    }
}
