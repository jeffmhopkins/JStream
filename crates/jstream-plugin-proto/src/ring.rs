//! The audio rings: one block per slot, published with a sequence word.

use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use crate::{CHANNELS, MAX_BLOCK_FRAMES, SLOTS};

/// Samples a slot holds: [`MAX_BLOCK_FRAMES`] interleaved stereo frames.
const SLOT_SAMPLES: usize = MAX_BLOCK_FRAMES * CHANNELS;

/// One block of interleaved stereo `f32` audio, stored as the samples' bits.
///
/// `seq` is 0 while the slot is empty or being rewritten, and `n + 1` once block `n` is
/// complete in it.
#[repr(C)]
#[derive(Debug)]
pub struct Slot {
    seq: AtomicU64,
    frames: AtomicU32,
    /// Always 0. Makes the padding before the samples explicit.
    reserved: AtomicU32,
    samples: [AtomicU32; SLOT_SAMPLES],
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            frames: AtomicU32::new(0),
            reserved: AtomicU32::new(0),
            samples: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

/// A ring of [`SLOTS`] blocks, written by one side and read by the other. Block `n`
/// lives in slot `n % SLOTS`.
#[repr(C)]
#[derive(Debug)]
pub struct BlockRing {
    /// One past the newest block written: the writer's next block number.
    head: AtomicU64,
    slots: [Slot; SLOTS],
}

impl Default for BlockRing {
    fn default() -> Self {
        Self {
            head: AtomicU64::new(0),
            slots: std::array::from_fn(|_| Slot::default()),
        }
    }
}

// The layout is the contract: any change here is a VERSION bump.
const _: () = {
    assert!(size_of::<Slot>() == 16 + 4 * SLOT_SAMPLES);
    assert!(align_of::<Slot>() == 8);
    assert!(offset_of!(Slot, seq) == 0);
    assert!(offset_of!(Slot, frames) == 8);
    assert!(offset_of!(Slot, reserved) == 12);
    assert!(offset_of!(Slot, samples) == 16);
    assert!(size_of::<BlockRing>() == 8 + SLOTS * size_of::<Slot>());
    assert!(offset_of!(BlockRing, head) == 0);
    assert!(offset_of!(BlockRing, slots) == 8);
};

impl BlockRing {
    fn slot(&self, block: u64) -> &Slot {
        &self.slots[(block % SLOTS as u64) as usize]
    }

    /// One past the newest block written (0 before the first). Untrusted when the other
    /// process writes this ring.
    pub fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Writes `samples` (interleaved stereo, 1 to [`MAX_BLOCK_FRAMES`] frames) as block
    /// `block`, then publishes it. False, writing nothing, for a length out of range.
    /// The writer's side only; never allocates, locks or blocks.
    pub fn write(&self, block: u64, samples: &[f32]) -> bool {
        if samples.is_empty()
            || !samples.len().is_multiple_of(CHANNELS)
            || samples.len() > SLOT_SAMPLES
        {
            return false;
        }
        let slot = self.slot(block);
        // Unpublish first, so a reader that copies any of the new samples sees the
        // sequence change when it re-checks.
        slot.seq.store(0, Ordering::Relaxed);
        fence(Ordering::Release);
        slot.frames
            .store((samples.len() / CHANNELS) as u32, Ordering::Relaxed);
        for (dst, src) in slot.samples.iter().zip(samples) {
            dst.store(src.to_bits(), Ordering::Relaxed);
        }
        slot.seq.store(block.wrapping_add(1), Ordering::Release);
        self.head.store(block.wrapping_add(1), Ordering::Release);
        true
    }

    /// Copies block `block` into `out`. True only when that block is in its slot, holds
    /// exactly `out.len()` samples, and was not rewritten while it was copied; on false,
    /// `out` holds garbage. The reader's side only; never allocates, locks or blocks.
    pub fn read(&self, block: u64, out: &mut [f32]) -> bool {
        let slot = self.slot(block);
        let want = block.wrapping_add(1);
        if want == 0 || slot.seq.load(Ordering::Acquire) != want {
            return false;
        }
        let frames = slot.frames.load(Ordering::Relaxed) as usize;
        if frames * CHANNELS != out.len() || out.len() > SLOT_SAMPLES {
            return false;
        }
        for (dst, src) in out.iter_mut().zip(&slot.samples) {
            *dst = f32::from_bits(src.load(Ordering::Relaxed));
        }
        fence(Ordering::Acquire);
        slot.seq.load(Ordering::Relaxed) == want
    }

