//! The single-producer, single-consumer ring that carries one input's resampled stereo
//! frames from its audio thread to the mixer.
//!
//! **Indexed by engine time.** Every frame is stored under its absolute engine frame
//! index `n` (the frame at media time `n / engine_rate`), not under a queue position. The
//! producer only appends (`n` never goes backwards); it may jump forward, and the frames it
//! jumps over read as silence. The mixer reads whatever window `[n, n + len)` its clock
//! asks for. That makes the policies fall out of the indexing:
//!
//! - **Underrun:** frames the producer hasn't written yet read as silence, and the read
//!   reports how many frames were valid.
//! - **Overrun, drop oldest:** the producer never waits. It overwrites the slot of the
//!   frame `capacity` frames back, whether or not the mixer read it, and counts the
//!   frames it overwrote unread.
//! - **Late data:** frames written behind the mixer's read position are counted and
//!   never read.
//!
//! **Why atomics and no `unsafe`.** The crate forbids `unsafe` (R-UNSAFE-001), so the
//! slots are `AtomicU32`s holding `f32` bits, accessed with `Relaxed` loads and stores
//! (plain moves on x86-64 and AArch64). A torn read is therefore never undefined behaviour,
//! only stale data, and a seqlock-style check catches that: before overwriting, the
//! producer publishes `claim`, the end of the write it is about to make, then issues a
//! release fence; the reader copies, issues an acquire fence and loads `claim`. If the
//! copy observed any overwritten slot, the fences guarantee it also observes the claim,
//! and every frame below `claim - capacity` is discarded as overwritten. No workspace
//! crate offers such a ring, and a general-purpose queue crate would neither be indexed
//! by time nor drop the oldest data, so it is written here (R-DEP-004).
//!
//! Neither side allocates, locks or blocks after [`FrameRing::new`] (R-RT-001).

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering, fence};
#[cfg(test)]
use std::sync::mpsc::Sender;
#[cfg(test)]
use std::sync::{Mutex, PoisonError};
#[cfg(test)]
use std::thread::ThreadId;

/// Marks a position that has not been set.
const UNSET: i64 = i64::MIN;

/// The shared ring. Create one per input; hand a [`RingWriter`] to the producer.
#[derive(Debug)]
pub(crate) struct FrameRing {
    /// `2 * capacity` interleaved samples as `f32` bits.
    slots: Box<[AtomicU32]>,
    capacity: i64,
    mask: u64,
    /// The first frame the producer ever wrote, or [`UNSET`]. Written once, before the
    /// first `written` store.
    start: AtomicI64,
    /// The end of the write in progress (or the last one): frames below
    /// `claim - capacity` may already be overwritten.
    claim: AtomicI64,
    /// The end of the last completed write: frames in
    /// `[max(start, written - capacity), written)` are readable.
    written: AtomicI64,
    /// The consumer's next read position, for the producer's late and overrun counts.
    read_pos: AtomicI64,
    /// Receives the thread id that frees the ring. Tests only.
    #[cfg(test)]
    drop_notify: Mutex<Option<Sender<ThreadId>>>,
}

#[cfg(test)]
fn lock_notify(
    mutex: &Mutex<Option<Sender<ThreadId>>>,
) -> std::sync::MutexGuard<'_, Option<Sender<ThreadId>>> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What a write did, for the producer's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WriteReport {
    /// Frames written behind the consumer's read position: never read.
    pub(crate) late: u64,
    /// Frames the consumer had not read yet that this write overwrote (dropped oldest).
    pub(crate) overwritten: u64,
}

impl WriteReport {
    fn add(&mut self, other: Self) {
        self.late += other.late;
        self.overwritten += other.overwritten;
    }
}

/// What a read found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadReport {
    /// Whether the producer has written anything yet.
    pub(crate) started: bool,
    /// Frames of the window that held data (the rest are silence).
    pub(crate) valid: usize,
}

impl FrameRing {
    /// A ring of `capacity` stereo frames. `capacity` must be a power of two of at
    /// least 2; the caller (mixer configuration) checks it.
    pub(crate) fn new(capacity: usize) -> Self {
        debug_assert!(capacity.is_power_of_two() && capacity >= 2);
        let slots = (0..capacity * 2).map(|_| AtomicU32::new(0)).collect();
        Self {
            slots,
            capacity: i64::try_from(capacity).unwrap_or(i64::MAX),
            mask: capacity as u64 - 1,
            start: AtomicI64::new(UNSET),
            claim: AtomicI64::new(UNSET),
            written: AtomicI64::new(UNSET),
            read_pos: AtomicI64::new(UNSET),
            #[cfg(test)]
            drop_notify: Mutex::new(None),
        }
    }

