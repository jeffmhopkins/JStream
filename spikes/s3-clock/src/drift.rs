//! Running linear regression of a device's clock against QPC.
//!
//! Each timestamped packet gives a point `(x, y)`: `x` is the packet's QPC time and `y`
//! is its device position divided by the nominal sample rate, both in seconds. If the
//! device clock were exact, `y = x + c`. The fitted slope is the device's real rate as a
//! fraction of nominal, so the drift is `(slope - 1) * 10^6` ppm (positive: the device
//! delivers more samples per real second than its nominal rate).
//!
//! The fit is **segmented**: when a stream's position mapping restarts (a glitch that
//! the position and the clock disagree on), a new segment starts with its own
//! intercept, and the slope is pooled over all segments (the within-segment, or
//! fixed-effects, estimator). A glitch therefore never biases the drift estimate.
//!
//! Internally the fit regresses `r = y - x` (device time minus QPC time, which only
//! moves by drift and jitter) on `x`, so `slope = 1 + b`. Fitting `y` directly would
//! subtract two sums of order 10^11 to get a residual sum of order 10^-2 after an hour,
//! losing most of the precision. Sums are kept as running means and co-moments
//! (Welford).

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

/// A segmented least-squares fit of device time against QPC time.
#[derive(Clone, Debug, Default)]
pub struct DriftFit {
    current: Moments,
    /// Pooled co-moments of the closed segments.
    closed_sxx: f64,
    closed_sxr: f64,
    closed_srr: f64,
    closed_n: u64,
    closed_segments: u32,
    /// Smallest and largest `x` seen, for the time span of the fit.
    x_min: f64,
    x_max: f64,
}

impl DriftFit {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one point: QPC time `x` and nominal device time `y`, both in seconds.
    pub fn add(&mut self, x: f64, y: f64) {
        if self.points() == 0 {
            self.x_min = x;
            self.x_max = x;
        } else {
            self.x_min = self.x_min.min(x);
            self.x_max = self.x_max.max(x);
        }
        self.current.add(x, y - x);
    }

    /// Closes the current segment; later points get their own intercept.
    pub fn new_segment(&mut self) {
        if self.current.n == 0 {
            return;
        }
        // A one-point segment contributes nothing to the co-moments, but still counts.
        self.closed_sxx += self.current.sxx;
        self.closed_sxr += self.current.sxr;
        self.closed_srr += self.current.srr;
        self.closed_n += self.current.n;
        self.closed_segments += 1;
        self.current = Moments::default();
    }

    /// Points in all segments.
    pub fn points(&self) -> u64 {
        self.closed_n + self.current.n
    }

    /// Segments with at least one point.
    pub fn segments(&self) -> u32 {
        self.closed_segments + u32::from(self.current.n > 0)
    }

    /// Points in the current segment.
    pub fn current_points(&self) -> u64 {
        self.current.n
    }

