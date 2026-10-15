//! Pacing of the stream's samples: each `RequestSample` gets the next slot on a fixed
//! schedule at the stream's frame rate, so clients that ask faster than that get samples
//! no faster. Platform-independent, so its tests run on any host.

use std::time::{Duration, Instant};

/// Hands out one sample slot per frame period.
#[derive(Debug)]
pub struct Pacer {
    period: Duration,
    next: Option<Instant>,
}

impl Pacer {
    /// A pacer for `fps` samples per second (0 is treated as 1).
    pub fn new(fps: u32) -> Self {
        Self {
            period: Duration::from_secs(1) / fps.max(1),
            next: None,
        }
    }

    /// Reserves the next slot for a request made at `now`, and returns how long to wait
    /// before delivering it (zero: deliver now).
    ///
    /// Slots follow one schedule, so several outstanding requests are spread one period
    /// apart. A request that comes more than a period after the schedule's next slot (the
    /// client paused, or fell behind) restarts the schedule at `now` instead of bursting
    /// to catch up.
    pub fn reserve(&mut self, now: Instant) -> Duration {
        let due = match self.next {
            Some(next) if now.saturating_duration_since(next) <= self.period => next,
            _ => now,
        };
        self.next = Some(due + self.period);
        due.saturating_duration_since(now)
    }

    /// Forgets the schedule, so the next request is served at once (the stream restarted).
    pub fn reset(&mut self) {
        self.next = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Duration = Duration::from_nanos(33_333_333);

    #[test]
    fn first_request_is_immediate_and_the_next_waits_a_period() {
        let t0 = Instant::now();
        let mut p = Pacer::new(30);
        assert_eq!(p.reserve(t0), Duration::ZERO);
        assert_eq!(p.reserve(t0), P);
        // A third request at the same moment queues behind the second.
        assert_eq!(p.reserve(t0), P * 2);
    }

    #[test]
    fn a_client_asking_four_times_per_frame_gets_one_sample_per_period() {
        // 11124 requests in 90 s (the first hardware run) become ~30 per second.
        let t0 = Instant::now();
        let mut p = Pacer::new(30);
        let mut delivered = Vec::new();
        let mut now = t0;
        let mut pending = t0;
        while now < t0 + Duration::from_secs(3) {
            // The client asks again as soon as its previous sample arrives, or every 8 ms.
            now = now.max(pending) + Duration::from_millis(8);
            let wait = p.reserve(now);
            pending = now + wait;
            delivered.push(pending);
        }
        let in_window = delivered
            .iter()
            .filter(|&&d| d < t0 + Duration::from_secs(3))
            .count();
        assert!((88..=92).contains(&in_window), "{in_window}");
        for pair in delivered.windows(2) {
            assert!(pair[1] - pair[0] >= P - Duration::from_millis(1));
        }
    }

    #[test]
    fn a_late_request_within_a_period_keeps_the_cadence() {
        let t0 = Instant::now();
        let mut p = Pacer::new(30);
        p.reserve(t0);
        // Asked 10 ms after its slot: served at once, and the following slot stays on the
        // original schedule.
        assert_eq!(
            p.reserve(t0 + P + Duration::from_millis(10)),
            Duration::ZERO
        );
        assert_eq!(
            p.reserve(t0 + P + Duration::from_millis(10)),
            P - Duration::from_millis(10)
        );
    }

    #[test]
    fn a_long_pause_restarts_the_schedule_instead_of_bursting() {
        let t0 = Instant::now();
        let mut p = Pacer::new(30);
        p.reserve(t0);
        let later = t0 + Duration::from_secs(5);
        assert_eq!(p.reserve(later), Duration::ZERO);
        assert_eq!(p.reserve(later), P);
    }

    #[test]
    fn reset_serves_the_next_request_at_once() {
        let t0 = Instant::now();
        let mut p = Pacer::new(30);
        p.reserve(t0);
        p.reserve(t0);
        p.reset();
        assert_eq!(p.reserve(t0), Duration::ZERO);
        // Zero fps behaves as one per second rather than dividing by zero.
        let mut slow = Pacer::new(0);
        slow.reserve(t0);
        assert_eq!(slow.reserve(t0), Duration::from_secs(1));
    }
}