    /// Sends the freeing thread's id to `tx` when this ring is dropped.
    #[cfg(test)]
    pub(crate) fn on_drop(&self, tx: Sender<ThreadId>) {
        *lock_notify(&self.drop_notify) = Some(tx);
    }

    fn slot(&self, pos: i64) -> usize {
        // Two's complement: `pos & mask` is `pos mod capacity` for negative `pos` too.
        ((pos as u64 & self.mask) as usize) * 2
    }

    /// Reads frames `[pos, pos + out.len())` into `out`; frames without data are zero.
    pub(crate) fn read(&self, pos: i64, out: &mut [[f32; 2]]) -> ReadReport {
        out.fill([0.0; 2]);
        let written = self.written.load(Ordering::Acquire);
        if written == UNSET {
            return ReadReport::default();
        }
        let start = self.start.load(Ordering::Relaxed);
        let len = i64::try_from(out.len()).unwrap_or(i64::MAX);
        let end = pos.saturating_add(len);
        let lo = pos.max(start).max(written.saturating_sub(self.capacity));
        let hi = end.min(written);
        if lo >= hi {
            return ReadReport {
                started: true,
                valid: 0,
            };
        }
        for n in lo..hi {
            let s = self.slot(n);
            let i = (n - pos) as usize;
            out[i] = [
                f32::from_bits(self.slots[s].load(Ordering::Relaxed)),
                f32::from_bits(self.slots[s + 1].load(Ordering::Relaxed)),
            ];
        }
        // Seqlock check: anything below `claim - capacity` may have been overwritten
        // while we copied it.
        fence(Ordering::Acquire);
        let safe_lo = self
            .claim
            .load(Ordering::Relaxed)
            .saturating_sub(self.capacity);
        let mut first_valid = lo;
        if safe_lo > lo {
            let torn_hi = safe_lo.min(hi);
            for n in lo..torn_hi {
                out[(n - pos) as usize] = [0.0; 2];
            }
            first_valid = torn_hi;
        }
        ReadReport {
            started: true,
            valid: (hi - first_valid).max(0) as usize,
        }
    }

