//! The QPC media clock and the per-stream sample-position bookkeeping.
//!
//! The media clock (ADR-0014) is QPC. WASAPI already reports each packet's QPC time in
//! 100 ns units (`pu64QPCPosition`), so the spike carries every clock value as signed
//! 100 ns ticks ("hns"), the same unit `IAudioClient` uses for durations. Raw counter
//! readings (`QueryPerformanceCounter`) are converted with [`qpc_to_hns`], exactly as the
//! audio engine does, so both kinds of reading share one time line.
//!
//! This mirrors the minimal part of `jstream_media::time` (exact integer conversion
//! between time bases); the spike is standalone and does not depend on the crate.

/// 100 ns ticks per second.
pub const HNS_PER_SEC: i64 = 10_000_000;

/// Converts a raw QPC reading to 100 ns ticks, rounding toward zero.
///
/// `ticks * 10^7 / freq` in 128-bit arithmetic, so it cannot overflow for any `i64`
/// reading or frequency. Returns `None` for a non-positive frequency.
pub fn qpc_to_hns(ticks: i64, freq: i64) -> Option<i64> {
    if freq <= 0 {
        return None;
    }
    let v = i128::from(ticks) * i128::from(HNS_PER_SEC) / i128::from(freq);
    i64::try_from(v).ok()
}

/// The duration of `frames` sample frames at `rate` Hz, in 100 ns ticks, rounded to
/// the nearest tick.
pub fn frames_to_hns(frames: i64, rate: u32) -> i64 {
    if rate == 0 {
        return 0;
    }
    let r = i128::from(rate);
    let n = i128::from(frames) * i128::from(HNS_PER_SEC);
    // Round half away from zero.
    let q = if n >= 0 {
        (n + r / 2) / r
    } else {
        (n - r / 2) / r
    };
    i64::try_from(q).unwrap_or(if n >= 0 { i64::MAX } else { i64::MIN })
}

/// A duration in 100 ns ticks as seconds.
pub fn hns_to_sec(hns: i64) -> f64 {
    hns as f64 / HNS_PER_SEC as f64
}

/// The `AUDCLNT_BUFFERFLAGS_*` bits of one `IAudioCaptureClient::GetBuffer` packet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PacketFlags {
    /// `DATA_DISCONTINUITY`: the data is not continuous with the previous packet.
    pub discontinuity: bool,
    /// `SILENT`: treat the packet as silence; the buffer contents are undefined.
    pub silent: bool,
    /// `TIMESTAMP_ERROR`: the device position or QPC time could not be recorded.
    pub timestamp_error: bool,
}

impl PacketFlags {
    pub const DATA_DISCONTINUITY: u32 = 0x1;
    pub const SILENT: u32 = 0x2;
    pub const TIMESTAMP_ERROR: u32 = 0x4;

    pub fn from_bits(bits: u32) -> Self {
        Self {
            discontinuity: bits & Self::DATA_DISCONTINUITY != 0,
            silent: bits & Self::SILENT != 0,
            timestamp_error: bits & Self::TIMESTAMP_ERROR != 0,
        }
    }
}

/// Why a stream's position mapping was restarted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    /// The device position went backwards.
    Backwards,
    /// The position jumped further than [`StreamClock::max_gap_frames`].
    GapTooLarge,
    /// The position jumped, but the QPC time did not move by the same amount.
    GapTimeMismatch,
}

/// How a packet's device position relates to the previous packet's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Continuity {
    /// The first packet of the stream.
    First,
    /// Starts exactly where the previous packet ended.
    Continuous,
    /// Starts `n` frames after the previous packet ended (frames were lost), and the QPC
    /// time agrees, so the caller fills `n` frames of silence and keeps the mapping.
    Gap(u64),
    /// The mapping cannot be continued: the caller starts a new segment.
    Reset(ResetReason),
}

/// Tracks where each packet sits in a stream's sample-position sequence.
///
/// WASAPI's device position counts frames since the stream started, including frames
/// that were lost in a glitch, so a gap in the position is the number of missing frames.
/// A gap is only trusted when the packets' QPC times moved by the same amount
/// (within [`StreamClock::gap_tolerance_hns`]); otherwise the position and the clock no
/// longer agree and the mapping restarts.
#[derive(Clone, Debug)]
pub struct StreamClock {
    rate: u32,
    /// The largest gap filled with silence; anything larger restarts the mapping.
    pub max_gap_frames: u64,
    /// How far a gap's QPC jump may differ from its frame count.
    pub gap_tolerance_hns: i64,
    /// Device position just after the previous packet.
    next_pos: Option<u64>,
    /// The last packet with a usable timestamp: (device position, QPC hns).
    last_stamp: Option<(u64, i64)>,
}

