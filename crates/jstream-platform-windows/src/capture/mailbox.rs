//! The single-slot "latest frame" mailbox between a capture callback and the render
//! thread (docs/ARCHITECTURE.md#threading).
//!
//! Lock-free: the slot is one atomic pointer, and both sides only swap it. The writer
//! swaps in a new boxed value and gets back the one it replaced, which the reader never
//! took (a skipped frame). The reader swaps in null and gets the newest value, if any.
//! Each pointer is owned by exactly one side after its swap, so neither side ever waits
//! for the other (R-RT-002).
//!
//! Platform-independent, so it is tested on every platform.

use std::marker::PhantomData;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// A slot holding at most one `T`.
pub(crate) struct Mailbox<T> {
    /// Null, or a pointer from `Box::into_raw` that the slot owns.
    slot: AtomicPtr<T>,
    /// The slot owns a `Box<T>`.
    _owns: PhantomData<Box<T>>,
}

impl<T> Mailbox<T> {
    pub(crate) fn new() -> Self {
        Self {
            slot: AtomicPtr::new(ptr::null_mut()),
            _owns: PhantomData,
        }
    }

    /// Puts `value` in the slot and returns the value it replaced, if the reader hadn't
    /// taken it.
    pub(crate) fn put(&self, value: T) -> Option<T> {
        let new = Box::into_raw(Box::new(value));
        // AcqRel: the release publishes `value` to the reader; the acquire makes the
        // replaced value (written by an earlier `put`) visible here.
        Self::unbox(self.slot.swap(new, Ordering::AcqRel))
    }

    /// Takes the newest value, leaving the slot empty.
    pub(crate) fn take(&self) -> Option<T> {
        Self::unbox(self.slot.swap(ptr::null_mut(), Ordering::AcqRel))
    }

    /// Takes ownership of a pointer swapped out of the slot.
    fn unbox(p: *mut T) -> Option<T> {
        if p.is_null() {
            return None;
        }
        // SAFETY: every non-null pointer in the slot came from `Box::into_raw` in `put`,
        // and the swap that returned it removed it from the slot, so this is its only
        // owner and it is turned back into a box exactly once.
        Some(*unsafe { Box::from_raw(p) })
    }
}

impl<T> Drop for Mailbox<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

// SAFETY: the mailbox only moves `T`s between threads (each value is owned by one side at
// a time, through atomic swaps) and never shares a `&T`, so `T: Send` is enough for both.
unsafe impl<T: Send> Send for Mailbox<T> {}
// SAFETY: as above; `&Mailbox` only allows `put` and `take`, which move whole values.
unsafe impl<T: Send> Sync for Mailbox<T> {}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::Mailbox;

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
