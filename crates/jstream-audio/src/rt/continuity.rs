//! Where each packet sits in an input's device-position sequence. Ported from spike S3
//! (`StreamClock` in `spikes/s3-clock/src/clock.rs`), with clock values in seconds on the
//! media clock instead of WASAPI's 100 ns ticks.
//!
//! A device position counts frames since the stream started, including frames lost in
//! a glitch (docs: `jstream_platform::AudioPacket::device_position`), so a forward jump
//! is the number of missing frames. A gap is only trusted when the packets' clock times
//! moved by the same amount, within [`GAP_TOLERANCE_S`]; otherwise the position and the
//! clock no longer agree and the mapping restarts.

/// The largest gap filled with silence, in seconds of device time; anything larger
/// restarts the mapping.
pub(crate) const MAX_GAP_S: u32 = 2;
/// How far a gap's clock jump may differ from its frame count, in seconds.
pub(crate) const GAP_TOLERANCE_S: f64 = 0.005;

/// Why a stream's position mapping restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetReason {
    /// The device position went backwards.
    Backwards,
    /// The position jumped further than [`MAX_GAP_S`].
    GapTooLarge,
    /// The position jumped, but the clock did not move by the same amount.
    GapTimeMismatch,
}

/// How a packet's device position relates to the previous packet's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Continuity {
    /// The first packet of the stream.
    First,
    /// Starts exactly where the previous packet ended.
    Continuous,
    /// Starts `n` frames after the previous packet ended (frames were lost), and the
    /// clock agrees: the caller fills `n` frames of silence and keeps the mapping.
    Gap(u64),
    /// The mapping cannot be continued: the caller starts over.
    Reset(ResetReason),
}

/// Tracks device positions and the last timestamp of one input.
#[derive(Clone, Debug)]
pub(crate) struct PositionTracker {
    rate: f64,
    max_gap_frames: u64,
    /// Device position just after the previous packet.
    next_pos: Option<u64>,
    /// The last packet with a usable timestamp: (device position, clock seconds).
    last_stamp: Option<(u64, f64)>,
}

impl PositionTracker {
    pub(crate) fn new(rate: u32) -> Self {
        Self {
            rate: f64::from(rate),
            max_gap_frames: u64::from(rate) * u64::from(MAX_GAP_S),
            next_pos: None,
            last_stamp: None,
        }
    }

    /// Classifies a packet of `frames` frames at device position `pos`, stamped `time`
    /// (clock seconds; `None` when unreliable), and advances the bookkeeping.
    pub(crate) fn classify(&mut self, pos: u64, frames: u64, time: Option<f64>) -> Continuity {
        let result = match self.next_pos {
            None => Continuity::First,
            Some(next) if pos == next => Continuity::Continuous,
            Some(next) if pos < next => Continuity::Reset(ResetReason::Backwards),
            Some(next) => {
                let gap = pos - next;
                if gap > self.max_gap_frames {
                    Continuity::Reset(ResetReason::GapTooLarge)
                } else if !self.gap_matches_clock(pos, time) {
                    Continuity::Reset(ResetReason::GapTimeMismatch)
                } else {
                    Continuity::Gap(gap)
                }
            }
        };
        if matches!(result, Continuity::Reset(_)) {
            self.last_stamp = None;
        }
        self.next_pos = Some(pos.saturating_add(frames));
        if let Some(t) = time {
            self.last_stamp = Some((pos, t));
        }
        result
    }

    /// Whether the clock time since the last stamped packet matches the frames since
    /// it. Without two timestamps nothing contradicts the position, so it passes.
    fn gap_matches_clock(&self, pos: u64, time: Option<f64>) -> bool {
        let (Some((p0, t0)), Some(t1)) = (self.last_stamp, time) else {
            return true;
        };
        let expected = pos.saturating_sub(p0) as f64 / self.rate;
        ((t1 - t0) - expected).abs() <= GAP_TOLERANCE_S
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_packets() {
        let mut c = PositionTracker::new(48_000);
        assert_eq!(c.classify(0, 480, Some(0.0)), Continuity::First);
        assert_eq!(c.classify(480, 480, Some(0.01)), Continuity::Continuous);
        // A missing timestamp doesn't affect continuity.
        assert_eq!(c.classify(960, 480, None), Continuity::Continuous);
    }

    #[test]
    fn gap_consistent_with_clock_is_filled() {
        let mut c = PositionTracker::new(48_000);
        c.classify(1000, 480, Some(1.0));
        // 960 frames (20 ms) are missing; the clock agrees (+30 ms since the last stamp).
        let next = 1000 + 480 + 960;
        assert_eq!(c.classify(next, 480, Some(1.03)), Continuity::Gap(960));
        // Without a timestamp nothing contradicts the position.
        assert_eq!(c.classify(next + 480 + 48, 480, None), Continuity::Gap(48));
    }

    #[test]
    fn gap_that_disagrees_with_clock_resets() {
        let mut c = PositionTracker::new(48_000);
        c.classify(0, 480, Some(0.0));
        // Position says 20 ms later, the clock says 2 s later.
        assert_eq!(
            c.classify(960, 480, Some(2.0)),
            Continuity::Reset(ResetReason::GapTimeMismatch)
        );
        // After a reset the next packet continues from the new position.
        assert_eq!(c.classify(1440, 480, Some(2.01)), Continuity::Continuous);
    }

    #[test]
    fn backwards_and_huge_gaps_reset() {
        let mut c = PositionTracker::new(48_000);
        c.classify(48_000, 480, Some(0.0));
        assert_eq!(
            c.classify(0, 480, Some(0.01)),
            Continuity::Reset(ResetReason::Backwards)
        );
        assert_eq!(
            c.classify(480 + 48_000 * 3, 480, None),
            Continuity::Reset(ResetReason::GapTooLarge)
        );
        // Positions near u64::MAX saturate instead of wrapping.
        assert_eq!(
            c.classify(u64::MAX - 10, 480, None),
            Continuity::Reset(ResetReason::GapTooLarge)
        );
        assert_eq!(c.classify(u64::MAX, 1, None), Continuity::Continuous);
    }
}
