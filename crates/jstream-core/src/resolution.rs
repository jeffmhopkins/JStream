//! How many pixels 3D models are drawn with (ADR-0197): the program stage's render
//! thread draws a model's layer smaller while the GPU can't keep its frames in time, so
//! that the outputs' frames are not the ones that give way. [`Governor`] chooses the
//! size once a tick from the GPU time of the frames that were timed and from whether the
//! GPU has fallen behind; `snapshot` draws a model's layers at what it chose
//! ([`crate::chain::Chains::model_percent`]).

use std::time::Duration;

/// The percentages of its canvas's width and height a 3D model may be drawn at, largest
/// first.
pub(crate) const STEPS: [u32; 5] = [100, 85, 75, 67, 50];

/// The environment variable that fixes the size or turns the choice off: `auto` (as
/// unset), `off` (always 100), or one of [`STEPS`].
pub(crate) const ENV: &str = "JSTREAM_MODEL_RESOLUTION";

/// A frame's GPU time above this share of its budget is too long: three timed frames
/// in a row step the size down.
const HIGH: f64 = 0.88;

/// The next larger size is tried once the GPU time it is expected to take has stayed
/// under this share of the budget for the whole wait.
const LOW: f64 = 0.78;

/// Timed frames in a row over [`HIGH`] before a step down.
const OVER: u32 = 3;

/// Ticks after a change during which nothing is decided: the frames in flight and the
/// times still on their way back were drawn at the size before.
const SETTLE: u32 = 10;

/// Seconds of room before the first step up, and the most it grows to when steps up
/// keep being undone.
const WAIT_S: u32 = 5;
const MAX_WAIT_S: u32 = 80;

/// A step up undone within this many seconds doubles the wait before the next.
const UNDONE_S: u32 = 15;

/// A percentage of its canvas's width and of its height that a model's layer is drawn
/// with: 100 by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Percent(u32);

impl Default for Percent {
    fn default() -> Self {
        Self(100)
    }
}

impl Percent {
    /// `percent`, held to 1 to 100.
    #[cfg(test)]
    pub(crate) fn of(percent: u32) -> Self {
        Self(percent.clamp(1, 100))
    }

    /// The percentage, 1 to 100.
    pub(crate) fn get(self) -> u32 {
        self.0
    }
}

/// Whether the size is chosen from the GPU's load or fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Chosen each tick.
    Auto,
    /// Always this step of [`STEPS`].
    Fixed(usize),
}

impl Mode {
    /// What `text`, the value of [`ENV`], asks for. Unset, empty or `auto` is
    /// [`Mode::Auto`]; `off` is 100; a number is that step. Anything else is `Err` with
    /// the text, and the caller falls back to [`Mode::Auto`].
    pub(crate) fn parse(text: Option<&str>) -> Result<Self, &str> {
        let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
            return Ok(Self::Auto);
        };
        if text.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        if text.eq_ignore_ascii_case("off") {
            return Ok(Self::Fixed(0));
        }
        text.parse::<u32>()
            .ok()
            .and_then(|percent| STEPS.iter().position(|step| *step == percent))
            .map(Self::Fixed)
            .ok_or(text)
    }

    /// The mode [`ENV`] asks for; a value that isn't one is logged and read as `auto`.
    pub(crate) fn from_env() -> Self {
        let text = std::env::var(ENV).ok();
        Self::parse(text.as_deref()).unwrap_or_else(|text| {
            tracing::warn!(
                value = text,
                "{ENV} is not auto, off or one of {STEPS:?}; choosing the size automatically"
            );
            Self::Auto
        })
    }
}

/// Chooses the size models are drawn at, one [`Self::tick`] per frame of the program.
///
/// It steps down at once when the GPU has fallen behind (a frame was about to be kept
/// from the outputs), and when [`OVER`] timed frames in a row took more than [`HIGH`] of
/// the frame's budget. It steps back up once the GPU time the larger size is expected to
/// take has stayed under [`LOW`] of the budget for the wait, which starts at [`WAIT_S`]
/// seconds and doubles each time a step up is undone within [`UNDONE_S`] seconds, so a
/// scene on the edge settles at the smaller size instead of flickering between two.
/// The expectation is the worst case: all of the frame's GPU time growing with the
/// model's pixels.
///
/// On a device that can't time its frames it has only the first signal, and steps up
/// after the wait without one.
#[derive(Debug)]
pub(crate) struct Governor {
    mode: Mode,
    step: usize,
    budget: Duration,
    /// Ticks a second.
    fps: u32,
    /// Timed frames in a row over [`HIGH`].
    over: u32,
    /// Whether any frame was timed: a device that can't time them never does.
    timed: bool,
    /// Whether the last timed frame left room for the next larger size; `true` until
    /// one is timed.
    roomy: bool,
    /// Ticks in a row with room and without the GPU behind.
    calm: u32,
    /// Ticks of calm before a step up.
    wait: u32,
    /// Ticks left during which nothing is decided.
    settle: u32,
    /// Ticks since the last step up.
    since_up: u32,
}

