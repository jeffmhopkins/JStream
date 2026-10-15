//! Latency and interval samples, summarised as percentiles.

use crate::json::Obj;

/// Durations in milliseconds.
#[derive(Default, Clone)]
pub struct Samples(Vec<f64>);

impl Samples {
    pub fn push(&mut self, ms: f64) {
        self.0.push(ms);
    }

    /// How many samples are above `limit`.
    pub fn count_above(&self, limit: f64) -> usize {
        self.0.iter().filter(|&&v| v > limit).count()
    }

    pub fn summary(&self) -> Summary {
        let mut v = self.0.clone();
        v.sort_by(f64::total_cmp);
        // Nearest-rank percentile.
        let pct = |p: f64| -> f64 {
            if v.is_empty() {
                return f64::NAN;
            }
            let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
            v[rank.clamp(1, v.len()) - 1]
        };
        Summary {
            count: v.len(),
            mean: if v.is_empty() {
                f64::NAN
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            },
            p50: pct(50.0),
            p95: pct(95.0),
            p99: pct(99.0),
            max: v.last().copied().unwrap_or(f64::NAN),
        }
    }
}

pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Summary {
    pub fn json(&self) -> String {
        Obj::new()
            .int("count", self.count as u64)
            .num("mean", self.mean)
            .num("p50", self.p50)
            .num("p95", self.p95)
            .num("p99", self.p99)
            .num("max", self.max)
            .build()
    }

    pub fn text(&self) -> String {
        format!(
            "p50 {} / p95 {} / p99 {} / max {} ms (n={})",
            ms(self.p50),
            ms(self.p95),
            ms(self.p99),
            ms(self.max),
            self.count
        )
    }
}

fn ms(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.3}")
    } else {
        "-".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::default();
        for v in 1..=100 {
            s.push(f64::from(v));
        }
        let sum = s.summary();
        assert_eq!(
            (sum.p50, sum.p95, sum.p99, sum.max),
            (50.0, 95.0, 99.0, 100.0)
        );
        assert_eq!(s.count_above(90.0), 10);
        assert!(Samples::default().summary().p50.is_nan());
    }
}
