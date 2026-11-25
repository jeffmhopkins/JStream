//! How many pixels 3D models are drawn with (ADR-0197): the program stage's render
//! thread draws a model's layer smaller while the GPU can't keep its frames in time, so
//! that the outputs' frames are not the ones that give way. [`Governor`] chooses the
//! size once a tick from the GPU time of the frames that were timed and from whether the
//! GPU has fallen behind; `snapshot` draws a model's layers at what it chose
//! ([`crate::chain::Chains::model_percent`]).

use std::time::Duration;

/// The percentages of its canvas's width and height the governor steps a 3D model
/// through while it chooses, largest first: the protocol's `MODEL_RESOLUTIONS` without
/// its 0.
pub(crate) const STEPS: [u32; 5] = [100, 85, 75, 67, 50];

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

/// Seconds after the first tick, and after an output's first frame, during which
/// nothing is decided: a program's first frames, and an output's, upload and compile,
/// and take longer than the ones after them.
const START_S: u32 = 3;

/// The smallest size the governor may choose while no floor is set: the last of
/// [`STEPS`].
const NO_FLOOR: u32 = STEPS[STEPS.len() - 1];

/// The sizes the governor chooses among while it may go no smaller than `floor`
/// (`RenderFeatures::min_model_resolution`), largest first, and how many there are:
/// each of [`STEPS`] above `floor`, then `floor` itself. A floor between two steps is
/// the last step down; one at or below the smallest step leaves [`STEPS`] as they are.
fn ladder(floor: u32) -> ([u32; STEPS.len()], usize) {
    let floor = floor.clamp(NO_FLOOR, STEPS[0]);
    let mut sizes = [0; STEPS.len()];
    let mut rungs = 0;
    for step in STEPS.iter().copied().filter(|step| *step > floor) {
        sizes[rungs] = step;
        rungs += 1;
    }
    sizes[rungs] = floor;
    (sizes, rungs + 1)
}

/// What a frame's GPU time at each of `sizes` is taken to be of its time at the size
/// before, until a step down has shown it: all of it growing with the model's pixels.
fn worst(sizes: &[u32], step: usize) -> f64 {
    match step.checked_sub(1) {
        Some(larger) if step < sizes.len() && sizes[larger] > 0 => {
            let side = f64::from(sizes[step]) / f64::from(sizes[larger]);
            side * side
        }
        _ => 1.0,
    }
}

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
    /// Always this percentage, the smallest of [`STEPS`] to 100.
    Fixed(u32),
}

impl Mode {
    /// What `setting` asks for, the rendering switches' `model_resolution`: 0 is
    /// [`Mode::Auto`] and any other is that percentage, held to the smallest of
    /// [`STEPS`] to 100 (`studio.set_rendering` refuses one outside them).
    pub(crate) fn of(setting: u32) -> Self {
        match setting {
            0 => Self::Auto,
            percent => Self::Fixed(percent.clamp(NO_FLOOR, STEPS[0])),
        }
    }
}

/// Chooses the size models are drawn at, one [`Self::tick`] per frame of the program.
///
/// It steps down at once when the GPU has fallen behind (a frame was about to be kept
/// from the outputs) while the last frame timed at this size was long too, and when [`OVER`] timed frames in a row took more than [`HIGH`] of
/// the frame's budget. It steps back up once the GPU time the larger size is expected to
/// take has stayed under [`LOW`] of the budget for the wait, which starts at [`WAIT_S`]
/// seconds and doubles each time a step up is undone within [`UNDONE_S`] seconds, so a
/// scene on the edge settles at the smaller size instead of flickering between two.
/// The expectation comes from what the step down to this size saved, the frame's time
/// after it over its time before; until a step down has been timed it is the worst
/// case, all of the frame's GPU time growing with the model's pixels. It decides
/// nothing for the first [`START_S`] seconds, whose frames upload and compile, nor for
/// the [`START_S`] seconds after an output starts ([`Self::output_started`],
/// ADR-0236).
///
/// It steps down only while the frames before drew a model: with none there is nothing
/// to gain. On a device that can't time its frames it has only the first signal, and
/// steps up after the wait without one.
///
/// While the rendering switches fix a size ([`Mode::Fixed`]) it draws at that one, and
/// goes on from it when they hand the choice back. While it chooses, it goes no smaller
/// than the floor [`Self::set_floor`] last gave (`RenderFeatures::min_model_resolution`):
/// it chooses among the [`STEPS`] above the floor and the floor itself ([`ladder`]).
#[derive(Debug)]
pub(crate) struct Governor {
    /// Which of `sizes` models are drawn at.
    step: usize,
    /// The sizes it chooses among, largest first ([`ladder`]); the first `rungs` count.
    sizes: [u32; STEPS.len()],
    rungs: usize,
    /// The floor `sizes` were made for.
    made_for: u32,
    /// The floor it is to choose above, from [`Self::set_floor`].
    floor: u32,
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
    /// The GPU time of the last frame timed at this size.
    last: Option<f64>,
    /// The size stepped down from and the GPU time of the last frame timed at it, until
    /// a frame at this size is timed.
    before: Option<(usize, f64)>,
    /// What a frame's GPU time at each of `sizes` is of its time at the size before
    /// ([`worst`], or what a step down showed).
    saved: [f64; STEPS.len()],
}