impl Governor {
    /// A governor for frames of `budget` each, starting at 100, or at `mode`'s size for
    /// good. Without a budget (a drawer no meter times) the size stays at 100.
    pub(crate) fn new(mode: Mode, budget: Duration) -> Self {
        let mode = match mode {
            Mode::Auto if budget.is_zero() => Mode::Fixed(0),
            mode => mode,
        };
        let fps = match budget.as_secs_f64() {
            period if period > 0.0 => (1.0 / period).round().clamp(1.0, 1000.0) as u32,
            _ => 30,
        };
        Self {
            mode,
            step: match mode {
                Mode::Auto => 0,
                Mode::Fixed(step) => step.min(STEPS.len() - 1),
            },
            budget,
            fps,
            over: 0,
            timed: false,
            roomy: true,
            calm: 0,
            wait: WAIT_S.saturating_mul(fps),
            settle: 0,
            since_up: u32::MAX,
        }
    }

    /// The size models are drawn at now.
    pub(crate) fn percent(&self) -> Percent {
        Percent(STEPS[self.step])
    }

    /// One frame of the program: `gpu` is the GPU time of a timed frame that came back
    /// since the last tick, if one did, and `behind` whether the GPU has fallen behind
    /// the frames already handed to it. Returns the size to draw this frame's models at.
    pub(crate) fn tick(&mut self, gpu: Option<Duration>, behind: bool) -> Percent {
        if self.mode != Mode::Auto {
            return self.percent();
        }
        self.since_up = self.since_up.saturating_add(1);
        if self.settle > 0 {
            self.settle -= 1;
            return self.percent();
        }
        if let Some(gpu) = gpu {
            self.timed = true;
            let budget = self.budget.as_secs_f64();
            let took = gpu.as_secs_f64();
            if took > budget * HIGH {
                self.over = self.over.saturating_add(1);
                self.roomy = false;
            } else {
                self.over = 0;
                self.roomy = self.step > 0 && {
                    let grow = f64::from(STEPS[self.step - 1]) / f64::from(STEPS[self.step]);
                    took * grow * grow < budget * LOW
                };
            }
        }
        if behind || self.over >= OVER {
            self.down(behind);
        } else if self.step > 0 && self.roomy {
            self.calm = self.calm.saturating_add(1);
            if self.calm >= self.wait {
                self.up();
            }
        } else {
            self.calm = 0;
        }
        self.percent()
    }

    fn down(&mut self, behind: bool) {
        self.over = 0;
        self.calm = 0;
        if self.step + 1 >= STEPS.len() {
            return;
        }
        self.step += 1;
        self.settle = SETTLE;
        // A step up this undoes was too early: the next waits twice as long.
        let base = WAIT_S.saturating_mul(self.fps);
        self.wait = if self.since_up < UNDONE_S.saturating_mul(self.fps) {
            self.wait
                .saturating_mul(2)
                .min(MAX_WAIT_S.saturating_mul(self.fps))
        } else {
            base
        };
        tracing::info!(
            percent = STEPS[self.step],
            behind,
            "3D models are drawn with fewer pixels: the GPU is short of time"
        );
    }

