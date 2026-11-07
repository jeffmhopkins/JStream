//! Telling new frames from repeats, and counting what went missing.

/// What a frame's sequence number says about it, from [`SeqTracker::frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeqVerdict {
    /// Not seen before: play it. `restarted` is set for the first frame of a stream (a
    /// new stream id: the sender started or restarted).
    Fresh {
        /// The stream is new.
        restarted: bool,
    },
    /// Already seen: the redundant copy of a frame, or a datagram the network repeated.
    Repeat,
    /// Older than the tracker remembers: too late to be anything but dropped.
    Stale,
}

/// How many frames back the tracker remembers which it has seen.
pub const SEQ_WINDOW: u32 = 64;
/// A jump further ahead than this is taken as a restart of the stream with the same id.
const MAX_JUMP: i32 = 1 << 16;

/// Tracks one sender's frame sequence numbers, which wrap at 2^32.
///
/// It remembers the newest frame and which of the [`SEQ_WINDOW`] before it have arrived.
/// A frame that leaves the window without arriving is counted as missing.
#[derive(Clone, Debug, Default)]
pub struct SeqTracker {
    stream: Option<u32>,
    newest: u32,
    /// Bit i set: frame `newest - i` has arrived (bit 0 is the newest).
    seen: u64,
    /// Bit i set: frame `newest - i` belongs to the stream (it isn't before its first
    /// frame), so its absence counts.
    span: u64,
    missing: u64,
    repeats: u64,
}

impl SeqTracker {
    /// A tracker that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one frame of `stream_id` with sequence number `seq`. Give a packet's frames
    /// oldest first (the redundant copy before the new frame).
    pub fn frame(&mut self, stream_id: u32, seq: u32) -> SeqVerdict {
        if self.stream != Some(stream_id) {
            self.stream = Some(stream_id);
            self.newest = seq;
            self.seen = 1;
            self.span = 1;
            return SeqVerdict::Fresh { restarted: true };
        }
        let ahead = seq.wrapping_sub(self.newest) as i32;
        if !(-MAX_JUMP..=MAX_JUMP).contains(&ahead) {
            self.stream = None;
            return self.frame(stream_id, seq);
        }
        if ahead > 0 {
            let shift = ahead as u32;
            // Frames pushed out of the window: those of the stream that never arrived
            // are missing.
            let (seen_out, span_out) = if shift >= SEQ_WINDOW {
                (self.seen, self.span)
            } else {
                (
                    self.seen >> (SEQ_WINDOW - shift),
                    self.span >> (SEQ_WINDOW - shift),
                )
            };
            self.missing += u64::from((span_out & !seen_out).count_ones());
            if shift > SEQ_WINDOW {
                // And the ones skipped over that never entered it.
                self.missing += u64::from(shift - SEQ_WINDOW);
            }
            if shift >= SEQ_WINDOW {
                self.seen = 1;
                self.span = u64::MAX;
            } else {
                self.seen = self.seen << shift | 1;
                self.span = self.span << shift | ((1 << shift) - 1);
            }
            self.newest = seq;
            return SeqVerdict::Fresh { restarted: false };
        }
        let back = (-ahead) as u32;
        if back >= SEQ_WINDOW {
            return SeqVerdict::Stale;
        }
        let bit = 1_u64 << back;
        if self.seen & bit != 0 {
            self.repeats += 1;
            return SeqVerdict::Repeat;
        }
        self.seen |= bit;
        SeqVerdict::Fresh { restarted: false }
    }

    /// Frames that left the window without arriving: lost on the network (a frame that
    /// arrives after leaving it is [`SeqVerdict::Stale`]).
    #[must_use]
    pub const fn missing(&self) -> u64 {
        self.missing
    }

    /// Frames that arrived more than once, redundant copies included.
    #[must_use]
    pub const fn repeats(&self) -> u64 {
        self.repeats
    }

    /// The newest sequence number seen, once a frame has arrived.
    #[must_use]
    pub fn newest(&self) -> Option<u32> {
        self.stream.map(|_| self.newest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRESH: SeqVerdict = SeqVerdict::Fresh { restarted: false };

    #[test]
    fn redundant_copies_are_repeats() {
        let mut t = SeqTracker::new();
        assert_eq!(t.frame(1, 10), SeqVerdict::Fresh { restarted: true });
        // Each packet: the previous frame again, then the new one.
        for seq in 11..20 {
            assert_eq!(t.frame(1, seq - 1), SeqVerdict::Repeat);
            assert_eq!(t.frame(1, seq), FRESH);
        }
        assert_eq!(t.repeats(), 9);
        assert_eq!(t.missing(), 0);
    }

    #[test]
    fn a_lost_packet_is_filled_by_the_next_packets_copy() {
        let mut t = SeqTracker::new();
        t.frame(1, 1);
        // Packet [2, 1] lost; packet [3, 2] brings frame 2 as its redundant copy.
        assert_eq!(t.frame(1, 2), FRESH);
        assert_eq!(t.frame(1, 3), FRESH);
        assert_eq!(t.missing(), 0);
    }

    #[test]
    fn frames_that_never_arrive_count_as_missing_once_they_leave_the_window() {
        let mut t = SeqTracker::new();
        t.frame(1, 100);
        t.frame(1, 103);
        assert_eq!(t.missing(), 0);
        // 101 arrives late but in the window: still fresh.
        assert_eq!(t.frame(1, 101), FRESH);
        t.frame(1, 103 + SEQ_WINDOW);
        // 102 left the window unseen.
        assert_eq!(t.missing(), 1);
        assert_eq!(t.frame(1, 102), SeqVerdict::Stale);
        // A jump far ahead counts everything skipped.
        t.frame(1, 103 + SEQ_WINDOW + 200);
        assert_eq!(t.missing(), 1 + 199);
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut t = SeqTracker::new();
        t.frame(1, u32::MAX - 1);
        assert_eq!(t.frame(1, u32::MAX), FRESH);
        assert_eq!(t.frame(1, 0), FRESH);
        assert_eq!(t.frame(1, u32::MAX), SeqVerdict::Repeat);
        assert_eq!(t.newest(), Some(0));
    }

    #[test]
    fn a_new_stream_id_or_a_wild_jump_is_a_restart() {
        let mut t = SeqTracker::new();
        t.frame(1, 5);
        assert_eq!(t.frame(2, 5), SeqVerdict::Fresh { restarted: true });
        assert_eq!(
            t.frame(2, 5 + (1 << 20)),
            SeqVerdict::Fresh { restarted: true }
        );
        assert_eq!(t.missing(), 0);
    }
}