impl Governor {
    /// A governor for frames of `budget` each, starting at 100. Without a budget (a
    /// drawer no meter times) it never chooses a smaller size itself.
    pub(crate) fn new(budget: Duration) -> Self {
        let fps = match budget.as_secs_f64() {
            period if period > 0.0 => (1.0 / period).round().clamp(1.0, 1000.0) as u32,
            _ => 30,
        };
        let (sizes, rungs) = ladder(NO_FLOOR);
        Self {
            step: 0,
            sizes,
            rungs,
            made_for: NO_FLOOR,
            floor: NO_FLOOR,
            budget,
            fps,
            over: 0,
            timed: false,
            roomy: true,
            calm: 0,
            wait: WAIT_S.saturating_mul(fps),
            settle: START_S.saturating_mul(fps),
            since_up: u32::MAX,
            last: None,
            before: None,
            saved: std::array::from_fn(|step| worst(&sizes[..rungs], step)),
        }
    }

    /// The smallest size it may choose from the next tick on, a percentage held to the
    /// smallest of [`STEPS`] to 100 (`RenderFeatures::min_model_resolution`). A fixed
    /// size is drawn as set whatever this is.
    pub(crate) fn set_floor(&mut self, floor: u32) {
        self.floor = floor.clamp(NO_FLOOR, STEPS[0]);
    }

    /// Chooses among the sizes for `floor` from now on, at the size it draws at now if
    /// that is one of them, else the next larger one, or the floor if it draws smaller.
    /// What it knew of the old sizes says nothing of the new ones.
    fn reladder(&mut self, floor: u32) {
        let was = self.percent().get();
        let (sizes, rungs) = ladder(floor);
        self.sizes = sizes;
        self.rungs = rungs;
        self.made_for = floor;
        self.step = sizes[..rungs]
            .iter()
            .rposition(|size| *size >= was)
            .unwrap_or(rungs - 1);
        self.saved = std::array::from_fn(|step| worst(&sizes[..rungs], step));
        self.before = None;
        if self.percent().get() != was {
            self.over = 0;
            self.calm = 0;
            self.settle = SETTLE;
            self.roomy = !self.timed;
            self.last = None;
            tracing::info!(
                percent = self.percent().get(),
                "3D models are drawn at a size the rendering switches set"
            );
        }
    }

    /// An output drew its first frame: its pipelines, layers and encode targets are
    /// made then, so the GPU falls behind for a moment that fewer pixels would not
    /// shorten. Nothing is decided for the next [`START_S`] seconds, and the frames
    /// timed before them say nothing about the ones after.
    pub(crate) fn output_started(&mut self) {
        self.settle = self.settle.max(START_S.saturating_mul(self.fps));
        self.over = 0;
        self.calm = 0;
        self.last = None;
        self.before = None;
    }

    /// The size models are drawn at now.
    pub(crate) fn percent(&self) -> Percent {
        Percent(self.sizes[self.step])
    }