    /// The QPC time spanned by the points, in seconds.
    pub fn span(&self) -> f64 {
        if self.points() == 0 {
            0.0
        } else {
            self.x_max - self.x_min
        }
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

    /// The fitted slope (device seconds per QPC second), once two points span some time.
    pub fn slope(&self) -> Option<f64> {
        let (sxx, sxr, _) = self.pooled();
        (sxx > 0.0 && self.dof() >= 0.0).then(|| 1.0 + sxr / sxx)
    }

    /// The drift in ppm: positive when the device runs fast against QPC.
    pub fn ppm(&self) -> Option<f64> {
        self.slope().map(|s| (s - 1.0) * 1e6)
    }

    /// Standard deviation of the points around the fitted lines, in seconds: the
    /// timestamp jitter. `None` until there are more points than parameters.
    pub fn residual_sd(&self) -> Option<f64> {
        let (sxx, sxr, srr) = self.pooled();
        let dof = self.dof();
        if sxx <= 0.0 || dof < 1.0 {
            return None;
        }
        let sse = (srr - sxr * sxr / sxx).max(0.0);
        Some((sse / dof).sqrt())
    }

    /// Standard error of the drift estimate, in ppm.
    pub fn stderr_ppm(&self) -> Option<f64> {
        let (sxx, ..) = self.pooled();
        self.residual_sd().map(|sd| sd / sxx.sqrt() * 1e6)
    }

    /// The fitted device time at QPC time `x` on the current segment's line.
    pub fn predict_y(&self, x: f64) -> Option<f64> {
        let s = self.slope()?;
        let c = &self.current;
        (c.n > 0).then_some(c.mean_x + c.mean_r + s * (x - c.mean_x))
    }

    /// The fitted QPC time of device time `y` on the current segment's line: this maps a
    /// sample position onto the media clock.
    pub fn map_to_qpc(&self, y: f64) -> Option<f64> {
        let s = self.slope()?;
        let c = &self.current;
        (c.n > 0 && s > 0.0).then(|| c.mean_x + (y - (c.mean_x + c.mean_r)) / s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Noise;

    #[test]
    fn exact_line() {
        let mut f = DriftFit::new();
        assert_eq!(f.slope(), None);
        f.add(5.0, 2.0);
        assert_eq!(f.slope(), None);
        for i in 1..100 {
            let x = 5.0 + f64::from(i) * 0.01;
            f.add(x, 2.0 + (x - 5.0) * (1.0 + 50e-6));
        }
        assert!((f.ppm().unwrap() - 50.0).abs() < 1e-6);
        assert!(f.residual_sd().unwrap() < 1e-12);
        assert!((f.span() - 0.99).abs() < 1e-9);
    }

    /// An hour of 10 ms packets with ±0.5 ms timestamp jitter, at -37 ppm: the
    /// estimate must be well inside a ppm, and the stderr honest.
    #[test]
    fn hour_with_jitter() {
        let mut f = DriftFit::new();
        let mut noise = Noise::new(7);
        let ppm = -37.0;
        let t0 = 12_345.0; // QPC seconds since boot: large offsets must not hurt.
        for i in 0..360_000 {
            let y = f64::from(i) * 0.01;
            let x = t0 + y / (1.0 + ppm * 1e-6) + noise.next() * 0.5e-3;
            f.add(x, y);
        }
        let est = f.ppm().unwrap();
        assert!((est - ppm).abs() < 0.1, "estimated {est} ppm");
        let se = f.stderr_ppm().unwrap();
        assert!(se > 0.0 && se < 0.05, "stderr {se}");
        // Uniform ±0.5 ms has sd 0.289 ms.
        let sd = f.residual_sd().unwrap();
        assert!((sd - 0.289e-3).abs() < 0.02e-3, "sd {sd}");
    }

    /// A position jump between segments must not bias the slope.
    #[test]
    fn segments_have_own_intercepts() {
        let mut f = DriftFit::new();
        for i in 0..1000 {
            let x = f64::from(i) * 0.01;
            f.add(x, x * (1.0 + 100e-6));
        }
        f.new_segment();
        // The device position jumped by half a second the clock didn't see.
        for i in 1000..2000 {
            let x = f64::from(i) * 0.01;
            f.add(x, 0.5 + x * (1.0 + 100e-6));
        }
        assert_eq!(f.segments(), 2);
        assert_eq!(f.points(), 2000);
        assert!((f.ppm().unwrap() - 100.0).abs() < 1e-3);
        // A naive single fit would see a huge slope here.
        let mut naive = DriftFit::new();
        for i in 0..2000 {
            let x = f64::from(i) * 0.01;
            naive.add(x, x * (1.0 + 100e-6) + if i >= 1000 { 0.5 } else { 0.0 });
        }
        assert!(naive.ppm().unwrap() > 10_000.0);
    }

    #[test]
    fn maps_positions_to_qpc() {
        let mut f = DriftFit::new();
        let ppm = 250.0;
        for i in 0..1000 {
            let y = f64::from(i) * 0.01;
            f.add(100.0 + y / (1.0 + ppm * 1e-6), y);
        }
        // Device time 20 s (beyond the data) maps to the QPC time the line predicts.
        let x = f.map_to_qpc(20.0).unwrap();
        assert!((x - (100.0 + 20.0 / (1.0 + ppm * 1e-6))).abs() < 1e-9);
        let y = f.predict_y(x).unwrap();
        assert!((y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn empty_segment_is_ignored() {
        let mut f = DriftFit::new();
        f.new_segment();
        assert_eq!(f.segments(), 0);
        f.add(0.0, 0.0);
        f.new_segment();
        f.new_segment();
        assert_eq!(f.segments(), 1);
        assert_eq!(f.slope(), None);
    }
}
