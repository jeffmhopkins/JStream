//! The state machine that walks a show (ST1): its clock, pausing, Continue, Back, Skip,
//! going to a bookmark, loops, arming and the end of the show. Pure: the caller gives
//! the animation clock's time in nanoseconds and runs the [`Effect`]s it gets back.

use std::collections::BTreeMap;

use super::plan::{Plan, ShowKeys};
use super::resolve::Edge;

/// Where the runner is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not running.
    Stopped,
    /// Show time runs with the clock.
    Playing,
    /// Waiting for Continue at a point; `bookmark` is the pausing bookmark it waits at,
    /// if any (a pausing landing with no bookmark, such as Back to the start, has none).
    Paused {
        /// The bookmark, by index.
        bookmark: Option<usize>,
        /// When it began waiting, on the animation clock.
        since_ns: i64,
    },
    /// The program scene was changed by hand: show time stops where it is until
    /// Continue (on to the next program clip) or back to script.
    OffScript {
        /// When, on the animation clock.
        since_ns: i64,
    },
    /// It reached its end with [`super::EndOfShow::Stop`]: program keeps its last scene.
    Ended,
}

/// How a jump lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    /// Put up the state just before the point and wait there.
    Pausing,
    /// Put up the state at the point and play on.
    PlayOn,
}

impl Landing {
    /// The side of the point it resolves.
    pub fn edge(self) -> Edge {
        match self {
            Landing::Pausing => Edge::Before,
            Landing::PlayOn => Edge::At,
        }
    }
}

/// What the runner last put up: the show's own view of program, its overlays, its
/// sequences and the state events it applied. Diffing the next [`super::Spanning`]
/// against it gives the effects; it is never read from the engine, so a hand change is
/// left alone.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Applied<T: Ord> {
    /// The program clip it cut to, if any.
    pub program: Option<usize>,
    /// The keyframe marker it loaded, if any.
    pub keyframe: Option<usize>,
    /// The overlay clips it has up, by index.
    pub overlays: Vec<usize>,
    /// The sequence clips it has playing, by sequence.
    pub sequences: BTreeMap<T, usize>,
    /// The last state event it applied, per state key, by event index. The key is the
    /// event's `state_key` rendered with `Debug`, so the map needs no bound on the key.
    pub states: BTreeMap<String, usize>,
}

/// One thing for the engine to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect<K: ShowKeys> {
    /// Put this program clip's scene on program, through `transition` (`None`: the
    /// engine's own choice). `offset_ms` > 0 on a timed clip asks for its media to seek
    /// there.
    Program {
        /// The clip, by index.
        clip: usize,
        /// Its scene.
        scene: K::Scene,
        /// The transition for this cut.
        transition: Option<K::Preset>,
        /// How far into the clip, in milliseconds.
        offset_ms: u32,
    },
    /// The program clip stays but its media must seek: a jump or a wrap within a timed
    /// clip.
    Seek {
        /// The clip, by index.
        clip: usize,
        /// Its scene.
        scene: K::Scene,
        /// Where, in milliseconds into the clip.
        offset_ms: u32,
    },
    /// Load a keyframe of the scene on program.
    Keyframe {
        /// The marker, by index.
        marker: usize,
        /// The keyframe scene.
        keyframe: K::Scene,
        /// The Move's preset.
        preset: Option<K::Preset>,
    },
    /// Take an overlay clip down.
    OverlayOff {
        /// The clip, by index.
        clip: usize,
        /// Its track.
        track: K::Track,
        /// Fade, in milliseconds: the clip's fade out at its end, a short fade on a jump.
        fade_ms: u32,
    },
    /// Put an overlay clip up (or seek it, if it is already up and timed).
    OverlayOn {
        /// The clip, by index.
        clip: usize,
        /// Its track.
        track: K::Track,
        /// Its scene.
        scene: K::Scene,
        /// Fade, in milliseconds.
        fade_ms: u32,
        /// How far into the clip, in milliseconds.
        offset_ms: u32,
    },
    /// Stop a sequence the show started.
    SequenceStop {
        /// The clip, by index.
        clip: usize,
        /// The sequence.
        timeline: K::Timeline,
    },
    /// Play a sequence from `offset_ms` (a seek when > 0), replacing any play of it.
    SequencePlay {
        /// The clip, by index.
        clip: usize,
        /// The sequence.
        timeline: K::Timeline,
        /// Where in it, in milliseconds.
        offset_ms: u32,
    },
    /// Run an event's action.
    Action {
        /// The event, by index.
        event: usize,
        /// Its action.
        action: K::Action,
    },
    /// The show paused at a point.
    Paused {
        /// The pausing bookmark, by index, if any.
        bookmark: Option<usize>,
        /// Where, in show time.
        at_ms: u32,
    },
    /// The show played on from a pause.
    Resumed {
        /// Where, in show time.
        at_ms: u32,
    },
    /// The show jumped.
    Jumped {
        /// Where from, in show time.
        from_ms: u32,
        /// Where to.
        to_ms: u32,
    },
    /// A loop wrapped to its start.
    Looped {
        /// The loop, by index.
        region: usize,
        /// The pass it starts: 1 the first repeat.
        pass: u64,
    },
    /// The show reached its end and stopped there.
    Ended,
}

