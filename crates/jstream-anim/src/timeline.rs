//! Timelines ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 4): timed cues inside a scene, and when each one fires on the animation clock.
//!
//! Times on the animation clock are nanoseconds (`i64`), so a schedule is exact: a cue's
//! fire time is fixed when the timeline starts, and the caller gives anything it starts
//! that exact time, however late its tick runs.

use std::collections::HashSet;
use std::hash::Hash;

/// The most cues one timeline holds (ADR-0099 decision 5).
pub const MAX_CUES: usize = 256;
/// The longest timeline, in milliseconds: one hour.
pub const MAX_DURATION_MS: u32 = 3_600_000;

const NS_PER_MS: i64 = 1_000_000;

/// How a timeline repeats.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Looping {
    /// Plays once and ends at its duration.
    #[default]
    Once,
    /// Plays from 0 to its duration, again and again.
    Loop,
    /// Plays from 0, and each time it reaches `to_ms` jumps back to `from_ms`: an intro,
    /// then a loop.
    Range {
        /// Where each repeat starts.
        from_ms: u32,
        /// Where each pass ends.
        to_ms: u32,
    },
}

/// One timed action.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue<A> {
    /// When it fires, in milliseconds from the timeline's start.
    pub at_ms: u32,
    /// What it does.
    pub action: A,
}

/// A timeline: cues in time order (ties keep their order), a duration and a way to
/// repeat. The action type is the caller's: the engine uses its protocol actions.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline<A> {
    /// Its length, in milliseconds.
    pub duration_ms: u32,
    /// How it repeats.
    pub looping: Looping,
    /// Its cues, sorted by `at_ms`.
    pub cues: Vec<Cue<A>>,
}

/// Why a timeline is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidTimeline {
    /// Its duration is over [`MAX_DURATION_MS`], or 0 for a looping timeline.
    Duration,
    /// It holds more than [`MAX_CUES`] cues.
    TooManyCues,
    /// The cue at this index isn't in time order.
    Unsorted(usize),
    /// The cue at this index is past the end, or for a looping timeline at or past the
    /// point where it jumps back, so it would never fire.
    Unreachable(usize),
    /// A range loop's bounds aren't `from_ms < to_ms <= duration_ms`.
    Range,
}

/// One cue firing: which cue, on which pass, and exactly when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fire {
    /// The cue's index in [`Timeline::cues`].
    pub cue: usize,
    /// Which pass it fires on: 0 the first.
    pub pass: u64,
    /// When, on the animation clock, in nanoseconds.
    pub at_ns: i64,
}

/// Where a timeline is: its pass and its position in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// Which pass: 0 the first.
    pub pass: u64,
    /// Milliseconds from the timeline's start, within this pass.
    pub ms: f64,
    /// Whether it has ended: only a [`Looping::Once`] timeline ends.
    pub ended: bool,
}

/// What a cue's action changes, for a seek: a cue that sets a state (a keyframe, an
/// item's visibility) has a key naming what it sets, and only the last one per key
/// matters. An event (a restart, starting another timeline) has none.
pub trait CueAction {
    /// What it sets.
    type Key: Eq + Hash;
    /// What it sets, or `None` for an event.
    fn state_key(&self) -> Option<Self::Key>;
}

impl<A> Timeline<A> {
    /// The first problem with it, or `Ok`.
    pub fn check(&self) -> Result<(), InvalidTimeline> {
        if self.duration_ms > MAX_DURATION_MS {
            return Err(InvalidTimeline::Duration);
        }
        if self.cues.len() > MAX_CUES {
            return Err(InvalidTimeline::TooManyCues);
        }
        let end = match self.looping {
            Looping::Once => None,
            Looping::Loop => {
                if self.duration_ms == 0 {
                    return Err(InvalidTimeline::Duration);
                }
                Some(self.duration_ms)
            }
            Looping::Range { from_ms, to_ms } => {
                if !(from_ms < to_ms && to_ms <= self.duration_ms) {
                    return Err(InvalidTimeline::Range);
                }
                Some(to_ms)
            }
        };
        for (i, cue) in self.cues.iter().enumerate() {
            if i > 0 && cue.at_ms < self.cues[i - 1].at_ms {
                return Err(InvalidTimeline::Unsorted(i));
            }
            let reachable = match end {
                None => cue.at_ms <= self.duration_ms,
                Some(end) => cue.at_ms < end,
            };
            if !reachable {
                return Err(InvalidTimeline::Unreachable(i));
            }
        }
        Ok(())
    }

