//! The PI controller that steers the adaptive resampler, and a one-pole smoother.
//!
//! The controlled quantity is the timing error `e` in seconds: how far the resampler's
//! output time line is ahead of the input's real (QPC) time line. The output is a
//! relative ratio correction `u` (dimensionless), applied as `ratio = nominal * (1 + u)`.
//! The plant is an integrator: `de/dt = drift + u`. With `u = -(kp*e + ki*∫e)` the loop
//! is `e'' + kp*e' + ki*e = drift'`, so a constant drift leaves no steady-state error,
//! and the gains follow from a natural frequency `w` and damping `z`:
//! `ki = w^2`, `kp = 2*z*w`.

/// A PI controller with output clamping and conditional-integration anti-windup.
#[derive(Clone, Debug)]
pub struct Pi {
    kp: f64,
    ki: f64,
    /// Largest |u|; also bounds the integrator.
    u_max: f64,
    integral: f64,
    u: f64,
}

impl Pi {
    /// A controller whose closed loop has natural frequency `bandwidth_hz` and damping
    /// `damping`, with the correction clamped to `±u_max`.
    pub fn new(bandwidth_hz: f64, damping: f64, u_max: f64) -> Self {
        let w = 2.0 * std::f64::consts::PI * bandwidth_hz;
        Self {
            kp: 2.0 * damping * w,
            ki: w * w,
            u_max,
            integral: 0.0,
            u: 0.0,
        }
    }

    /// Feeds the error `e` (seconds) observed after `dt` seconds; returns the new
    /// correction `u`.
    pub fn update(&mut self, e: f64, dt: f64) -> f64 {
        if !e.is_finite() || !dt.is_finite() || dt <= 0.0 {
            return self.u;
        }
        let candidate = self.integral + e * dt;
        let u_try = -(self.kp * e + self.ki * candidate);
        // Integrate only while unsaturated, or when the step pulls out of saturation.
        if u_try.abs() <= self.u_max || (candidate.abs() < self.integral.abs()) {
            self.integral = candidate;
        }
        self.u = (-(self.kp * e + self.ki * self.integral)).clamp(-self.u_max, self.u_max);
        self.u
    }

    /// The current correction.
    pub fn output(&self) -> f64 {
        self.u
    }
}

/// A one-pole low-pass filter with time constant `tau` seconds.
#[derive(Clone, Debug)]
pub struct Ema {
    tau: f64,
    value: Option<f64>,
}

impl Ema {
    pub fn new(tau: f64) -> Self {
        Self { tau, value: None }
    }

    /// Feeds `x` observed `dt` seconds after the previous sample; the first sample
    /// initialises the filter.
    pub fn update(&mut self, x: f64, dt: f64) -> f64 {
        let v = match self.value {
            None => x,
            Some(v) => {
                let a = if self.tau > 0.0 {
                    1.0 - (-dt.max(0.0) / self.tau).exp()
                } else {
                    1.0
                };
                v + a * (x - v)
            }
        };
        self.value = Some(v);
        v
    }

    pub fn reset(&mut self) {
        self.value = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates the plant `de/dt = drift + u` for `secs` seconds; returns the final
    /// error and the largest |e| after `settle` seconds.
    fn simulate(pi: &mut Pi, drift: impl Fn(f64) -> f64, secs: f64, settle: f64) -> (f64, f64) {
        let dt = 0.01;
        let mut e = 0.0;
        let mut worst: f64 = 0.0;
        let steps = (secs / dt) as usize;
        for i in 0..steps {
            let t = i as f64 * dt;
            let u = pi.update(e, dt);
            e += (drift(t) + u) * dt;
            if t >= settle {
                worst = worst.max(e.abs());
            }
        }
        (e, worst)
    }

    #[test]
    fn constant_drift_leaves_no_error() {
        let mut pi = Pi::new(0.02, 0.707, 0.005);
        let (e, worst) = simulate(&mut pi, |_| 150e-6, 600.0, 120.0);
        assert!(e.abs() < 1e-6, "final error {e}");
        assert!(worst < 50e-6, "worst after settling {worst}");
        // The correction cancels the drift.
        assert!((pi.output() + 150e-6).abs() < 1e-7);
    }

    #[test]
    fn transient_stays_under_a_millisecond_for_realistic_drift() {
        // A 200 ppm device (a poor crystal): the start-up transient, which peaks near
        // 0.46 * drift / w, stays below 1 ms. The spike's settling time hides it anyway.
        let mut pi = Pi::new(0.02, 0.707, 0.005);
        let (_, worst) = simulate(&mut pi, |_| 200e-6, 300.0, 0.0);
        assert!(worst < 1e-3, "transient {worst}");
    }

    #[test]
    fn tracks_slow_drift_change() {
        // Drift wandering with temperature: ±20 ppm over 10 minutes.
        let mut pi = Pi::new(0.02, 0.707, 0.005);
        let drift = |t: f64| 80e-6 + 20e-6 * (t * 2.0 * std::f64::consts::PI / 600.0).sin();
        let (_, worst) = simulate(&mut pi, drift, 1800.0, 120.0);
        assert!(worst < 20e-6, "worst {worst}");
    }

    #[test]
    fn saturation_recovers_without_windup() {
        let mut pi = Pi::new(0.02, 0.707, 0.001);
        // A drift beyond the clamp: the error grows, the integrator must not run away.
        for _ in 0..10_000 {
            pi.update(1.0, 0.01);
        }
        assert_eq!(pi.output(), -0.001);
        // Once the error flips, the output leaves saturation within a few steps.
        let mut steps = 0;
        while pi.update(-1e-3, 0.01) <= -0.001 + 1e-12 && steps < 1000 {
            steps += 1;
        }
        assert!(steps < 1000, "stuck in saturation");
    }

    #[test]
    fn ignores_bad_input() {
        let mut pi = Pi::new(0.02, 0.707, 0.005);
        pi.update(1e-3, 0.01);
        let u = pi.output();
        assert_eq!(pi.update(f64::NAN, 0.01), u);
        assert_eq!(pi.update(1e-3, 0.0), u);
    }

    #[test]
    fn ema_smooths() {
        let mut f = Ema::new(1.0);
        assert_eq!(f.update(5.0, 0.01), 5.0);
        let mut v = 0.0;
        for _ in 0..100 {
            v = f.update(0.0, 0.01);
        }
        // After one time constant, e^-1 of the step remains.
        assert!((v - 5.0 * (-1.0f64).exp()).abs() < 1e-9);
        f.reset();
        assert_eq!(f.update(2.0, 0.01), 2.0);
    }
}