/// An effect and the exact time it falls due on the animation clock, so the engine runs
/// it as of that time however late its tick ran.
#[derive(Clone, Debug, PartialEq)]
pub struct Step<K: ShowKeys> {
    /// When, on the animation clock, in nanoseconds.
    pub at_ns: i64,
    /// What.
    pub effect: Effect<K>,
}

/// What Continue would do now, for "Next" in the panel and `studio.get`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing: stopped, ended, or no pausing bookmark ahead.
    Nothing,
    /// Play on from the pause.
    Resume,
    /// Leave the loop the play head is in (by index).
    LeaveLoop(usize),
    /// Jump to this show time and play on (early Continue, an armed spot, or Continue
    /// while off script).
    Jump(u32),
}

/// The runner: one show's play head. The plan is passed to every call rather than held,
/// so an edit to a running show takes effect at the next call.
#[derive(Clone, Debug)]
pub struct Runner<T: Ord> {
    status: Status,
    /// Show time at `anchor_clock_ns`, in nanoseconds.
    anchor_show_ns: i64,
    /// The clock time show time was last anchored at.
    anchor_clock_ns: i64,
    /// Points at or before this show time (ms) are done; `None` before any point.
    done_ms: Option<u32>,
    /// A pause at this point is skipped once: the bookmark just resumed from, or the
    /// loop start just wrapped to.
    resume_ms: Option<u32>,
    /// The loop the play head is in, by index, and its pass.
    looping: Option<(usize, u64)>,
    /// Whether Continue asked to finish this pass and leave.
    leaving: bool,
    /// A spot armed for the next Continue, in show time.
    armed_ms: Option<u32>,
    applied: Applied<T>,
}

impl<T: Ord + Clone> Runner<T> {
    /// A stopped runner.
    pub fn new() -> Self {
        todo!()
    }

    /// Starts the show at `at_ms` with `landing`, from whatever was up before (a fresh
    /// [`Applied`]). `Runner::start(plan, 0, Landing::PlayOn, now)` plays from the top.
    pub fn start<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ms: u32,
        landing: Landing,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        todo!()
    }

    /// Advances show time to `now_ns` and returns what fell due, in order.
    pub fn tick<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        todo!()
    }

    /// Continue: resume from a pause; leave a loop; jump to an armed spot; off script, on
    /// to the next program clip; else jump past the next pausing bookmark and play on.
    /// The engine ignores it during a transition.
    pub fn continue_<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        todo!()
    }

    /// Back: jump to the pausing bookmark before the play head (before the one it waits
    /// at), else the start, with a pausing landing.
    pub fn back<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        todo!()
    }

    /// Skip: jump to the next pausing bookmark after the play head, with a pausing
    /// landing, without playing what is between. Nothing if there is none.
    pub fn skip<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        todo!()
    }

    /// Jumps to bookmark `bookmark` (by index), landing as it pauses or not.
    pub fn go_to<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        bookmark: usize,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        todo!()
    }

    /// Jumps to `at_ms` with `landing`.
    pub fn jump<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ms: u32,
        landing: Landing,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        todo!()
    }

    /// Arms `at_ms` for the next Continue (a ruler click while running); `None` disarms.
    pub fn arm(&mut self, at_ms: Option<u32>) {
        todo!()
    }

    /// The program scene was changed by hand: stop show time where it is. The show's
    /// view of program is forgotten, so going back to script cuts to its clip again.
    pub fn off_script(&mut self, now_ns: i64) {
        todo!()
    }

    /// Back to script: land on the play head's own point, playing on.
    pub fn back_to_script<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        todo!()
    }

    /// Stops: effects that stop the sequences it started and take its overlays down.
    /// Program and state-event values stay.
    pub fn stop<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        todo!()
    }

    /// Where it is.
    pub fn status(&self) -> Status {
        self.status
    }

    /// Show time at `now_ns`, in milliseconds.
    pub fn position_ms(&self, now_ns: i64) -> u32 {
        todo!()
    }

    /// The loop it is in and the pass, if any.
    pub fn looping(&self) -> Option<(usize, u64)> {
        self.looping
    }

    /// The armed spot, if any.
    pub fn armed_ms(&self) -> Option<u32> {
        self.armed_ms
    }

    /// What it last put up.
    pub fn applied(&self) -> &Applied<T> {
        &self.applied
    }

    /// What Continue would do now.
    pub fn next<K: ShowKeys<Timeline = T>>(&self, plan: &Plan<K>, now_ns: i64) -> Next {
        todo!()
    }

    /// When it next needs a tick, on the animation clock: its next point, or a pausing
    /// bookmark's limit running out. `None` when nothing is due (stopped, ended, paused
    /// with no limit, off script).
    pub fn next_due<K: ShowKeys<Timeline = T>>(&self, plan: &Plan<K>) -> Option<i64> {
        todo!()
    }
}

impl<T: Ord + Clone> Default for Runner<T> {
    fn default() -> Self {
        Self::new()
    }
}