    /// Records the consumer's next read position.
    pub(crate) fn set_read_pos(&self, pos: i64) {
        self.read_pos.store(pos, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Drop for FrameRing {
    fn drop(&mut self) {
        if let Some(tx) = lock_notify(&self.drop_notify).take() {
            let _ = tx.send(std::thread::current().id());
        }
    }
}

/// The producer's end of a [`FrameRing`]. Exactly one exists per ring.
#[derive(Debug)]
pub(crate) struct RingWriter {
    ring: std::sync::Arc<FrameRing>,
    /// The next frame to write, once started.
    pos: Option<i64>,
}

impl RingWriter {
    pub(crate) fn new(ring: std::sync::Arc<FrameRing>) -> Self {
        Self { ring, pos: None }
    }

    /// Releases the ring to its caller, which retires it onto the collector thread.
    pub(crate) fn into_ring(self) -> std::sync::Arc<FrameRing> {
        self.ring
    }

    #[cfg(test)]
    pub(crate) fn ring(&self) -> &FrameRing {
        &self.ring
    }

    /// The next frame this writer writes, once it has started.
    pub(crate) fn position(&self) -> Option<i64> {
        self.pos
    }

    /// Starts writing at `pos`, or, if already started, jumps forward to `pos`: the
    /// frames in between read as silence. A `pos` behind the current position is ignored
    /// (the ring only appends).
    pub(crate) fn seek(&mut self, pos: i64) -> WriteReport {
        match self.pos {
            None => {
                self.ring.start.store(pos, Ordering::Relaxed);
                self.pos = Some(pos);
                WriteReport::default()
            }
            Some(cur) if pos > cur => self.fill_silence(pos.saturating_sub(cur).unsigned_abs()),
            Some(_) => WriteReport::default(),
        }
    }

    /// Appends `frames` frames of silence. Only the last `capacity` of them touch
    /// memory, so the work is bounded however long the gap is.
    pub(crate) fn fill_silence(&mut self, frames: u64) -> WriteReport {
        let Some(cur) = self.pos else {
            return WriteReport::default();
        };
        let n = i64::try_from(frames).unwrap_or(i64::MAX);
        let end = cur.saturating_add(n);
        let from = cur.max(end.saturating_sub(self.ring.capacity));
        self.commit(cur, end, |ring| {
            for p in from..end {
                let s = ring.slot(p);
                ring.slots[s].store(0, Ordering::Relaxed);
                ring.slots[s + 1].store(0, Ordering::Relaxed);
            }
        })
    }

    /// Appends `frames`. Writes longer than the ring keep only their last `capacity`
    /// frames.
    pub(crate) fn write(&mut self, frames: &[[f32; 2]]) -> WriteReport {
        let Some(cur) = self.pos else {
            return WriteReport::default();
        };
        if frames.is_empty() {
            return WriteReport::default();
        }
        let mut report = WriteReport::default();
        let cap = usize::try_from(self.ring.capacity).unwrap_or(usize::MAX);
        let mut pos = cur;
        for chunk in frames.chunks(cap) {
            let end = pos.saturating_add(chunk.len() as i64);
            report.add(self.commit(pos, end, |ring| {
                for (i, f) in chunk.iter().enumerate() {
                    let s = ring.slot(pos + i as i64);
                    ring.slots[s].store(f[0].to_bits(), Ordering::Relaxed);
                    ring.slots[s + 1].store(f[1].to_bits(), Ordering::Relaxed);
                }
            }));
            pos = end;
        }
        report
    }

    /// Publishes a write of `[cur, end)` around `store`, which fills the slots.
    fn commit(&mut self, cur: i64, end: i64, store: impl FnOnce(&FrameRing)) -> WriteReport {
        let ring = &*self.ring;
        let report = self.report(cur, end);
        ring.claim.store(end, Ordering::Relaxed);
        fence(Ordering::Release);
        store(ring);
        ring.written.store(end, Ordering::Release);
        self.pos = Some(end);
        report
    }

    /// Counts what writing `[cur, end)` does to frames the consumer hasn't read.
    fn report(&self, cur: i64, end: i64) -> WriteReport {
        let ring = &*self.ring;
        let read_pos = ring.read_pos.load(Ordering::Relaxed);
        if read_pos == UNSET {
            return WriteReport::default();
        }
        let late = read_pos.min(end).saturating_sub(cur).max(0).unsigned_abs();
        let start = ring.start.load(Ordering::Relaxed);
        // Slots for `[cur, end)` held frames `[cur - cap, end - cap)`; the ones that were
        // written (below `cur`, at or above `start`) and not read (at or above
        // `read_pos`) are lost.
        let lost_lo = cur.saturating_sub(ring.capacity).max(read_pos).max(start);
        let lost_hi = end.saturating_sub(ring.capacity).min(cur);
        let overwritten = lost_hi.saturating_sub(lost_lo).max(0).unsigned_abs();
        WriteReport { late, overwritten }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn frames(from: i64, n: usize) -> Vec<[f32; 2]> {
        (0..n)
            .map(|i| {
                let v = (from + i as i64) as f32;
                [v, -v]
            })
            .collect()
    }

    fn ring(cap: usize) -> (Arc<FrameRing>, RingWriter) {
        let r = Arc::new(FrameRing::new(cap));
        let w = RingWriter::new(Arc::clone(&r));
        (r, w)
    }

    #[test]
    fn nothing_written_reads_silence_and_not_started() {
        let (r, mut w) = ring(16);
        let mut out = [[1.0; 2]; 4];
        assert_eq!(r.read(0, &mut out), ReadReport::default());
        assert_eq!(out, [[0.0; 2]; 4]);
        // Writing before a seek is ignored.
        assert_eq!(w.write(&frames(0, 4)), WriteReport::default());
        assert_eq!(w.position(), None);
    }

    #[test]
    fn reads_what_was_written_at_its_engine_position() {
        let (r, mut w) = ring(16);
        w.seek(100);
        w.write(&frames(100, 8));
        let mut out = [[9.0; 2]; 6];
        // A window straddling the start: frames before 100 are silence.
        let rep = r.read(97, &mut out);
        assert_eq!(rep.valid, 3);
        assert_eq!(out[0..3], [[0.0; 2]; 3]);
        assert_eq!(out[3], [100.0, -100.0]);
        assert_eq!(out[5], [102.0, -102.0]);
        // A window past the end: underrun.
        let rep = r.read(106, &mut out);
        assert_eq!(rep.valid, 2);
        assert_eq!(out[1], [107.0, -107.0]);
        assert_eq!(out[2..], [[0.0; 2]; 4]);
    }

    #[test]
    fn negative_positions_and_wrap_around() {
        let (r, mut w) = ring(8);
        w.seek(-20);
        for k in 0..10 {
            w.write(&frames(-20 + 5 * k, 5));
        }
        // Written up to 30; the last 8 frames are readable.
        let mut out = [[0.0; 2]; 8];
        assert_eq!(r.read(22, &mut out).valid, 8);
        assert_eq!(out, frames(22, 8)[..]);
        // Older frames were overwritten.
        let mut out = [[0.0; 2]; 4];
        assert_eq!(r.read(18, &mut out).valid, 0);
    }

    #[test]
    fn overrun_drops_the_oldest_and_counts_unread_frames() {
        let (r, mut w) = ring(8);
        w.seek(0);
        r.set_read_pos(0);
        assert_eq!(w.write(&frames(0, 8)).overwritten, 0);
        // The consumer read nothing: writing 3 more overwrites frames 0..3.
        assert_eq!(w.write(&frames(8, 3)).overwritten, 3);
        // The consumer moved to 6: overwriting frames 3..6, which it has passed, loses
        // nothing.
        r.set_read_pos(6);
        assert_eq!(w.write(&frames(11, 3)).overwritten, 0);
        // Frames 6..9 are unread; a write of 3 more overwrites them.
        assert_eq!(w.write(&frames(14, 3)).overwritten, 3);
        // One write longer than the ring keeps only its tail.
        let rep = w.write(&frames(17, 20));
        assert_eq!(rep.overwritten, 8 + 12);
        let mut out = [[0.0; 2]; 8];
        assert_eq!(r.read(29, &mut out).valid, 8);
        assert_eq!(out, frames(29, 8)[..]);
    }

    #[test]
    fn late_frames_are_counted() {
        let (r, mut w) = ring(16);
        w.seek(0);
        r.set_read_pos(5);
        assert_eq!(w.write(&frames(0, 8)).late, 5);
        assert_eq!(w.write(&frames(8, 8)).late, 0);
    }

    #[test]
    fn a_forward_seek_reads_as_silence_even_over_old_data() {
        let (r, mut w) = ring(8);
        w.seek(0);
        w.write(&frames(0, 8));
        // Jump 5 frames: frames 8..13 are silence, not the stale 0..5.
        w.seek(13);
        w.write(&frames(13, 2));
        let mut out = [[9.0; 2]; 8];
        assert_eq!(r.read(7, &mut out).valid, 8);
        assert_eq!(out[0], [7.0, -7.0]);
        assert_eq!(out[1..6], [[0.0; 2]; 5]);
        assert_eq!(out[6], [13.0, -13.0]);
        // A huge jump touches at most `capacity` slots, and everything reads as silence.
        w.seek(1_000_000_000);
        assert_eq!(w.position(), Some(1_000_000_000));
        let mut out = [[9.0; 2]; 8];
        assert_eq!(r.read(1_000_000_000 - 8, &mut out).valid, 8);
        assert_eq!(out, [[0.0; 2]; 8]);
        // Seeking backwards is ignored.
        w.seek(5);
        assert_eq!(w.position(), Some(1_000_000_000));
    }

    /// A producer thread writes frames whose value is their position, as fast as it
    /// can, through a ring far too small for the consumer to keep up; every frame the
    /// consumer accepts as valid must hold its own position. Without the claim check,
    /// torn copies would show up as mismatches.
    #[test]
    fn concurrent_reads_never_return_torn_frames() {
        let (r, mut w) = ring(64);
        w.seek(0);
        let writer = std::thread::spawn(move || {
            let mut buf = [[0.0f32; 2]; 7];
            let mut pos = 0i64;
            while pos < 2_000_000 {
                for (i, f) in buf.iter_mut().enumerate() {
                    let v = (pos + i as i64) as f32;
                    *f = [v, v];
                }
                w.write(&buf);
                pos += 7;
            }
        });
        let mut out = [[0.0f32; 2]; 48];
        let mut checked = 0u64;
        while !writer.is_finished() {
            let written = r.written.load(Ordering::Acquire);
            if written == UNSET {
                continue;
            }
            let pos = written - 40;
            let rep = r.read(pos, &mut out);
            // Every frame is either silence (no valid data) or the frame at its own
            // position; a torn copy would hold a frame from an earlier lap.
            let mut nonzero = 0;
            for (i, f) in out.iter().enumerate() {
                if *f != [0.0; 2] {
                    let want = (pos + i as i64) as f32;
                    assert_eq!(*f, [want, want], "torn frame at {}", pos + i as i64);
                    nonzero += 1;
                }
            }
            assert!(nonzero <= rep.valid);
            checked += nonzero as u64;
        }
        writer.join().expect("writer thread");
        assert!(checked > 0);
    }
}
