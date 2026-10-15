//! The parameter-event rings: single-producer, single-consumer queues of parameter
//! changes, one from the engine to the host and one back.

use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::PARAM_SLOTS;

/// A parameter change: the plugin's parameter id and its new plain value (CLAP
/// parameters are `f64`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamEvent {
    /// The plugin's `clap_id` for the parameter.
    pub id: u32,
    /// The new value, in the parameter's own range.
    pub value: f64,
}

#[repr(C)]
#[derive(Debug, Default)]
struct EventSlot {
    id: AtomicU32,
    /// Always 0. Makes the padding before the value explicit.
    reserved: AtomicU32,
    value: AtomicU64,
}

/// A bounded queue of [`ParamEvent`]s. One process pushes, the other pops.
///
/// `write` counts events pushed and `read` events popped; both only grow (wrapping), and
/// `write − read` is the number queued. A ring whose counts are further apart than
/// [`PARAM_SLOTS`] was corrupted by the other side, and reads as empty to the consumer
/// and full to the producer.
#[repr(C)]
#[derive(Debug)]
pub struct ParamRing {
    write: AtomicU64,
    read: AtomicU64,
    events: [EventSlot; PARAM_SLOTS],
}

impl Default for ParamRing {
    fn default() -> Self {
        Self {
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            events: std::array::from_fn(|_| EventSlot::default()),
        }
    }
}

// The layout is the contract: any change here is a VERSION bump.
const _: () = {
    assert!(size_of::<EventSlot>() == 16);
    assert!(offset_of!(EventSlot, id) == 0);
    assert!(offset_of!(EventSlot, reserved) == 4);
    assert!(offset_of!(EventSlot, value) == 8);
    assert!(size_of::<ParamRing>() == 16 + 16 * PARAM_SLOTS);
    assert!(align_of::<ParamRing>() == 8);
    assert!(offset_of!(ParamRing, write) == 0);
    assert!(offset_of!(ParamRing, read) == 8);
    assert!(offset_of!(ParamRing, events) == 16);
};

impl ParamRing {
    fn slot(&self, index: u64) -> &EventSlot {
        &self.events[(index % PARAM_SLOTS as u64) as usize]
    }

    /// Queues `event`. False when the ring is full (or corrupt). The producer's side
    /// only; never allocates, locks or blocks.
    pub fn push(&self, event: ParamEvent) -> bool {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= PARAM_SLOTS as u64 {
            return false;
        }
        let slot = self.slot(write);
        slot.id.store(event.id, Ordering::Relaxed);
        slot.value.store(event.value.to_bits(), Ordering::Relaxed);
        self.write.store(write.wrapping_add(1), Ordering::Release);
        true
    }

    /// Takes the oldest queued event. `None` when the ring is empty (or corrupt). The
    /// consumer's side only; never allocates, locks or blocks.
    pub fn pop(&self) -> Option<ParamEvent> {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        let queued = write.wrapping_sub(read);
        if queued == 0 || queued > PARAM_SLOTS as u64 {
            return None;
        }
        let slot = self.slot(read);
        let event = ParamEvent {
            id: slot.id.load(Ordering::Relaxed),
            value: f64::from_bits(slot.value.load(Ordering::Relaxed)),
        };
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(event)
    }

    /// Drops every queued event, as for a new host. Only while neither side is using
    /// the ring.
    pub fn clear(&self) {
        self.write.store(0, Ordering::Relaxed);
        self.read.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: u32) -> ParamEvent {
        ParamEvent {
            id,
            value: f64::from(id) / 2.0,
        }
    }

    #[test]
    fn events_come_out_in_order() {
        let ring = Box::<ParamRing>::default();
        assert_eq!(ring.pop(), None);
        for id in 0..10 {
            assert!(ring.push(ev(id)));
        }
        for id in 0..10 {
            assert_eq!(ring.pop(), Some(ev(id)));
        }
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn a_full_ring_refuses_until_popped() {
        let ring = Box::<ParamRing>::default();
        for id in 0..PARAM_SLOTS as u32 {
            assert!(ring.push(ev(id)));
        }
        assert!(!ring.push(ev(999)));
        assert_eq!(ring.pop(), Some(ev(0)));
        assert!(ring.push(ev(999)));
        // Wraps around the slots in order.
        for id in 1..PARAM_SLOTS as u32 {
            assert_eq!(ring.pop(), Some(ev(id)));
        }
        assert_eq!(ring.pop(), Some(ev(999)));
    }

    #[test]
    fn corrupt_counts_read_as_empty_and_full() {
        let ring = Box::<ParamRing>::default();
        ring.write.store(10_000, Ordering::Relaxed);
        assert_eq!(ring.pop(), None);
        assert!(!ring.push(ev(1)));
        ring.clear();
        assert!(ring.push(ev(1)));
        assert_eq!(ring.pop(), Some(ev(1)));
    }

    #[test]
    fn counts_wrap() {
        let ring = Box::<ParamRing>::default();
        ring.write.store(u64::MAX, Ordering::Relaxed);
        ring.read.store(u64::MAX, Ordering::Relaxed);
        assert!(ring.push(ev(7)));
        assert!(ring.push(ev(8)));
        assert_eq!(ring.pop(), Some(ev(7)));
        assert_eq!(ring.pop(), Some(ev(8)));
        assert_eq!(ring.pop(), None);
    }

    #[test]
    fn crosses_threads_in_order() {
        let ring: std::sync::Arc<ParamRing> = std::sync::Arc::from(Box::<ParamRing>::default());
        let producer = {
            let ring = std::sync::Arc::clone(&ring);
            std::thread::spawn(move || {
                let mut id = 0;
                while id < 10_000 {
                    if ring.push(ev(id)) {
                        id += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
            })
        };
        let mut next = 0;
        while next < 10_000 {
            match ring.pop() {
                Some(e) => {
                    assert_eq!(e, ev(next));
                    next += 1;
                }
                None => std::thread::yield_now(),
            }
        }
        producer.join().unwrap();
    }
}