    /// Whether its looping has a length to repeat: a `Loop` longer than 0 and a `Range`
    /// with `from_ms < to_ms`. [`Timeline::check`] refuses the rest; [`Timeline::due`]
    /// and [`Timeline::position`] treat them as never playing, so unchecked input can't
    /// make them spin.
    fn repeats_validly(&self) -> bool {
        match self.looping {
            Looping::Once => true,
            Looping::Loop => self.duration_ms > 0,
            Looping::Range { from_ms, to_ms } => from_ms < to_ms,
        }
    }

    /// Where in the timeline `at_ms` milliseconds of playing falls, by pass. `None` for a
    /// negative time.
    fn place(&self, played_ms: f64) -> Option<(u64, f64, bool)> {
        if played_ms < 0.0 || played_ms.is_nan() {
            return None;
        }
        if !self.repeats_validly() {
            return None;
        }
        let duration = f64::from(self.duration_ms);
        Some(match self.looping {
            Looping::Once => (0, played_ms.min(duration), played_ms >= duration),
            Looping::Loop => {
                let pass = (played_ms / duration).floor();
                (pass as u64, played_ms - pass * duration, false)
            }
            Looping::Range { from_ms, to_ms } => {
                let (from, to) = (f64::from(from_ms), f64::from(to_ms));
                if played_ms < to {
                    (0, played_ms, false)
                } else {
                    let span = to - from;
                    let after = played_ms - to;
                    let pass = (after / span).floor();
                    (1 + pass as u64, from + (after - pass * span), false)
                }
            }
        })
    }

    /// Its position `now_ns` on the animation clock, when it started at `started_ns`.
    pub fn position(&self, started_ns: i64, now_ns: i64) -> Position {
        let played_ms = (now_ns.saturating_sub(started_ns)) as f64 / NS_PER_MS as f64;
        let (pass, ms, ended) = self.place(played_ms.max(0.0)).unwrap_or((0, 0.0, false));
        Position { pass, ms, ended }
    }

    /// When pass `pass` starts, in milliseconds of playing, and the part of the timeline
    /// it covers, `[lo, hi)` in timeline milliseconds (`hi` inclusive for a `Once`).
    fn pass_span(&self, pass: u64) -> (i64, u32, u32) {
        match self.looping {
            Looping::Once => (0, 0, self.duration_ms),
            Looping::Loop => (
                pass as i64 * i64::from(self.duration_ms),
                0,
                self.duration_ms,
            ),
            Looping::Range { from_ms, to_ms } => {
                if pass == 0 {
                    (0, 0, to_ms)
                } else {
                    let span = i64::from(to_ms - from_ms);
                    (
                        i64::from(to_ms) + (pass as i64 - 1) * span - i64::from(from_ms),
                        from_ms,
                        to_ms,
                    )
                }
            }
        }
    }

    /// The cues that fire in `[from_ns, to_ns)` on the animation clock, for a timeline
    /// that started at `started_ns`, in the order they fire. The control thread calls it
    /// each tick with the window since its last tick, so each cue fires exactly once a
    /// pass, at its exact time.
    pub fn due(&self, started_ns: i64, from_ns: i64, to_ns: i64) -> Vec<Fire> {
        let mut out = Vec::new();
        if to_ns <= from_ns || self.cues.is_empty() {
            return out;
        }
        let from_ms = (from_ns - started_ns) as f64 / NS_PER_MS as f64;
        let Some((first_pass, _, _)) = self.place(from_ms.max(0.0)) else {
            return out;
        };
        let mut pass = first_pass;
        loop {
            let (pass_start_ms, lo, hi) = self.pass_span(pass);
            let pass_start_ns = started_ns + pass_start_ms * NS_PER_MS;
            if pass_start_ns + i64::from(lo) * NS_PER_MS >= to_ns {
                break;
            }
            for (cue, c) in self.cues.iter().enumerate() {
                let inside = match self.looping {
                    Looping::Once => c.at_ms <= hi,
                    _ => c.at_ms >= lo && c.at_ms < hi,
                };
                if !inside {
                    continue;
                }
                let at_ns = pass_start_ns + i64::from(c.at_ms) * NS_PER_MS;
                if at_ns >= from_ns && at_ns < to_ns {
                    out.push(Fire { cue, pass, at_ns });
                }
            }
            if matches!(self.looping, Looping::Once) {
                break;
            }
            pass += 1;
        }
        out
    }
}