    /// One frame of the program: `mode` is what the rendering switches ask for, `gpu`
    /// the GPU time of a timed frame that came back since the last tick, if one did,
    /// `behind` whether the GPU has fallen behind the frames already handed to it, and
    /// `models` whether a model was drawn since the last tick. Returns the size to draw
    /// this frame's models at.
    pub(crate) fn tick(
        &mut self,
        mode: Mode,
        gpu: Option<Duration>,
        behind: bool,
        models: bool,
    ) -> Percent {
        if let Mode::Fixed(fixed) = mode {
            // A fixed size between two steps, or below the floor, is on no rung of the
            // sizes: they become the steps above it and it.
            if !self.sizes[..self.rungs].contains(&fixed) {
                self.reladder(fixed);
            }
            let step = self.sizes[..self.rungs]
                .iter()
                .position(|size| *size == fixed)
                .unwrap_or(0);
            // The next choice starts from here, with nothing known about it.
            if self.step != step {
                self.step = step;
                self.over = 0;
                self.calm = 0;
                self.settle = SETTLE;
                self.roomy = !self.timed;
                self.last = None;
                self.before = None;
            }
            return self.percent();
        }
        if self.made_for != self.floor {
            self.reladder(self.floor);
        }
        if self.budget.is_zero() {
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
            // The first frame timed after a step down says what the step saved, held
            // between the worst case and nothing.
            if let Some((from, was)) = self.before.take()
                && from + 1 == self.step
                && was > 0.0
            {
                let worst = worst(&self.sizes[..self.rungs], self.step);
                self.saved[self.step] = (took / was).clamp(worst, 1.0);
            }
            self.last = Some(took);
            if took > budget * HIGH {
                self.over = self.over.saturating_add(1);
                self.roomy = false;
            } else {
                self.over = 0;
                self.roomy = self.step > 0 && took / self.saved[self.step] < budget * LOW;
            }
        }
        if !models {
            // Nothing to gain from fewer pixels: long frames have another cause.
            self.over = 0;
        }
        // Behind with frames that leave room is a hitch fewer pixels would not shorten
        // (a scene's or view's first frames, an upload, another app's work): it steps
        // down only where the last frame timed at this size was long too, or none can
        // be timed (ADR-0236).
        let long = !self.timed
            || self
                .last
                .is_some_and(|took| took > self.budget.as_secs_f64() * HIGH);
        if models && ((behind && long) || self.over >= OVER) {
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
        if self.step + 1 >= self.rungs {
            return;
        }
        self.before = self.last.take().map(|was| (self.step, was));
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
            percent = self.sizes[self.step],
            behind,
            "3D models are drawn with fewer pixels: the GPU is short of time"
        );
    }

