//! Running linear regression of a device's clock against the media clock. Ported from
//! spike S3 (`spikes/s3-clock/src/drift.rs`).
//!
//! Each timestamped packet gives a point `(x, y)`: `x` is the packet's media-clock time
//! and `y` is its device position divided by the nominal sample rate, both in seconds.
//! If the device clock were exact, `y = x + c`. The fitted slope is the device's real
//! rate as a fraction of nominal, so its drift is `(slope - 1) * 10^6` ppm (positive: the
//! device delivers more samples per real second than its nominal rate).
//!
//! The input processor uses the fit for two things: to reject timestamp outliers before
//! they reach the resampler's timing error, and to report the drift. The resampler is
//! steered by the accepted raw stamps, as in the spike, not by the fitted line: a single
//! line over a long run cannot follow a drift that wanders with temperature, and the PI
//! loop already filters the stamps' jitter.
//!
//! The fit is **segmented**: when a stream's position mapping restarts, a new segment
//! starts with its own intercept, and the slope is pooled over all segments (the
//! within-segment, or fixed-effects, estimator), so a glitch never biases the drift.
//!
//! Internally the fit regresses `r = y - x` (device time minus clock time, which only
//! moves by drift and jitter) on `x`, so `slope = 1 + b`. Fitting `y` directly would
//! subtract two sums of order 10^11 to get a residual sum of order 10^-2 after an hour,
//! losing most of the precision. Sums are kept as running means and co-moments
//! (Welford), so nothing grows without bound but the point count.

/// Running means and co-moments of one segment, of `x` and `r = y - x`.
#[derive(Clone, Copy, Debug, Default)]
struct Moments {
    n: u64,
    mean_x: f64,
    mean_r: f64,
    sxx: f64,
    sxr: f64,
    srr: f64,
}

impl Moments {
    fn add(&mut self, x: f64, r: f64) {
        self.n += 1;
        let n = self.n as f64;
        let dx = x - self.mean_x;
        let dr = r - self.mean_r;
        self.mean_x += dx / n;
        self.mean_r += dr / n;
        // Co-moment updates use the old delta times the new deviation.
        self.sxx += dx * (x - self.mean_x);
        self.sxr += dx * (r - self.mean_r);
        self.srr += dr * (r - self.mean_r);
    }
}

/// A segmented least-squares fit of device time against media-clock time.
#[derive(Clone, Debug, Default)]
pub(crate) struct DriftFit {
    current: Moments,
    /// Pooled co-moments of the closed segments.
    closed_sxx: f64,
    closed_sxr: f64,
    closed_srr: f64,
    closed_n: u64,
    closed_segments: u32,
}

impl DriftFit {
    /// Adds one point: clock time `x` and nominal device time `y`, both in seconds.
    /// Non-finite points are ignored.
    pub(crate) fn add(&mut self, x: f64, y: f64) {
        if x.is_finite() && y.is_finite() {
            self.current.add(x, y - x);
        }
    }

    /// Closes the current segment; later points get their own intercept.
    pub(crate) fn new_segment(&mut self) {
        if self.current.n == 0 {
            return;
        }
        // A one-point segment contributes nothing to the co-moments, but still counts.
        self.closed_sxx += self.current.sxx;
        self.closed_sxr += self.current.sxr;
        self.closed_srr += self.current.srr;
        self.closed_n += self.current.n;
        self.closed_segments = self.closed_segments.saturating_add(1);
        self.current = Moments::default();
    }

    /// Points in all segments.
    pub(crate) fn points(&self) -> u64 {
        self.closed_n + self.current.n
    }

    /// Segments with at least one point.
    pub(crate) fn segments(&self) -> u32 {
        self.closed_segments
            .saturating_add(u32::from(self.current.n > 0))
    }

    /// Points in the current segment.
    pub(crate) fn current_points(&self) -> u64 {
        self.current.n
    }

    fn pooled(&self) -> (f64, f64, f64) {
        (
            self.closed_sxx + self.current.sxx,
            self.closed_sxr + self.current.sxr,
            self.closed_srr + self.current.srr,
        )
    }

    /// Residual degrees of freedom: one intercept per segment plus the shared slope.
    fn dof(&self) -> f64 {
        self.points() as f64 - f64::from(self.segments()) - 1.0
    }

