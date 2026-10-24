//! The single-slot "latest frame" mailbox between a backend's capture callback and the
//! render thread (docs/ARCHITECTURE.md#threading), shared by every platform crate.
//!
//! Lock-free: the slot is one atomic cell holding an optional box, and both sides only
//! swap it. The writer swaps in a new boxed value and gets back the one it replaced, which
//! the reader never took (a skipped frame). The reader swaps in nothing and gets the
//! newest value, if any. Each box is owned by exactly one side after its swap, so neither
//! side ever waits for the other (R-RT-002). The cell is `crossbeam-utils`' `AtomicCell`,
//! which keeps the atomics behind a safe API; for a pointer-sized value like this one it
//! is a single atomic swap.

use crossbeam_utils::atomic::AtomicCell;

/// A slot holding at most one `T`: the capture side [`put`](Self::put)s, the render side
/// [`take`](Self::take)s, and neither blocks. `Send` and `Sync` when `T` is `Send`.
pub struct Mailbox<T> {
    slot: AtomicCell<Option<Box<T>>>,
}

impl<T> Mailbox<T> {
    /// An empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slot: AtomicCell::new(None),
        }
    }

    /// Puts `value` in the slot and returns the value it replaced, if the reader hadn't
    /// taken it (a skipped frame, for the caller to count).
    pub fn put(&self, value: T) -> Option<T> {
        self.slot.swap(Some(Box::new(value))).map(|b| *b)
    }

    /// Takes the newest value, leaving the slot empty.
    pub fn take(&self) -> Option<T> {
        self.slot.swap(None).map(|b| *b)
    }
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> std::fmt::Debug for Mailbox<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailbox").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crossbeam_utils::atomic::AtomicCell;

    use super::Mailbox;

    #[test]
    fn the_slot_is_lock_free() {
        // An optional box is pointer-sized, so the cell swaps it with one atomic
        // instruction rather than its fallback lock.
        assert!(AtomicCell::<Option<Box<[u8; 64]>>>::is_lock_free());
    }

    #[test]
    fn is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<Mailbox<Vec<u8>>>();
    }

    #[test]
    fn keeps_only_the_newest_value() {
        let m = Mailbox::new();
        assert_eq!(m.take(), None::<u32>);
        assert_eq!(m.put(1), None);
        assert_eq!(m.put(2), Some(1), "the replaced value comes back");
        assert_eq!(m.take(), Some(2));
        assert_eq!(m.take(), None);
    }

    /// Counts drops, to check that no value is leaked or dropped twice.
    struct Counted(Arc<AtomicUsize>);

    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn drops_what_it_holds() {
        let drops = Arc::new(AtomicUsize::new(0));
        let m = Mailbox::new();
        assert!(m.put(Counted(Arc::clone(&drops))).is_none());
        drop(m);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn every_value_is_taken_or_replaced_exactly_once_across_threads() {
        const N: u64 = 20_000;
        let m = Arc::new(Mailbox::new());
        let writer = {
            let m = Arc::clone(&m);
            thread::spawn(move || (0..N).filter(|&i| m.put(i).is_some()).count() as u64)
        };
        let mut taken = Vec::new();
        while !writer.is_finished() {
            if let Some(v) = m.take() {
                taken.push(v);
            }
        }
        let skipped = writer.join().expect("writer");
        taken.extend(m.take());
        // Values arrive in order, and each one was either taken or skipped.
        assert!(taken.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(taken.len() as u64 + skipped, N);
        assert_eq!(
            taken.last(),
            Some(&(N - 1)),
            "the newest value is never lost"
        );
    }
}