    fn up(&mut self) {
        self.calm = 0;
        self.step -= 1;
        self.settle = SETTLE;
        self.since_up = 0;
        self.last = None;
        self.before = None;
        // Until a frame at the new size is timed, it isn't known to have room for more.
        self.roomy = !self.timed;
        tracing::info!(
            percent = self.sizes[self.step],
            "3D models are drawn with more pixels: the GPU has time to spare"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::Mode::Auto;
    use super::{Governor, Mode, STEPS};
    use std::time::Duration;

    const FPS: u32 = 30;

    /// A governor past its first seconds.
    fn governor() -> Governor {
        let mut governor = Governor::new(Duration::from_micros(33_333));
        for _ in 0..super::START_S * FPS {
            governor.tick(Auto, None, false, true);
        }
        governor
    }

    fn ms(ms: f64) -> Option<Duration> {
        Some(Duration::from_secs_f64(ms / 1000.0))
    }

    /// Runs `ticks` ticks, a timed frame of `gpu_ms` on every fourth, and returns the
    /// size after them.
    fn run(governor: &mut Governor, ticks: u32, gpu_ms: f64) -> u32 {
        for tick in 0..ticks {
            governor.tick(
                Auto,
                (tick % 4 == 0).then(|| ms(gpu_ms)).flatten(),
                false,
                true,
            );
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
        governor.tick(Auto, ms(31.0), false, true);
        governor.tick(Auto, ms(31.0), false, true);
        assert_eq!(governor.percent().get(), 100);
        governor.tick(Auto, ms(20.0), false, true);
        governor.tick(Auto, ms(31.0), false, true);
        governor.tick(Auto, ms(31.0), false, true);
        assert_eq!(governor.percent().get(), 100, "the run was broken");
        governor.tick(Auto, ms(31.0), false, true);
        assert_eq!(governor.percent().get(), 85);
    }

    #[test]
    fn a_gpu_that_is_behind_steps_down_at_once_but_not_twice_while_it_settles() {
        let mut governor = governor();
        governor.tick(Auto, None, true, true);
        assert_eq!(governor.percent().get(), 85);
        for _ in 0..super::SETTLE {
            governor.tick(Auto, None, true, true);
        }
        assert_eq!(
            governor.percent().get(),
            85,
            "the frames in flight were the old size"
        );
        governor.tick(Auto, None, true, true);
        assert_eq!(governor.percent().get(), 75);
    }

    #[test]
    fn it_stops_at_the_smallest_size() {
        let mut governor = governor();
        for _ in 0..1000 {
            governor.tick(Auto, None, true, true);
        }
        assert_eq!(governor.percent().get(), STEPS[STEPS.len() - 1]);
    }

    #[test]
    fn it_steps_back_up_only_after_the_wait_with_room_for_the_larger_size() {
        let mut governor = governor();
        governor.tick(Auto, None, true, true);
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
        governor.tick(Auto, None, true, true);
        assert_eq!(run(&mut governor, 6 * FPS, 15.0), 100);
        // At full size its frames are long again at once.
        run(&mut governor, super::SETTLE, 15.0);
        for _ in 0..super::OVER {
            governor.tick(Auto, ms(31.0), false, true);
        }
        assert_eq!(governor.percent().get(), 85);
        assert_eq!(
            run(&mut governor, 9 * FPS, 15.0),
            85,
            "five seconds no longer do"
        );
        assert_eq!(run(&mut governor, 2 * FPS, 15.0), 100, "ten do");
    }

    #[test]
    fn without_timed_frames_it_steps_up_after_the_wait_without_falling_behind() {
        let mut governor = governor();
        governor.tick(Auto, None, true, true);
        for _ in 0..4 * FPS {
            governor.tick(Auto, None, false, true);
        }
        assert_eq!(governor.percent().get(), 85);
        for _ in 0..2 * FPS {
            governor.tick(Auto, None, false, true);
        }
        assert_eq!(governor.percent().get(), 100);
    }

    #[test]
    fn a_fixed_size_holds_and_the_choice_goes_on_from_it() {
        let fixed = Mode::of(75);
        assert_eq!(fixed, Mode::Fixed(75));
        let mut governor = governor();
        for _ in 0..100 {
            governor.tick(fixed, ms(40.0), true, true);
        }
        assert_eq!(governor.percent().get(), 75);
        for _ in 0..3000 {
            governor.tick(fixed, ms(1.0), false, true);
        }
        assert_eq!(governor.percent().get(), 75);
        // Handed back, it steps up from there once there has been room for the wait.
        assert_eq!(run(&mut governor, 4 * FPS, 5.0), 75, "not yet");
        assert_eq!(run(&mut governor, 3 * FPS, 5.0), 85);
    }

    #[test]
    fn it_does_not_step_down_while_no_model_is_drawn() {
        let mut governor = governor();
        for _ in 0..100 {
            governor.tick(Auto, ms(40.0), true, false);
        }
        assert_eq!(governor.percent().get(), 100);
        governor.tick(Auto, None, true, true);
        assert_eq!(governor.percent().get(), 85);
    }

    #[test]
    fn a_floor_between_two_steps_is_the_last_step_down() {
        assert_eq!(super::ladder(90), ([100, 90, 0, 0, 0], 2));
        assert_eq!(super::ladder(85), ([100, 85, 0, 0, 0], 2));
        assert_eq!(super::ladder(50), (STEPS, STEPS.len()));
        assert_eq!(
            super::ladder(10),
            (STEPS, STEPS.len()),
            "held to the smallest"
        );
        assert_eq!(super::ladder(100), ([100, 0, 0, 0, 0], 1));
        let mut governor = governor();
        governor.set_floor(90);
        for _ in 0..1000 {
            governor.tick(Auto, None, true, true);
        }
        assert_eq!(governor.percent().get(), 90, "never below the floor");
        // It steps back up from the floor as from any size.
        assert_eq!(run(&mut governor, 6 * FPS, 15.0), 100);
    }

    #[test]
    fn a_floor_of_100_keeps_models_whole() {
        let mut governor = governor();
        governor.set_floor(100);
        for _ in 0..1000 {
            governor.tick(Auto, ms(40.0), true, true);
        }
        assert_eq!(governor.percent().get(), 100);
    }

    #[test]
    fn a_floor_raised_below_the_size_drawn_lifts_it_to_the_floor() {
        let mut governor = governor();
        for _ in 0..1000 {
            governor.tick(Auto, None, true, true);
        }
        assert_eq!(governor.percent().get(), 50);
        governor.set_floor(80);
        governor.tick(Auto, None, false, true);
        assert_eq!(governor.percent().get(), 80);
        // Raised above a size it draws at that is one of the new sizes, it stays.
        let mut other = self::governor();
        other.tick(Auto, None, true, true);
        assert_eq!(other.percent().get(), 85);
        other.set_floor(75);
        other.tick(Auto, None, false, true);
        assert_eq!(other.percent().get(), 85);
    }

    #[test]
    fn a_fixed_size_between_two_steps_is_drawn_and_the_choice_goes_on_above_it() {
        let mut governor = governor();
        for _ in 0..10 {
            governor.tick(Mode::of(92), ms(40.0), true, true);
        }
        assert_eq!(governor.percent().get(), 92);
        // Handed back, it starts at the next step up, and steps down through the steps.
        governor.tick(Auto, None, false, true);
        assert_eq!(governor.percent().get(), 100);
        for _ in 0..1000 {
            governor.tick(Auto, None, true, true);
        }
        assert_eq!(governor.percent().get(), 50);
    }

    #[test]
    fn a_fixed_size_below_the_floor_is_drawn_as_set() {
        let mut governor = governor();
        governor.set_floor(90);
        for _ in 0..10 {
            governor.tick(Mode::of(67), None, true, true);
        }
        assert_eq!(governor.percent().get(), 67);
        // Handed back, the choice starts at the floor.
        governor.tick(Auto, None, false, true);
        assert_eq!(governor.percent().get(), 90);
    }

    #[test]
    fn the_first_seconds_decide_nothing() {
        let mut governor = Governor::new(Duration::from_micros(33_333));
        for _ in 0..super::START_S * FPS {
            governor.tick(Auto, ms(60.0), true, true);
        }
        assert_eq!(governor.percent().get(), 100, "start-up frames are long");
        governor.tick(Auto, None, true, true);
        assert_eq!(governor.percent().get(), 85);
    }

    /// An output started 0.8 s into a show, as measured on a streaming PC: its first
    /// frames keep the GPU behind for a moment, which used to step a scene that fits at
    /// 100 down to 85 for good.
    #[test]
    fn an_output_starting_does_not_step_down() {
        let mut governor = governor();
        assert_eq!(run(&mut governor, 30, 24.0), 100);
        governor.output_started();
        for _ in 0..FPS {
            governor.tick(Auto, ms(60.0), true, true);
        }
        assert_eq!(
            governor.percent().get(),
            100,
            "the output's first frames are long"
        );
        assert_eq!(run(&mut governor, 3000, 24.0), 100);
        // Once the start is over, falling behind with a long frame steps down as before.
        governor.tick(Auto, ms(31.0), true, true);
        assert_eq!(governor.percent().get(), 85);
    }

    /// A view switched to on a streaming PC: its first frames kept the GPU behind for
    /// seconds while each frame timed left room, which stepped 85, 75, 67 and 50 in turn.
    #[test]
    fn falling_behind_with_frames_that_leave_room_does_not_step_down() {
        let mut governor = governor();
        assert_eq!(run(&mut governor, 30, 24.0), 100);
        for tick in 0..10 * FPS {
            governor.tick(
                Auto,
                (tick % 4 == 0).then(|| ms(24.0)).flatten(),
                true,
                true,
            );
        }
        assert_eq!(
            governor.percent().get(),
            100,
            "the hitch is not the models'"
        );
        // Long frames while behind step down once, and the smaller size's frames, which
        // leave room, hold it there however long the GPU stays behind.
        governor.tick(Auto, ms(31.0), true, true);
        assert_eq!(governor.percent().get(), 85);
        for tick in 0..10 * FPS {
            governor.tick(
                Auto,
                (tick % 4 == 0).then(|| ms(22.0)).flatten(),
                true,
                true,
            );
        }
        assert_eq!(governor.percent().get(), 85);
    }

    /// A model that is a small part of the frame: 29.5 ms at 100 and 26.8 at 85, where
    /// the worst case would expect 37 ms back at 100.
    #[test]
    fn it_steps_up_by_what_the_step_down_saved_and_not_the_worst_case() {
        let mut governor = governor();
        assert_eq!(run(&mut governor, 12, 29.5), 85, "three long frames");
        // 26.8 ms here was 29.5 there: over 78% of the budget, so it stays.
        assert_eq!(run(&mut governor, 3000, 26.8), 85);
        // 23 ms here is expected to be 25.3 there, where the worst case says 31.8.
        assert_eq!(run(&mut governor, 4 * FPS, 23.0), 85, "not yet");
        assert_eq!(run(&mut governor, 2 * FPS, 23.0), 100);
    }

    #[test]
    fn a_drawer_with_no_budget_stays_at_full_size() {
        let mut governor = Governor::new(Duration::ZERO);
        for _ in 0..100 {
            governor.tick(Auto, ms(40.0), true, true);
        }
        assert_eq!(governor.percent().get(), 100);
    }

    #[test]
    fn the_setting_reads_zero_as_the_choice_and_a_percentage_as_itself() {
        assert_eq!(Mode::of(0), Mode::Auto);
        assert_eq!(Mode::of(100), Mode::Fixed(100));
        assert_eq!(Mode::of(67), Mode::Fixed(67));
        assert_eq!(Mode::of(80), Mode::Fixed(80), "any whole percentage");
        assert_eq!(Mode::of(20), Mode::Fixed(50), "refused before it gets here");
        assert_eq!(
            jstream_protocol::MODEL_RESOLUTIONS[1..],
            STEPS,
            "the protocol's sizes are these, after its 0"
        );
    }
}
