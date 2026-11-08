//! The GPU-time guard of #349: it turns an effect-pack kind off before the OS resets a
//! GPU the kind hangs (Windows' TDR fires after about 2 s). naga validation bounds a
//! pack's shader in everything but its loops, so a loop that runs long is caught here,
//! at run time.
//!
//! The program stage's render thread asks its compositor to time each pack kind's pass
//! with timestamp queries, where the GPU has them, and hands the times to a
//! [`PassGuard`] once per tick. A kind whose pass takes more than half the frame budget
//! in [`SLOW_FRAMES`] ticks running is reported as a [`SlowPass`], which the engine
//! quarantines as it does a kind blamed for a device loss (ADR-0087). On a GPU without
//! timestamp queries nothing is timed, and a device loss is the only guard.

use std::time::Duration;

/// Ticks running in which a kind's pass must run over the limit to be reported.
pub(crate) const SLOW_FRAMES: u32 = 30;

/// An effect-pack kind that ran too long, for the engine to quarantine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlowPass {
    /// The kind, `<pack>/<effect>`.
    pub(crate) kind_id: String,
    /// Its longest pass in the last tick it ran over.
    pub(crate) took: Duration,
    /// The limit it ran over: half the frame budget.
    pub(crate) limit: Duration,
}

/// One kind's run of ticks over the limit.
#[derive(Debug)]
struct Run {
    kind_id: String,
    /// Ticks running over the limit.
    ticks: u32,
    /// Measured in the tick being collected.
    measured: bool,
    /// The longest time over the limit in the tick being collected, or zero.
    over: Duration,
}

/// Counts, per kind, the ticks running in which its pass ran over half the frame
/// budget. A tick in which a kind was timed and every time was within the limit starts
/// its count again; a tick with no time back for it (times come back a frame or two
/// late, and not every pass gets a slot) leaves the count as it is. A steady tick
/// allocates nothing: a kind seen before is found by reference.
#[derive(Debug)]
pub(crate) struct PassGuard {
    limit: Duration,
    runs: Vec<Run>,
    slow: Option<SlowPass>,
}

impl PassGuard {
    /// A guard for a frame budget of `budget`: a pass may take half of it.
    pub(crate) fn new(budget: Duration) -> Self {
        Self {
            limit: budget / 2,
            runs: Vec::new(),
            slow: None,
        }
    }

    /// One time measured for `kind_id` in the tick being collected.
    pub(crate) fn measured(&mut self, kind_id: &str, took: Duration) {
        let index = match self.runs.iter().position(|run| run.kind_id == kind_id) {
            Some(index) => index,
            None => {
                self.runs.push(Run {
                    kind_id: kind_id.to_owned(),
                    ticks: 0,
                    measured: false,
                    over: Duration::ZERO,
                });
                self.runs.len() - 1
            }
        };
        let run = &mut self.runs[index];
        run.measured = true;
        if took > self.limit {
            run.over = run.over.max(took);
        }
    }

    /// Ends the tick's collection: counts the ticks over the limit, and keeps the first
    /// kind to reach [`SLOW_FRAMES`] for [`Self::take`]. A kind reported starts its
    /// count again, so it is reported again only after as many more ticks.
    pub(crate) fn end_tick(&mut self) {
        for run in &mut self.runs {
            if !std::mem::take(&mut run.measured) {
                continue;
            }
            let over = std::mem::take(&mut run.over);
            if over.is_zero() {
                run.ticks = 0;
                continue;
            }
            run.ticks = run.ticks.saturating_add(1);
            if run.ticks >= SLOW_FRAMES && self.slow.is_none() {
                run.ticks = 0;
                self.slow = Some(SlowPass {
                    kind_id: run.kind_id.clone(),
                    took: over,
                    limit: self.limit,
                });
            }
        }
    }

    /// The kind that ran too long, once.
    pub(crate) fn take(&mut self) -> Option<SlowPass> {
        self.slow.take()
    }
}

#[cfg(test)]
mod tests {
    use super::{PassGuard, SLOW_FRAMES, SlowPass};
    use std::time::Duration;

    const BUDGET: Duration = Duration::from_millis(30);
    const SLOW: Duration = Duration::from_millis(16);
    const FAST: Duration = Duration::from_millis(14);

    fn tick(guard: &mut PassGuard, times: &[(&str, Duration)]) -> Option<SlowPass> {
        for (kind, took) in times {
            guard.measured(kind, *took);
        }
        guard.end_tick();
        guard.take()
    }

    /// #349: a kind over half the budget in 30 ticks running is reported once, with
    /// the time and the limit; then it counts again from zero.
    #[test]
    fn a_kind_over_half_the_budget_for_30_ticks_running_is_reported() {
        let mut guard = PassGuard::new(BUDGET);
        for _ in 1..SLOW_FRAMES {
            assert_eq!(tick(&mut guard, &[("fx/loop", SLOW)]), None);
        }
        assert_eq!(
            tick(&mut guard, &[("fx/loop", SLOW)]),
            Some(SlowPass {
                kind_id: "fx/loop".into(),
                took: SLOW,
                limit: Duration::from_millis(15),
            })
        );
        for _ in 1..SLOW_FRAMES {
            assert_eq!(tick(&mut guard, &[("fx/loop", SLOW)]), None);
        }
        assert!(tick(&mut guard, &[("fx/loop", SLOW)]).is_some());
    }

    /// #349: a tick within the limit starts the count again; one with no time back
    /// for the kind leaves it as it is.
    #[test]
    fn a_fast_tick_starts_the_count_again_and_a_tick_without_a_time_doesn_t() {
        let mut guard = PassGuard::new(BUDGET);
        for _ in 1..SLOW_FRAMES {
            assert_eq!(tick(&mut guard, &[("fx/loop", SLOW)]), None);
        }
        assert_eq!(tick(&mut guard, &[("fx/loop", FAST)]), None);
        for _ in 1..SLOW_FRAMES {
            assert_eq!(tick(&mut guard, &[("fx/loop", SLOW)]), None);
            assert_eq!(tick(&mut guard, &[]), None);
        }
        assert!(tick(&mut guard, &[("fx/loop", SLOW)]).is_some());
    }

    /// #349: a kind is judged by its longest pass in a tick, so a fast instance beside
    /// a slow one doesn't hide it, and each kind counts on its own.
    #[test]
    fn each_kind_counts_by_its_longest_pass() {
        let mut guard = PassGuard::new(BUDGET);
        for _ in 1..SLOW_FRAMES {
            let times = [("fx/loop", FAST), ("fx/loop", SLOW), ("fx/tint", FAST)];
            assert_eq!(tick(&mut guard, &times), None);
        }
        let reported = tick(&mut guard, &[("fx/tint", FAST), ("fx/loop", SLOW)]);
        assert_eq!(
            reported.map(|slow| slow.kind_id),
            Some("fx/loop".to_owned())
        );
    }
}