    fn up(&mut self) {
        self.calm = 0;
        self.step -= 1;
        self.settle = SETTLE;
        self.since_up = 0;
        // Until a frame at the new size is timed, it isn't known to have room for more.
        self.roomy = !self.timed;
        tracing::info!(
            percent = STEPS[self.step],
            "3D models are drawn with more pixels: the GPU has time to spare"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Governor, Mode, STEPS};
    use std::time::Duration;

    const FPS: u32 = 30;

    fn governor() -> Governor {
        Governor::new(Mode::Auto, Duration::from_micros(33_333))
    }

    fn ms(ms: f64) -> Option<Duration> {
        Some(Duration::from_secs_f64(ms / 1000.0))
    }

    /// Runs `ticks` ticks, a timed frame of `gpu_ms` on every fourth, and returns the
    /// size after them.
    fn run(governor: &mut Governor, ticks: u32, gpu_ms: f64) -> u32 {
        for tick in 0..ticks {
            governor.tick((tick % 4 == 0).then(|| ms(gpu_ms)).flatten(), false);
        }
        governor.percent().get()
    }

    #[test]
    fn frames_in_time_stay_at_full_size() {
        let mut governor = governor();
        assert_eq!(run(&mut governor, 3000, 20.0), 100);
    }

    #[test]
    fn three_long_frames_in_a_row_step_down_and_one_does_not() {
        let mut governor = governor();
        governor.tick(ms(31.0), false);
        governor.tick(ms(31.0), false);
        assert_eq!(governor.percent().get(), 100);
        governor.tick(ms(20.0), false);
        governor.tick(ms(31.0), false);
        governor.tick(ms(31.0), false);
        assert_eq!(governor.percent().get(), 100, "the run was broken");
        governor.tick(ms(31.0), false);
        assert_eq!(governor.percent().get(), 85);
    }

    #[test]
    fn a_gpu_that_is_behind_steps_down_at_once_but_not_twice_while_it_settles() {
        let mut governor = governor();
        governor.tick(None, true);
        assert_eq!(governor.percent().get(), 85);
        for _ in 0..super::SETTLE {
            governor.tick(None, true);
        }
        assert_eq!(governor.percent().get(), 85, "the frames in flight were the old size");
        governor.tick(None, true);
        assert_eq!(governor.percent().get(), 75);
    }

    #[test]
    fn it_stops_at_the_smallest_size() {
        let mut governor = governor();
        for _ in 0..1000 {
            governor.tick(None, true);
        }
        assert_eq!(governor.percent().get(), STEPS[STEPS.len() - 1]);
    }

    #[test]
    fn it_steps_back_up_only_after_the_wait_with_room_for_the_larger_size() {
        let mut governor = governor();
        governor.tick(None, true);
        assert_eq!(governor.percent().get(), 85);
        // 24 ms at 85% is expected to take 33 ms at 100%: no room.
        assert_eq!(run(&mut governor, 3000, 24.0), 85);
        // 15 ms is expected to take under 21 ms: room, after five seconds of it.
        assert_eq!(run(&mut governor, 4 * FPS, 15.0), 85, "not yet");
        assert_eq!(run(&mut governor, 2 * FPS, 15.0), 100);
    }

    #[test]
    fn a_step_up_that_is_undone_waits_twice_as_long_for_the_next() {
        let mut governor = governor();
        governor.tick(None, true);
        assert_eq!(run(&mut governor, 6 * FPS, 15.0), 100);
        // At full size it is behind again at once.
        run(&mut governor, super::SETTLE, 15.0);
        governor.tick(None, true);
        assert_eq!(governor.percent().get(), 85);
        assert_eq!(run(&mut governor, 9 * FPS, 15.0), 85, "five seconds no longer do");
        assert_eq!(run(&mut governor, 2 * FPS, 15.0), 100, "ten do");
    }

    #[test]
    fn without_timed_frames_it_steps_up_after_the_wait_without_falling_behind() {
        let mut governor = governor();
        governor.tick(None, true);
        for _ in 0..4 * FPS {
            governor.tick(None, false);
        }
        assert_eq!(governor.percent().get(), 85);
        for _ in 0..2 * FPS {
            governor.tick(None, false);
        }
        assert_eq!(governor.percent().get(), 100);
    }

    #[test]
    fn a_fixed_size_never_changes() {
        let mode = Mode::parse(Some("75")).expect("a step");
        let mut governor = Governor::new(mode, Duration::from_micros(33_333));
        for _ in 0..100 {
            governor.tick(ms(40.0), true);
        }
        assert_eq!(governor.percent().get(), 75);
        assert_eq!(run(&mut governor, 3000, 1.0), 75);
    }

    #[test]
    fn a_drawer_with_no_budget_stays_at_full_size() {
        let mut governor = Governor::new(Mode::Auto, Duration::ZERO);
        for _ in 0..100 {
            governor.tick(ms(40.0), true);
        }
        assert_eq!(governor.percent().get(), 100);
    }

    #[test]
    fn the_override_reads_auto_off_and_the_steps() {
        assert_eq!(Mode::parse(None), Ok(Mode::Auto));
        assert_eq!(Mode::parse(Some("")), Ok(Mode::Auto));
        assert_eq!(Mode::parse(Some(" Auto ")), Ok(Mode::Auto));
        assert_eq!(Mode::parse(Some("off")), Ok(Mode::Fixed(0)));
        assert_eq!(Mode::parse(Some("100")), Ok(Mode::Fixed(0)));
        assert_eq!(Mode::parse(Some("67")), Ok(Mode::Fixed(3)));
        assert_eq!(Mode::parse(Some("80")), Err("80"));
        assert_eq!(Mode::parse(Some("half")), Err("half"));
    }
}