    /// Empties every slot and sets the head to 0, as for a new host. Only while neither
    /// side is using the ring.
    pub fn clear(&self) {
        for slot in &self.slots {
            slot.seq.store(0, Ordering::Relaxed);
            slot.frames.store(0, Ordering::Relaxed);
        }
        self.head.store(0, Ordering::Release);
    }
}

/// The oldest block the mixer can still read, when the input ring's head is `head` and
/// the pipeline is `depth` blocks deep: the mixer has already read every output block
/// before `head − depth`. A host that is behind starts at
/// `max(its next block, first_useful(head, depth))`.
pub fn first_useful(head: u64, depth: u32) -> u64 {
    head.saturating_sub(u64::from(depth))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn block(start: f32, frames: usize) -> Vec<f32> {
        (0..frames * CHANNELS).map(|i| start + i as f32).collect()
    }

    #[test]
    fn round_trips_a_block() {
        let ring = Box::<BlockRing>::default();
        assert_eq!(ring.head(), 0);
        let b = block(1.0, 256);
        assert!(ring.write(5, &b));
        assert_eq!(ring.head(), 6);
        let mut out = vec![0.0; b.len()];
        assert!(ring.read(5, &mut out));
        assert_eq!(out, b);
    }

    #[test]
    fn rejects_the_wrong_block_or_length() {
        let ring = Box::<BlockRing>::default();
        assert!(ring.write(3, &block(0.0, 64)));
        let mut out = vec![0.0; 128];
        // Same slot, another block number.
        assert!(!ring.read(3 + SLOTS as u64, &mut out));
        assert!(!ring.read(2, &mut out));
        // Another length.
        assert!(!ring.read(3, &mut [0.0; 64]));
        // Empty, odd and oversized writes are refused.
        assert!(!ring.write(4, &[]));
        assert!(!ring.write(4, &[0.0; 3]));
        assert!(!ring.write(4, &vec![0.0; SLOT_SAMPLES + 2]));
        assert!(ring.write(4, &vec![0.0; SLOT_SAMPLES]));
        // A never-written block reads as missing, including the one at u64::MAX.
        assert!(!ring.read(u64::MAX, &mut out));
    }

    #[test]
    fn an_overwritten_slot_reads_as_missing() {
        let ring = Box::<BlockRing>::default();
        assert!(ring.write(1, &block(0.0, 16)));
        assert!(ring.write(1 + SLOTS as u64, &block(100.0, 16)));
        let mut out = vec![0.0; 32];
        assert!(!ring.read(1, &mut out));
        assert!(ring.read(1 + SLOTS as u64, &mut out));
        assert_eq!(out[0], 100.0);
    }

    #[test]
    fn clear_empties_it() {
        let ring = Box::<BlockRing>::default();
        assert!(ring.write(0, &block(0.0, 16)));
        ring.clear();
        assert_eq!(ring.head(), 0);
        assert!(!ring.read(0, &mut [0.0; 32]));
    }

    #[test]
    fn first_useful_trails_the_head_by_the_depth() {
        assert_eq!(first_useful(0, 2), 0);
        assert_eq!(first_useful(1, 2), 0);
        assert_eq!(first_useful(10, 2), 8);
    }

    /// A reader racing a writer that keeps rewriting the same slot never accepts a mixed
    /// block: every accepted copy is one block's samples throughout.
    #[test]
    fn a_racing_reader_never_accepts_a_torn_block() {
        let ring: Arc<BlockRing> = Arc::from(Box::<BlockRing>::default());
        let done = Arc::new(AtomicBool::new(false));
        let frames = 512;
        let writer = {
            let ring = Arc::clone(&ring);
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut n = 0u64;
                while !done.load(Ordering::Relaxed) {
                    // Every sample of block n is n, always in slot 0.
                    let value = n as f32;
                    ring.write(n * SLOTS as u64, &vec![value; frames * CHANNELS]);
                    n += 1;
                }
            })
        };
        let mut out = vec![0.0; frames * CHANNELS];
        let mut accepted = 0;
        for _ in 0..20_000 {
            let head = ring.head();
            if head == 0 {
                continue;
            }
            if ring.read(head - 1, &mut out) {
                accepted += 1;
                assert!(
                    out.iter().all(|&s| s == out[0]),
                    "a torn block was accepted"
                );
                assert_eq!(out[0], ((head - 1) / SLOTS as u64) as f32);
            }
        }
        done.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        // Not asserted > 0: on a single core the writer may always win. The property is
        // that nothing accepted was torn.
        let _ = accepted;
    }
}
