//! What waits for the GPU before it is let go: a pixel buffer whose frame the engine's
//! last staged submit still reads. No system calls, so it is tested on every platform.

/// Items held until a counter reaches the value each waits for.
///
/// A frame's hold retires its pixel buffer here when the last clone of the frame drops
/// while the GPU may still read it: the value is the shared event's value that the read's
/// submit signals. [`release`](Self::release) lets go of every item whose value the event
/// has reached. The values a context hands out only increase, so an item retired later
/// never waits for a smaller value than one retired before it, but nothing here relies on
/// that.
#[derive(Debug)]
pub(crate) struct Retired<T> {
    waiting: Vec<(u64, T)>,
}

impl<T> Default for Retired<T> {
    fn default() -> Self {
        Self {
            waiting: Vec::new(),
        }
    }
}

impl<T> Retired<T> {
    /// Holds `item` until the counter reaches `value`, unless it has already (`done` is
    /// the counter now), in which case `item` drops at once.
    pub(crate) fn retire(&mut self, value: u64, done: u64, item: T) {
        if value > done {
            self.waiting.push((value, item));
        }
    }

    /// Lets go of every item whose value is at or below `done`, the counter now.
    pub(crate) fn release(&mut self, done: u64) {
        self.waiting.retain(|(value, _)| *value > done);
    }

    /// How many items are still held.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.waiting.len()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::Retired;

    #[test]
    fn an_item_the_gpu_is_done_with_drops_at_once() {
        let item = Rc::new(());
        let mut retired = Retired::default();
        retired.retire(3, 3, Rc::clone(&item));
        retired.retire(2, 5, Rc::clone(&item));
        retired.retire(0, 0, Rc::clone(&item));
        assert_eq!(retired.len(), 0);
        assert_eq!(Rc::strong_count(&item), 1);
    }

    #[test]
    fn items_are_held_until_the_counter_reaches_their_value() {
        let a = Rc::new(());
        let b = Rc::new(());
        let mut retired = Retired::default();
        retired.retire(4, 2, Rc::clone(&a));
        retired.retire(6, 2, Rc::clone(&b));
        assert_eq!(retired.len(), 2);
        retired.release(3);
        assert_eq!((Rc::strong_count(&a), Rc::strong_count(&b)), (2, 2));
        retired.release(4);
        assert_eq!((Rc::strong_count(&a), Rc::strong_count(&b)), (1, 2));
        retired.release(10);
        assert_eq!((Rc::strong_count(&b), retired.len()), (1, 0));
    }
}
