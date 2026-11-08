//! The sender's clock against this machine's.
//!
//! A frame's position in the sender's stream (samples since it started) says when the
//! sender recorded it, by the sender's clock. Its arrival time says when it got here, by
//! the media clock, late by the network's delay. The delay is never less than the path's
//! fixed part, so the *earliest* arrivals trace the true mapping, and the rest sit above
//! it by their jitter. The estimate keeps the lowest offset (arrival minus sender time)
//! in each second, and fits a line through the last 32 of them: its slope is the two
//! clocks' drift, and the line maps any sender position to the media-clock time it
//! arrives at with no jitter. That is how JTunnel's own receiver measures its CLOCK
//! reading, which it shows after 15 s.

/// Seconds of arrivals each lowest offset is taken over.
const BUCKET_NS: u64 = 1_000_000_000;
/// How many seconds' lowest offsets the fit keeps.
const BUCKETS: usize = 32;
/// Seconds of history before the drift is reported (and used).
pub const DRIFT_AFTER_S: f64 = 15.0;

#[derive(Clone, Copy, Debug, Default)]
struct Point {
    /// Sender time, seconds.
    x: f64,
    /// Lowest offset in the bucket, seconds: arrival minus sender time.
    y: f64,
}

/// Maps a sender's stream position to media-clock time, from the frames' arrivals.
///
/// It keeps a fixed amount of state and never allocates.
#[derive(Clone, Debug)]
pub struct SenderClock {
    rate: f64,
    points: [Point; BUCKETS],
    count: usize,
    head: usize,
    /// The bucket being filled: when it started (arrival ns), and its lowest point.
    current: Option<(u64, Point)>,
}

impl SenderClock {
    /// A clock for a stream at `sample_rate` samples per second, with nothing seen.
    #[must_use]
    pub fn new(sample_rate: u32) -> Self {
        Self {
            rate: f64::from(sample_rate.max(1)),
            points: [Point::default(); BUCKETS],
            count: 0,
            head: 0,
            current: None,
        }
    }

    /// Forgets everything, as when the sender restarts.
    pub fn reset(&mut self, sample_rate: u32) {
        *self = Self::new(sample_rate);
    }

    /// Takes one frame: its position in samples (where it ends: the sender sends a frame
    /// once all of it is recorded) and when it arrived on the media clock.
    pub fn observe(&mut self, position: u64, arrival_ns: u64) {
        let x = position as f64 / self.rate;
        let p = Point {
            x,
            y: arrival_ns as f64 / 1e9 - x,
        };
        match &mut self.current {
            Some((start, low)) if arrival_ns.saturating_sub(*start) < BUCKET_NS => {
                if p.y < low.y {
                    *low = p;
                }
            }
            Some((_, low)) => {
                let done = *low;
                self.points[self.head] = done;
                self.head = (self.head + 1) % BUCKETS;
                self.count = (self.count + 1).min(BUCKETS);
                self.current = Some((arrival_ns, p));
            }
            None => self.current = Some((arrival_ns, p)),
        }
    }

    fn all(&self) -> impl Iterator<Item = Point> + '_ {
        self.points[..self.count]
            .iter()
            .copied()
            .chain(self.current.map(|(_, p)| p))
    }

    /// The span of sender time the estimate covers, in seconds.
    fn span(&self) -> f64 {
        let (lo, hi) = self.all().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        });
        if lo > hi { 0.0 } else { hi - lo }
    }

    /// The fitted line, as (slope, offset at `x0`), once there's history for one.
    fn fit(&self) -> Option<(f64, f64, f64)> {
        let n = self.all().count();
        if n < 3 || self.span() < DRIFT_AFTER_S {
            return None;
        }
        let nf = n as f64;
        let mx = self.all().map(|p| p.x).sum::<f64>() / nf;
        let my = self.all().map(|p| p.y).sum::<f64>() / nf;
        let (sxx, sxy) = self.all().fold((0.0, 0.0), |(sxx, sxy), p| {
            (sxx + (p.x - mx) * (p.x - mx), sxy + (p.x - mx) * (p.y - my))
        });
        (sxx > 0.0).then(|| (sxy / sxx, mx, my))
    }

    /// The drift between the clocks in parts per million, once [`DRIFT_AFTER_S`] of
    /// stream have arrived. Positive: each of the sender's seconds takes longer here
    /// (the sender's clock runs slow against this machine's).
    #[must_use]
    pub fn drift_ppm(&self) -> Option<f64> {
        self.fit().map(|(slope, _, _)| slope * 1e6)
    }

    /// The media-clock time, in nanoseconds, at which the sample at `position` arrives
    /// with no network jitter: the earliest it can be here. `None` before any frame.
    /// Before the drift is known it is the lowest offset seen, with no slope.
    #[must_use]
    pub fn arrival_of(&self, position: u64) -> Option<u64> {
        let x = position as f64 / self.rate;
        let offset = match self.fit() {
            Some((slope, mx, my)) => {
                // The line through the lowest points sits on the earliest of them, so
                // it isn't pulled up by jitter that every bucket has.
                let lift = self
                    .all()
                    .map(|p| p.y - (my + slope * (p.x - mx)))
                    .fold(f64::MAX, f64::min);
                my + slope * (x - mx) + lift
            }
            None => self.all().map(|p| p.y).fold(f64::MAX, f64::min),
        };
        if offset == f64::MAX {
            return None;
        }
        let t = (x + offset) * 1e9;
        // Before the media clock's start it is its start.
        t.is_finite().then(|| t.max(0.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic generator, so the tests need no dependency.
    struct Lcg(u64);
    impl Lcg {
        fn unit(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 11) as f64 / (1_u64 << 53) as f64
        }
    }

    #[test]
    fn finds_the_drift_and_the_floor_under_jitter() {
        // A sender 150 ppm slow, 5 ms frames at 48 kHz, 3 ms path, up to 20 ms jitter.
        let mut c = SenderClock::new(48_000);
        let mut rng = Lcg(7);
        let ppm = 150.0;
        let path = 0.003;
        let mut last = 0;
        for k in 1..=12_000_u64 {
            let pos = k * 240;
            let sender_s = pos as f64 / 48_000.0;
            let true_arrival = sender_s * (1.0 + ppm / 1e6) + path;
            let jitter = 0.020 * rng.unit().powi(3);
            let t = ((true_arrival + jitter) * 1e9) as u64;
            c.observe(pos, t);
            last = pos;
            if k == 2_000 {
                // 10 s in: no drift yet.
                assert_eq!(c.drift_ppm(), None);
            }
        }
        let d = c.drift_ppm().unwrap_or(0.0);
        assert!((d - ppm).abs() < 15.0, "drift {d}");
        let want = (last as f64 / 48_000.0 * (1.0 + ppm / 1e6) + path) * 1e9;
        let got = c.arrival_of(last).unwrap_or(0) as f64;
        assert!(
            (got - want).abs() < 1.0e6,
            "off by {} ms",
            (got - want) / 1e6
        );
    }

    #[test]
    fn before_the_drift_is_known_it_uses_the_lowest_offset() {
        let mut c = SenderClock::new(1_000);
        assert_eq!(c.arrival_of(0), None);
        c.observe(1_000, 1_010_000_000);
        c.observe(2_000, 2_003_000_000);
        c.observe(3_000, 3_020_000_000);
        // Lowest offset: 3 ms.
        assert_eq!(c.arrival_of(4_000), Some(4_003_000_000));
        c.reset(1_000);
        assert_eq!(c.arrival_of(4_000), None);
    }
}