    /// The fitted slope (device seconds per clock second), once two points of one
    /// segment span some time.
    pub(crate) fn slope(&self) -> Option<f64> {
        let (sxx, sxr, _) = self.pooled();
        (sxx > 0.0 && self.dof() >= 0.0).then(|| 1.0 + sxr / sxx)
    }

    /// The drift in ppm: positive when the device runs fast against the clock.
    pub(crate) fn ppm(&self) -> Option<f64> {
        self.slope().map(|s| (s - 1.0) * 1e6)
    }

    /// Standard deviation of the points around the fitted lines, in seconds: the
    /// timestamp jitter. `None` until there are more points than parameters.
    pub(crate) fn residual_sd(&self) -> Option<f64> {
        let (sxx, sxr, srr) = self.pooled();
        let dof = self.dof();
        if sxx <= 0.0 || dof < 1.0 {
            return None;
        }
        let sse = (srr - sxr * sxr / sxx).max(0.0);
        Some((sse / dof).sqrt())
    }

    /// The fitted device time at clock time `x` on the current segment's line.
    pub(crate) fn predict_y(&self, x: f64) -> Option<f64> {
        let s = self.slope()?;
        let c = &self.current;
        (c.n > 0).then_some(c.mean_x + c.mean_r + s * (x - c.mean_x))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::testutil::Noise;

    #[test]
    fn exact_line() {
        let mut f = DriftFit::default();
        assert_eq!(f.slope(), None);
        f.add(5.0, 2.0);
        assert_eq!(f.slope(), None);
        for i in 1..100 {
            let x = 5.0 + f64::from(i) * 0.01;
            f.add(x, 2.0 + (x - 5.0) * (1.0 + 50e-6));
        }
        assert!((f.ppm().unwrap() - 50.0).abs() < 1e-6);
        assert!(f.residual_sd().unwrap() < 1e-12);
        f.add(f64::NAN, 1.0);
        assert_eq!(f.points(), 100);
    }

    /// An hour of 10 ms packets with ±0.5 ms timestamp jitter, at -37 ppm: the
    /// estimate must be well inside a ppm.
    #[test]
    fn hour_with_jitter() {
        let mut f = DriftFit::default();
        let mut noise = Noise::new(7);
        let ppm = -37.0;
        let t0 = 12_345.0; // clock seconds since boot: large offsets must not hurt.
        for i in 0..360_000 {
            let y = f64::from(i) * 0.01;
            let x = t0 + y / (1.0 + ppm * 1e-6) + noise.next() * 0.5e-3;
            f.add(x, y);
        }
        let est = f.ppm().unwrap();
        assert!((est - ppm).abs() < 0.1, "estimated {est} ppm");
        // Uniform ±0.5 ms has sd 0.289 ms.
        let sd = f.residual_sd().unwrap();
        assert!((sd - 0.289e-3).abs() < 0.02e-3, "sd {sd}");
    }

    /// A position jump between segments must not bias the slope.
    #[test]
    fn segments_have_own_intercepts() {
        let mut f = DriftFit::default();
        for i in 0..1000 {
            let x = f64::from(i) * 0.01;
            f.add(x, x * (1.0 + 100e-6));
        }
        f.new_segment();
        for i in 1000..2000 {
            let x = f64::from(i) * 0.01;
            f.add(x, 0.5 + x * (1.0 + 100e-6));
        }
        assert_eq!(f.segments(), 2);
        assert_eq!(f.points(), 2000);
        assert!((f.ppm().unwrap() - 100.0).abs() < 1e-3);
        let y = f.predict_y(25.0).unwrap();
        assert!((y - (0.5 + 25.0 * (1.0 + 100e-6))).abs() < 1e-9);
    }

    #[test]
    fn empty_segment_is_ignored() {
        let mut f = DriftFit::default();
        f.new_segment();
        assert_eq!(f.segments(), 0);
        f.add(0.0, 0.0);
        f.new_segment();
        f.new_segment();
        assert_eq!(f.segments(), 1);
        assert_eq!(f.slope(), None);
        assert_eq!(f.current_points(), 0);
    }
}
