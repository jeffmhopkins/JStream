//! Test helpers: a deterministic noise source and sine analysis.

/// A tiny deterministic PRNG (xorshift64*) giving uniform values in `[-1, 1)`.
pub(crate) struct Noise(u64);

impl Noise {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(crate) fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        v as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// `sin(2π·freq·t)`.
pub(crate) fn tone(freq: f64, t: f64) -> f64 {
    (std::f64::consts::TAU * freq * t).sin()
}

/// The least-squares sine at a known frequency.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SineFit {
    pub(crate) amplitude: f64,
    /// Phase of `amplitude · sin(2π·freq·n/rate + phase)` at `n = 0`.
    pub(crate) phase: f64,
    /// Power not explained by the sine and DC, relative to the sine's, in dB.
    pub(crate) residual_db: f64,
}

/// Fits `a·sin(ωn) + b·cos(ωn) + c` to `x` (`ω = 2π·freq/rate`) by least squares.
pub(crate) fn fit_sine(x: &[f32], freq: f64, rate: f64) -> SineFit {
    let w = std::f64::consts::TAU * freq / rate;
    // Normal equations for [sin, cos, 1].
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (n, &s) in x.iter().enumerate() {
        let b = [(w * n as f64).sin(), (w * n as f64).cos(), 1.0];
        for i in 0..3 {
            v[i] += b[i] * f64::from(s);
            for j in 0..3 {
                m[i][j] += b[i] * b[j];
            }
        }
    }
    let [a, b, c] = solve3(m, v);
    let mut res = 0.0;
    for (n, &s) in x.iter().enumerate() {
        let p = a * (w * n as f64).sin() + b * (w * n as f64).cos() + c;
        res += (f64::from(s) - p).powi(2);
    }
    let amplitude = a.hypot(b);
    let sig = amplitude * amplitude / 2.0 * x.len() as f64;
    SineFit {
        amplitude,
        phase: b.atan2(a),
        residual_db: 10.0 * (res / sig).log10(),
    }
}

fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> [f64; 3] {
    for col in 0..3 {
        let piv = (col..3)
            .max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))
            .unwrap_or(col);
        m.swap(col, piv);
        v.swap(col, piv);
        let (pivot, vp) = (m[col], v[col]);
        for (mr, vr) in m.iter_mut().zip(v.iter_mut()).skip(col + 1) {
            let f = mr[col] / pivot[col];
            for (a, p) in mr.iter_mut().zip(pivot.iter()).skip(col) {
                *a -= f * p;
            }
            *vr -= f * vp;
        }
    }
    let mut out = [0.0; 3];
    for row in (0..3).rev() {
        let mut s = v[row];
        for k in row + 1..3 {
            s -= m[row][k] * out[k];
        }
        out[row] = s / m[row][row];
    }
    out
}

#[test]
fn fit_sine_recovers_a_known_sine() {
    let x: Vec<f32> = (0..4800)
        .map(|n| {
            (0.3 * (std::f64::consts::TAU * 1000.0 * n as f64 / 48_000.0 + 0.5).sin() + 0.1) as f32
        })
        .collect();
    let f = fit_sine(&x, 1000.0, 48_000.0);
    assert!((f.amplitude - 0.3).abs() < 1e-6);
    assert!((f.phase - 0.5).abs() < 1e-5);
    assert!(f.residual_db < -120.0);
    let mut n = Noise::new(1);
    assert!(
        (0..1000)
            .map(|_| n.next())
            .all(|v| (-1.0..1.0).contains(&v))
    );
}