impl<A: CueAction> Timeline<A> {
    /// The cues to run when the timeline is sought to `at_ms` of pass `pass`, so the
    /// scene shows what it would if it had played there: for each state key, the cue that
    /// last set it, in the order they would have fired. Events are skipped, since they
    /// happened in the past.
    ///
    /// On a later pass of a loop, a state the previous pass set after `at_ms` still holds,
    /// and so does a `Range` intro's. So the scan covers, in play order, pass 0's cues
    /// (from pass 1 on), one whole repeat (from pass 2 on), then this pass's cues up to
    /// `at_ms`. Every pass of a loop holds the same cues, so that is the whole history.
    pub fn seek(&self, pass: u64, at_ms: u32) -> Vec<usize> {
        let (lo, hi) = match self.looping {
            Looping::Once => (0, self.duration_ms.saturating_add(1)),
            Looping::Loop => (0, self.duration_ms),
            Looping::Range { from_ms, to_ms } => (from_ms, to_ms),
        };
        let in_span = |c: &Cue<A>, lo: u32, hi: u32| c.at_ms >= lo && c.at_ms < hi;
        let first_hi = match self.looping {
            Looping::Range { to_ms, .. } => to_ms,
            _ => hi,
        };
        let pass = if matches!(self.looping, Looping::Once) {
            0
        } else {
            pass
        };
        let indexed = || self.cues.iter().enumerate();
        // In play order: pass 0 whole, one whole repeat, then this pass up to `at_ms`.
        let mut order: Vec<usize> = Vec::new();
        if pass >= 1 {
            order.extend(
                indexed()
                    .filter(|(_, c)| in_span(c, 0, first_hi))
                    .map(|(i, _)| i),
            );
        }
        if pass >= 2 {
            order.extend(
                indexed()
                    .filter(|(_, c)| in_span(c, lo, hi))
                    .map(|(i, _)| i),
            );
        }
        let this_lo = if pass == 0 { 0 } else { lo };
        order.extend(
            indexed()
                .filter(|(_, c)| in_span(c, this_lo, hi) && c.at_ms <= at_ms)
                .map(|(i, _)| i),
        );
        let mut seen = HashSet::new();
        let mut picked: Vec<usize> = order
            .into_iter()
            .rev()
            .filter(|&i| {
                self.cues[i]
                    .action
                    .state_key()
                    .is_some_and(|key| seen.insert(key))
            })
            .collect();
        picked.reverse();
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test action: `Set(target)` sets a state, `Ping` is an event.
    #[derive(Clone, Debug, PartialEq)]
    enum Act {
        Set(&'static str, u8),
        Ping,
    }

    impl CueAction for Act {
        type Key = &'static str;
        fn state_key(&self) -> Option<&'static str> {
            match self {
                Act::Set(target, _) => Some(target),
                Act::Ping => None,
            }
        }
    }

    fn timeline(duration_ms: u32, looping: Looping, at: &[u32]) -> Timeline<Act> {
        Timeline {
            duration_ms,
            looping,
            cues: at
                .iter()
                .map(|&at_ms| Cue {
                    at_ms,
                    action: Act::Ping,
                })
                .collect(),
        }
    }

    const MS: i64 = NS_PER_MS;

    fn fired(t: &Timeline<Act>, start: i64, from: i64, to: i64) -> Vec<(usize, u64, i64)> {
        t.due(start, from, to)
            .into_iter()
            .map(|f| (f.cue, f.pass, (f.at_ns - start) / MS))
            .collect()
    }

    #[test]
    fn a_sequence_fires_each_cue_once_at_its_time() {
        let t = timeline(4000, Looping::Once, &[0, 1500, 1950, 2400, 2850, 3300]);
        assert_eq!(t.check(), Ok(()));
        let start = 7 * 1000 * MS;
        // Ticked unevenly, including one long stall.
        let ticks = [0, 16, 33, 900, 1950, 1967, 2500, 3400, 5000];
        let mut all = Vec::new();
        for w in ticks.windows(2) {
            all.extend(fired(&t, start, start + w[0] * MS, start + w[1] * MS));
        }
        let times: Vec<i64> = all.iter().map(|f| f.2).collect();
        assert_eq!(times, [0, 1500, 1950, 2400, 2850, 3300]);
    }

    #[test]
    fn a_loop_fires_every_pass_and_its_end_cue_is_refused() {
        let t = timeline(1000, Looping::Loop, &[0, 250]);
        assert_eq!(t.check(), Ok(()));
        assert_eq!(
            fired(&t, 0, 0, 2600 * MS),
            [
                (0, 0, 0),
                (1, 0, 250),
                (0, 1, 1000),
                (1, 1, 1250),
                (0, 2, 2000),
                (1, 2, 2250)
            ]
        );
        assert_eq!(
            timeline(1000, Looping::Loop, &[1000]).check(),
            Err(InvalidTimeline::Unreachable(0))
        );
        assert_eq!(
            timeline(0, Looping::Loop, &[]).check(),
            Err(InvalidTimeline::Duration)
        );
    }

    #[test]
    fn a_range_plays_its_intro_once_then_loops_the_range() {
        let t = timeline(
            3000,
            Looping::Range {
                from_ms: 1000,
                to_ms: 2000,
            },
            &[0, 500, 1000, 1500],
        );
        assert_eq!(t.check(), Ok(()));
        assert_eq!(
            fired(&t, 0, 0, 4100 * MS),
            [
                (0, 0, 0),
                (1, 0, 500),
                (2, 0, 1000),
                (3, 0, 1500),
                (2, 1, 2000),
                (3, 1, 2500),
                (2, 2, 3000),
                (3, 2, 3500),
                (2, 3, 4000),
            ]
        );
        let p = t.position(0, 3700 * MS);
        assert_eq!((p.pass, p.ms, p.ended), (2, 1700.0, false));
    }

    #[test]
    fn once_ends_at_its_duration_and_keeps_its_last_cue() {
        let t = timeline(1000, Looping::Once, &[1000]);
        assert_eq!(t.check(), Ok(()));
        assert_eq!(fired(&t, 0, 0, 2000 * MS), [(0, 0, 1000)]);
        assert!(!t.position(0, 999 * MS).ended);
        assert!(t.position(0, 1000 * MS).ended);
        assert_eq!(
            timeline(1000, Looping::Once, &[1001]).check(),
            Err(InvalidTimeline::Unreachable(0))
        );
    }

    #[test]
    fn windows_before_the_start_or_empty_fire_nothing() {
        let t = timeline(1000, Looping::Loop, &[0]);
        assert_eq!(fired(&t, 500 * MS, 0, 400 * MS), []);
        assert_eq!(fired(&t, 0, 100 * MS, 100 * MS), []);
        assert_eq!(fired(&t, 500 * MS, 0, 501 * MS), [(0, 0, 0)]);
    }

    #[test]
    fn check_refuses_each_problem() {
        let mut t = timeline(1000, Looping::Once, &[500, 100]);
        assert_eq!(t.check(), Err(InvalidTimeline::Unsorted(1)));
        t = timeline(MAX_DURATION_MS + 1, Looping::Once, &[]);
        assert_eq!(t.check(), Err(InvalidTimeline::Duration));
        t = timeline(
            1000,
            Looping::Range {
                from_ms: 500,
                to_ms: 500,
            },
            &[],
        );
        assert_eq!(t.check(), Err(InvalidTimeline::Range));
        t = timeline(
            1000,
            Looping::Range {
                from_ms: 0,
                to_ms: 1001,
            },
            &[],
        );
        assert_eq!(t.check(), Err(InvalidTimeline::Range));
        let many: Vec<u32> = (0..=MAX_CUES as u32).collect();
        t = timeline(1000, Looping::Once, &many);
        assert_eq!(t.check(), Err(InvalidTimeline::TooManyCues));
    }

    #[test]
    fn a_seek_runs_the_last_state_per_target_and_no_events() {
        let t = Timeline {
            duration_ms: 5000,
            looping: Looping::Once,
            cues: vec![
                Cue {
                    at_ms: 0,
                    action: Act::Set("a", 0),
                },
                Cue {
                    at_ms: 100,
                    action: Act::Set("b", 1),
                },
                Cue {
                    at_ms: 200,
                    action: Act::Ping,
                },
                Cue {
                    at_ms: 300,
                    action: Act::Set("a", 1),
                },
                Cue {
                    at_ms: 900,
                    action: Act::Set("b", 2),
                },
            ],
        };
        assert_eq!(t.seek(0, 500), [1, 3]);
        assert_eq!(t.seek(0, 5000), [3, 4]);
        assert_eq!(t.seek(0, 0), [0]);
    }

    #[test]
    fn a_seek_into_a_later_pass_keeps_what_earlier_passes_set() {
        let set = |at_ms, target, v| Cue {
            at_ms,
            action: Act::Set(target, v),
        };
        // A loop whose only cue for `x` is late in the pass.
        let looped = Timeline {
            duration_ms: 1000,
            looping: Looping::Loop,
            cues: vec![set(100, "y", 0), set(900, "x", 1)],
        };
        assert_eq!(looped.seek(0, 500), [0], "pass 0 hasn't set x yet");
        assert_eq!(
            looped.seek(2, 500),
            [1, 0],
            "pass 1 set x, then this pass set y"
        );
        // A range: the intro sets `a` once, each repeat sets `c` at its start and `b` late.
        let ranged = Timeline {
            duration_ms: 3000,
            looping: Looping::Range {
                from_ms: 1000,
                to_ms: 2000,
            },
            cues: vec![set(0, "a", 0), set(1000, "c", 2), set(1800, "b", 1)],
        };
        assert_eq!(ranged.check(), Ok(()));
        assert_eq!(
            ranged.seek(3, 1050),
            [0, 2, 1],
            "the intro's a, the last repeat's b, then this pass's c"
        );
    }

    #[test]
    fn unchecked_zero_length_loops_never_spin() {
        let t = timeline(0, Looping::Loop, &[0]);
        assert_eq!(fired(&t, 0, 0, 10 * MS), []);
        let r = timeline(
            1000,
            Looping::Range {
                from_ms: 5,
                to_ms: 5,
            },
            &[0],
        );
        assert_eq!(fired(&r, 0, 0, 10 * MS), []);
        assert!(!t.position(0, 5 * MS).ended);
    }

    #[test]
    fn a_range_refuses_a_cue_at_or_past_its_end() {
        let t = timeline(
            3000,
            Looping::Range {
                from_ms: 1000,
                to_ms: 2000,
            },
            &[500, 2000],
        );
        assert_eq!(t.check(), Err(InvalidTimeline::Unreachable(1)));
    }

    #[test]
    fn a_random_tick_pattern_fires_the_same_cues_as_one_window() {
        // Seeded xorshift, so the test is reproducible.
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        for _ in 0..200 {
            let duration = 100 + next(2000) as u32;
            let mut at: Vec<u32> = (0..next(12))
                .map(|_| next(u64::from(duration)) as u32)
                .collect();
            at.sort_unstable();
            let looping = match next(3) {
                0 => Looping::Once,
                1 => Looping::Loop,
                _ => {
                    let from = next(u64::from(duration) - 1) as u32;
                    let to = from + 1 + next(u64::from(duration - from)) as u32;
                    at.retain(|&a| a < to);
                    Looping::Range {
                        from_ms: from,
                        to_ms: to,
                    }
                }
            };
            let t = timeline(duration, looping, &at);
            assert_eq!(t.check(), Ok(()), "{t:?}");
            let start = next(10_000) as i64 * MS;
            let end = start + 5 * i64::from(duration) * MS;
            let whole = t.due(start, start, end);
            let mut pieces = Vec::new();
            let mut tick = start;
            while tick < end {
                let step = (1 + next(400) as i64) * MS / 7;
                pieces.extend(t.due(start, tick, (tick + step).min(end)));
                tick += step;
            }
            assert_eq!(whole, pieces, "{t:?}");
        }
    }
}
