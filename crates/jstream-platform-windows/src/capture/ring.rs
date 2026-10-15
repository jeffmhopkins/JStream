//! The ring of shared textures that GPU capture copies frames into (`jstream-platform`
//! README › GPU frames, frame lifetime): which slot the next frame goes to.
//!
//! A slot is free once nothing holds its frame and the GPU has finished with its last
//! staged use. The ring reuses a free slot of the frame's size, allocates while it has
//! room, and otherwise drops the frame: capture never waits for a slot (R-RT-004). Free
//! slots of another size (from before the window was resized) are released first, so
//! they don't keep their memory.
//!
//! Platform-independent, so it is tested on every platform.

/// One slot as the ring sees it.
pub(crate) trait Slot {
    /// The slot's texture size, in pixels.
    fn size(&self) -> (u32, u32);
    /// Whether the next frame may be written into it.
    fn is_free(&self) -> bool;
}

/// At most `capacity` slots.
pub(crate) struct Ring<S> {
    slots: Vec<S>,
    capacity: usize,
}

impl<S: Slot> Ring<S> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// The slot for the next frame, of `size`: a free one of that size, else a new one
    /// from `allocate` if the ring has room. `Ok(None)` when every slot is held.
    ///
    /// # Errors
    /// What `allocate` returns.
    pub(crate) fn acquire<E>(
        &mut self,
        size: (u32, u32),
        allocate: impl FnOnce() -> Result<S, E>,
    ) -> Result<Option<&S>, E> {
        self.slots.retain(|s| s.size() == size || !s.is_free());
        if let Some(i) = self
            .slots
            .iter()
            .position(|s| s.size() == size && s.is_free())
        {
            return Ok(self.slots.get(i));
        }
        if self.slots.len() >= self.capacity {
            return Ok(None);
        }
        self.slots.push(allocate()?);
        Ok(self.slots.last())
    }

    /// The number of slots allocated now.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::{Ring, Slot};

    /// A slot whose freedom the test sets, with an ID to tell slots apart.
    #[derive(Clone)]
    struct Fake {
        id: u32,
        size: (u32, u32),
        free: Rc<Cell<bool>>,
    }

    impl Slot for Fake {
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn is_free(&self) -> bool {
            self.free.get()
        }
    }

    /// Acquires a slot of `size`, allocating with the next ID, and marks it held.
    fn take(ring: &mut Ring<Fake>, size: (u32, u32), next: &mut u32) -> Option<Fake> {
        let id = *next;
        let slot = ring
            .acquire(size, || {
                Ok::<_, ()>(Fake {
                    id,
                    size,
                    free: Rc::new(Cell::new(true)),
                })
            })
            .expect("allocation can't fail here")
            .cloned();
        if let Some(s) = &slot {
            if s.id == id {
                *next += 1;
            }
            s.free.set(false);
        }
        slot
    }

    #[test]
    fn reuses_free_slots_and_drops_frames_when_every_slot_is_held() {
        let mut ring = Ring::new(2);
        let mut next = 0;
        let a = take(&mut ring, (4, 4), &mut next).expect("a");
        let b = take(&mut ring, (4, 4), &mut next).expect("b");
        assert_ne!(a.id, b.id);
        assert!(take(&mut ring, (4, 4), &mut next).is_none(), "both held");
        a.free.set(true);
        let c = take(&mut ring, (4, 4), &mut next).expect("a again");
        assert_eq!(c.id, a.id, "the free slot is reused, not reallocated");
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn a_new_size_releases_free_slots_of_the_old_one() {
        let mut ring = Ring::new(2);
        let mut next = 0;
        let a = take(&mut ring, (4, 4), &mut next).expect("a");
        let b = take(&mut ring, (4, 4), &mut next).expect("b");
        a.free.set(true);
        // `b` is still held at the old size; `a` is free, so it makes room.
        let c = take(&mut ring, (8, 2), &mut next).expect("a new slot");
        assert_eq!(c.size, (8, 2));
        assert_ne!(c.id, a.id);
        assert_eq!(ring.len(), 2, "a was released, b kept");
        assert!(take(&mut ring, (8, 2), &mut next).is_none(), "b and c held");
        b.free.set(true);
        c.free.set(true);
        let d = take(&mut ring, (8, 2), &mut next).expect("c again");
        assert_eq!(d.id, c.id);
        assert_eq!(ring.len(), 1, "b, free at the old size, was released");
    }

    #[test]
    fn an_allocation_failure_is_returned() {
        let mut ring = Ring::<Fake>::new(1);
        assert_eq!(
            ring.acquire((2, 2), || Err("no memory")).err(),
            Some("no memory")
        );
        assert_eq!(ring.len(), 0);
    }
}