impl StreamClock {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            max_gap_frames: u64::from(rate) * 2,
            gap_tolerance_hns: 5 * HNS_PER_SEC / 1000,
            next_pos: None,
            last_stamp: None,
        }
    }

    /// Classifies a packet of `frames` frames at `pos`, stamped `qpc_hns` (`None` when
    /// the timestamp is flagged unreliable), and advances the bookkeeping.
    pub fn classify(&mut self, pos: u64, frames: u32, qpc_hns: Option<i64>) -> Continuity {
        let result = match self.next_pos {
            None => Continuity::First,
            Some(next) if pos == next => Continuity::Continuous,
            Some(next) if pos < next => Continuity::Reset(ResetReason::Backwards),
            Some(next) => {
                let gap = pos - next;
                if gap > self.max_gap_frames {
                    Continuity::Reset(ResetReason::GapTooLarge)
                } else if !self.gap_matches_clock(pos, qpc_hns) {
                    Continuity::Reset(ResetReason::GapTimeMismatch)
                } else {
                    Continuity::Gap(gap)
                }
            }
        };
        if matches!(result, Continuity::Reset(_)) {
            self.last_stamp = None;
        }
        self.next_pos = Some(pos.saturating_add(u64::from(frames)));
        if let Some(t) = qpc_hns {
            self.last_stamp = Some((pos, t));
        }
        result
    }

    /// Whether the QPC time since the last stamped packet matches the frames since it.
    /// Without two timestamps there is nothing to contradict the position, so it passes.
    fn gap_matches_clock(&self, pos: u64, qpc_hns: Option<i64>) -> bool {
        let (Some((p0, t0)), Some(t1)) = (self.last_stamp, qpc_hns) else {
            return true;
        };
        let frames = i64::try_from(pos.saturating_sub(p0)).unwrap_or(i64::MAX);
        let expected = frames_to_hns(frames, self.rate);
        let actual = t1.saturating_sub(t0);
        (actual - expected).abs() <= self.gap_tolerance_hns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_conversion_is_exact_and_does_not_overflow() {
        // The common 10 MHz QPC: one tick is one hns.
        assert_eq!(qpc_to_hns(123_456_789, 10_000_000), Some(123_456_789));
        // TSC-derived frequencies are not multiples of 10 MHz.
        assert_eq!(qpc_to_hns(3_579_545, 3_579_545), Some(HNS_PER_SEC));
        assert_eq!(qpc_to_hns(1, 3_579_545), Some(2)); // 2.79 rounds toward zero
        // A week of uptime at 3 GHz would overflow ticks * 10^7 in 64 bits.
        let week = 7 * 24 * 3600_i64;
        let freq = 3_000_000_000_i64;
        assert_eq!(qpc_to_hns(week * freq, freq), Some(week * HNS_PER_SEC));
        assert_eq!(qpc_to_hns(1, 0), None);
    }

    #[test]
    fn frames_round_to_nearest_tick() {
        assert_eq!(frames_to_hns(48_000, 48_000), HNS_PER_SEC);
        assert_eq!(frames_to_hns(480, 48_000), 100_000); // 10 ms
        // One frame at 44.1 kHz is 226.757 hns.
        assert_eq!(frames_to_hns(1, 44_100), 227);
        assert_eq!(frames_to_hns(-1, 44_100), -227);
        assert_eq!(frames_to_hns(5, 0), 0);
        assert!((hns_to_sec(15_000_000) - 1.5).abs() < 1e-12);
    }

    #[test]
    fn flags_decode() {
        let f = PacketFlags::from_bits(0x5);
        assert!(f.discontinuity && f.timestamp_error && !f.silent);
        assert_eq!(PacketFlags::from_bits(0), PacketFlags::default());
    }

    #[test]
    fn continuous_packets() {
        let mut c = StreamClock::new(48_000);
        assert_eq!(c.classify(0, 480, Some(0)), Continuity::First);
        assert_eq!(c.classify(480, 480, Some(100_000)), Continuity::Continuous);
        // A missing timestamp doesn't affect continuity.
        assert_eq!(c.classify(960, 480, None), Continuity::Continuous);
    }

    #[test]
    fn gap_consistent_with_clock_is_filled() {
        let mut c = StreamClock::new(48_000);
        c.classify(1000, 480, Some(10_000_000));
        // 960 frames (20 ms) are missing; the QPC time agrees (+30 ms since the last stamp).
        let next = 1000 + 480 + 960;
        assert_eq!(
            c.classify(next, 480, Some(10_000_000 + 300_000)),
            Continuity::Gap(960)
        );
        // Without a timestamp nothing contradicts the position.
        assert_eq!(c.classify(next + 480 + 48, 480, None), Continuity::Gap(48));
    }

    #[test]
    fn gap_that_disagrees_with_clock_resets() {
        let mut c = StreamClock::new(48_000);
        c.classify(0, 480, Some(0));
        // Position says 20 ms later, QPC says 2 s later.
        assert_eq!(
            c.classify(960, 480, Some(20_000_000)),
            Continuity::Reset(ResetReason::GapTimeMismatch)
        );
        // After a reset the next packet continues from the new position.
        assert_eq!(
            c.classify(1440, 480, Some(20_100_000)),
            Continuity::Continuous
        );
    }

    #[test]
    fn backwards_and_huge_gaps_reset() {
        let mut c = StreamClock::new(48_000);
        c.classify(48_000, 480, Some(0));
        assert_eq!(
            c.classify(0, 480, Some(100_000)),
            Continuity::Reset(ResetReason::Backwards)
        );
        assert_eq!(
            c.classify(480 + 48_000 * 3, 480, None),
            Continuity::Reset(ResetReason::GapTooLarge)
        );
    }
}
