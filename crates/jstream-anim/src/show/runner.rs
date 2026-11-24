//! The state machine that walks a show (ST1): its clock, pausing, Continue, Back, Skip,
//! going to a bookmark, loops, arming and the end of the show. Pure: the caller gives
//! the animation clock's time in nanoseconds and runs the [`Effect`]s it gets back.

use std::collections::BTreeMap;

use super::plan::{Bookmark, EndOfShow, Leave, Plan, ShowKeys};
use super::resolve::{Edge, Spanning, spanning, state_events_before};
use crate::CueAction;

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
#[derive(Clone, Debug, PartialEq)]
pub struct Applied<T: Ord> {
    /// The program clip it cut to, if any.
    pub program: Option<usize>,
    /// The keyframe marker it loaded, if any.
    pub keyframe: Option<usize>,
    /// The overlay clips it has up, by index.
    pub overlays: Vec<usize>,
    /// The audio clips it has up, by index.
    pub audio: Vec<usize>,
    /// The sequence clips it has playing, by sequence.
    pub sequences: BTreeMap<T, usize>,
    /// The last state event it applied, per state key, by event index. The key is the
    /// event's `state_key` rendered with `Debug`, so the map needs no bound on the key.
    pub states: BTreeMap<String, usize>,
}

impl<T: Ord> Default for Applied<T> {
    fn default() -> Self {
        Applied {
            program: None,
            keyframe: None,
            overlays: Vec::new(),
            audio: Vec::new(),
            sequences: BTreeMap::new(),
            states: BTreeMap::new(),
        }
    }
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
        /// The keyframe scene; `None` for the scene's own values.
        keyframe: Option<K::Scene>,
        /// The Move's preset.
        preset: Option<K::Preset>,
    },
    /// Take an overlay clip down.
    OverlayOff {
        /// The clip, by index.
        clip: usize,
        /// Its track.
        track: K::Track,
        /// Its scene.
        scene: K::Scene,
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
    /// Bring an audio clip's source up, `offset_ms` into the clip (its media sought
    /// there): the clip came up, or it stays up across a jump or a loop wrap. Its fader
    /// follows [`AudioClip::gain_db_at`](super::AudioClip::gain_db_at) from then on.
    AudioOn {
        /// The clip, by index.
        clip: usize,
        /// Its track.
        track: K::Track,
        /// Its source.
        source: K::Source,
        /// How far into the clip, in milliseconds.
        offset_ms: u32,
    },
    /// Take an audio clip's source down, its fader falling to the floor over `fade_ms`
    /// from where it is: 0 at the clip's end, whose own fade out already brought it
    /// down; a short fade (about 0.3 s) when a jump, a wrap or a stop cuts it off.
    AudioOff {
        /// The clip, by index.
        clip: usize,
        /// Its track.
        track: K::Track,
        /// Its source.
        source: K::Source,
        /// Fade, in milliseconds.
        fade_ms: u32,
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
    /// Whether it is paused by hand ([`Runner::pause`]): show time and its sequences
    /// hold where they were, and playing on puts them back from there.
    held: bool,
    applied: Applied<T>,
}

impl<T: Ord + Clone> Runner<T> {
    /// A stopped runner.
    pub fn new() -> Self {
        Runner {
            status: Status::Stopped,
            anchor_show_ns: 0,
            anchor_clock_ns: 0,
            done_ms: None,
            resume_ms: None,
            looping: None,
            leaving: false,
            armed_ms: None,
            held: false,
            applied: Applied::default(),
        }
    }

    /// Starts the show at `at_ms` with `landing`, from whatever was up before (a fresh
    /// [`Applied`]). `Runner::start(plan, 0, Landing::PlayOn, now)` plays from the top.
    /// A program cut it makes uses the show's default transition.
    pub fn start<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ms: u32,
        landing: Landing,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        *self = Self::new();
        let mut out = Vec::new();
        let arrival = Arrival {
            at_ms,
            landing,
            transition: plan.default_transition.clone(),
            jumped_from: None,
        };
        self.land(plan, arrival, now_ns, &mut out);
        out
    }

    /// Starts the show at its `bookmark`th bookmark, landing as a jump there would: the
    /// bookmark's pause or play on, and its arrive transition for a program cut. A
    /// bookmark the plan doesn't have starts from the top.
    pub fn start_at_bookmark<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        bookmark: usize,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let Some(b) = plan.bookmarks.get(bookmark) else {
            return self.start(plan, 0, Landing::PlayOn, now_ns);
        };
        *self = Self::new();
        let mut out = Vec::new();
        let (at_ms, landing, transition) = bookmark_landing(plan, b);
        let arrival = Arrival {
            at_ms,
            landing,
            transition,
            jumped_from: None,
        };
        self.land(plan, arrival, now_ns, &mut out);
        out
    }

    /// Advances show time to `now_ns` and returns what fell due, in order. A pause at a
    /// bookmark with a limit that has run out resumes first, as of the moment it ran out.
    pub fn tick<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if let Some(due) = self.limit_due(plan)
            && now_ns >= due
        {
            self.resume(plan, due, &mut out);
        }
        self.run(plan, now_ns, &mut out);
        out
    }

    /// Continue: resume from a pause; leave a loop; jump to an armed spot; off script, on
    /// to the next program clip; else jump past the next pausing bookmark and play on.
    /// The engine ignores it during a transition. An armed spot wins over everything
    /// else. Resuming returns only [`Effect::Resumed`]: what sits at the point happens
    /// on the next tick, as after a play-on landing.
    pub fn continue_<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        match self.next(plan, now_ns) {
            Next::Nothing => {}
            Next::Resume => self.resume(plan, now_ns, &mut out),
            Next::LeaveLoop(i) => {
                let Some(region) = plan.loops.get(i) else {
                    return out;
                };
                match region.leave {
                    Leave::AtOnce => {
                        let to = region.to_ms;
                        let pausing = pausing_at(plan, to);
                        let landing = if pausing.is_some() {
                            Landing::Pausing
                        } else {
                            Landing::PlayOn
                        };
                        let transition = arrive_at(plan, to);
                        self.jump_inner(plan, to, landing, transition, now_ns, &mut out);
                    }
                    Leave::FinishPass => self.leaving = true,
                }
            }
            Next::Jump(t) => {
                let transition = arrive_at(plan, t);
                self.jump_inner(plan, t, Landing::PlayOn, transition, now_ns, &mut out);
            }
        }
        out
    }

    /// Back: jump to the pausing bookmark before the play head (before the one it waits
    /// at), else the start, with a pausing landing. Nothing while stopped.
    pub fn back<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.status == Status::Stopped {
            return out;
        }
        let position = self.position_ns(now_ns);
        let target = plan
            .bookmarks
            .iter()
            .rev()
            .find(|b| b.pausing && ms_ns(b.at_ms) < position)
            .map_or(0, |b| b.at_ms);
        let transition = arrive_at(plan, target);
        self.jump_inner(plan, target, Landing::Pausing, transition, now_ns, &mut out);
        out
    }

    /// Skip: jump to the next pausing bookmark after the play head, with a pausing
    /// landing, without playing what is between. Nothing if there is none, or while
    /// stopped.
    pub fn skip<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.status == Status::Stopped {
            return out;
        }
        if let Some(t) = self.next_pausing(plan, now_ns) {
            let transition = arrive_at(plan, t);
            self.jump_inner(plan, t, Landing::Pausing, transition, now_ns, &mut out);
        }
        out
    }

    /// Jumps to bookmark `bookmark` (by index), landing as it pauses or not. Nothing for
    /// an index the show doesn't have, or while stopped.
    pub fn go_to<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        bookmark: usize,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.status == Status::Stopped {
            return out;
        }
        if let Some(b) = plan.bookmarks.get(bookmark) {
            let (t, landing, transition) = bookmark_landing(plan, b);
            self.jump_inner(plan, t, landing, transition, now_ns, &mut out);
        }
        out
    }

    /// Jumps to `at_ms` (at most the show's length) with `landing`. A program cut it
    /// makes uses the show's default transition. Nothing while stopped.
    pub fn jump<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ms: u32,
        landing: Landing,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.status == Status::Stopped {
            return out;
        }
        let transition = plan.default_transition.clone();
        self.jump_inner(plan, at_ms, landing, transition, now_ns, &mut out);
        out
    }

    /// Arms `at_ms` for the next Continue (a ruler click while running); `None` disarms.
    pub fn arm(&mut self, at_ms: Option<u32>) {
        self.armed_ms = at_ms;
    }

    /// The program scene was changed by hand: stop show time where it is. The show's
    /// view of program is forgotten, so going back to script cuts to its clip again.
    /// Nothing while stopped or ended.
    pub fn off_script(&mut self, now_ns: i64) {
        match self.status {
            Status::Stopped | Status::Ended => {}
            Status::OffScript { .. } => {
                self.applied.program = None;
                self.applied.keyframe = None;
            }
            Status::Playing | Status::Paused { .. } => {
                self.anchor_show_ns = self.position_ns(now_ns);
                self.anchor_clock_ns = now_ns;
                self.status = Status::OffScript { since_ns: now_ns };
                self.applied.program = None;
                self.applied.keyframe = None;
            }
        }
    }

    /// Back to script: land on the play head's own point, playing on. Nothing unless off
    /// script.
    pub fn back_to_script<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if !matches!(self.status, Status::OffScript { .. }) {
            return out;
        }
        let t = self.position_ms(now_ns);
        let transition = plan.default_transition.clone();
        self.put_landing_state(plan, t, Edge::At, transition, now_ns, &mut out);
        self.anchor_clock_ns = now_ns;
        self.status = Status::Playing;
        if self.done_ms.is_none_or(|done| done < t) {
            self.resume_ms = Some(t);
        }
        out
    }

    /// Pause by hand: show time holds where the play head is, between points or at one,
    /// and the sequences it started stop ([`Effect::SequenceStop`]); program, overlays and
    /// state values stay, and program runs on (its media is not held). Continue (or a
    /// jump) plays on from there: the sequences play again from where they were, as a
    /// [`Landing::PlayOn`] put-up without seeking the program clip.
    /// Nothing unless it is playing.
    pub fn pause<K: ShowKeys<Timeline = T>>(
        &mut self,
        _plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.status != Status::Playing {
            return out;
        }
        let at_ns = self.position_ns(now_ns);
        self.anchor_show_ns = at_ns;
        self.anchor_clock_ns = now_ns;
        self.status = Status::Paused {
            bookmark: None,
            since_ns: now_ns,
        };
        self.held = true;
        for (timeline, clip) in std::mem::take(&mut self.applied.sequences) {
            out.push(Step {
                at_ns: now_ns,
                effect: Effect::SequenceStop { clip, timeline },
            });
        }
        out.push(Step {
            at_ns: now_ns,
            effect: Effect::Paused {
                bookmark: None,
                at_ms: self.position_ms(now_ns),
            },
        });
        out
    }

    /// Whether it is paused by hand ([`Runner::pause`]).
    pub fn held(&self) -> bool {
        self.held
    }

    /// Plays on from a pause by hand, as Continue would but never to an armed spot.
    /// Nothing unless it is paused by hand.
    pub fn unpause<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if self.held && matches!(self.status, Status::Paused { .. }) {
            self.resume(plan, now_ns, &mut out);
        }
        out
    }

    /// Stops: effects that stop the sequences it started and take its overlays down.
    /// Program and state-event values stay.
    pub fn stop<K: ShowKeys<Timeline = T>>(&mut self, plan: &Plan<K>, now_ns: i64) -> Vec<Step<K>> {
        let mut out = Vec::new();
        for (timeline, &clip) in &self.applied.sequences {
            let effect = Effect::SequenceStop {
                clip,
                timeline: timeline.clone(),
            };
            out.push(Step {
                at_ns: now_ns,
                effect,
            });
        }
        for &clip in &self.applied.overlays {
            if let Some(c) = plan.overlays.get(clip) {
                let effect = Effect::OverlayOff {
                    clip,
                    track: c.track.clone(),
                    scene: c.scene.clone(),
                    fade_ms: c.fade_out_ms.min(SHORT_FADE_MS),
                };
                out.push(Step {
                    at_ns: now_ns,
                    effect,
                });
            }
        }
        for &clip in &self.applied.audio {
            if let Some(c) = plan.audio.get(clip) {
                out.push(Step {
                    at_ns: now_ns,
                    effect: Effect::AudioOff {
                        clip,
                        track: c.track.clone(),
                        source: c.source.clone(),
                        fade_ms: SHORT_FADE_MS,
                    },
                });
            }
        }
        *self = Self::new();
        out
    }

    /// The show was edited while it runs: carries what it has up from `old` over to
    /// `new` by what each thing is (a program clip and an overlay by scene and track, a
    /// sequence by its timeline, a bookmark by ID), so unchanged things stay up with no
    /// flicker, then puts up what `new` spans at the play head as playing into it would
    /// (nothing re-seeks or plays again): an overlay or sequence whose clip is gone or no longer covers the
    /// head goes, one that now does comes up, and a changed program clip cuts. Show time
    /// stays where it is; a show edited shorter than where it is ends there.
    pub fn replan<K: ShowKeys<Timeline = T>>(
        &mut self,
        old: &Plan<K>,
        new: &Plan<K>,
        now_ns: i64,
    ) -> Vec<Step<K>> {
        let mut out = Vec::new();
        if matches!(self.status, Status::Stopped | Status::Ended) {
            return out;
        }
        let at = self.position_ms(now_ns);
        // Waiting at a point shows what is just before it (and the clip a pausing
        // bookmark there starts), as the pause put up.
        let waiting = matches!(self.status, Status::Paused { .. }) && !self.held;
        let mut target = spanning(new, at, if waiting { Edge::Before } else { Edge::At });
        if waiting
            && pausing_at(new, at).is_some()
            && let Some(clip) = clip_starting_at(new, at)
        {
            target.program = Some((clip, 0));
            target.keyframe = None;
        }
        // Program: the same scene stays up as the new clip.
        let program = self.applied.program.and_then(|i| old.program.get(i));
        self.applied.program = match (program, target.program) {
            (Some(was), Some((j, _)))
                if new.program.get(j).is_some_and(|c| c.scene == was.scene) =>
            {
                Some(j)
            }
            _ => None,
        };
        // Keyframe: the same marker, if it is still there.
        let keyframe = self.applied.keyframe.and_then(|i| old.keyframes.get(i));
        self.applied.keyframe = keyframe.and_then(|m| {
            new.keyframes
                .iter()
                .position(|n| n.at_ms == m.at_ms && n.keyframe == m.keyframe)
        });
        // Overlays: kept by track and scene where the new plan spans one; the rest go.
        for clip in std::mem::take(&mut self.applied.overlays) {
            let Some(c) = old.overlays.get(clip) else {
                continue;
            };
            let kept = target.overlays.keys().copied().find(|&j| {
                new.overlays
                    .get(j)
                    .is_some_and(|n| n.track == c.track && n.scene == c.scene)
                    && !self.applied.overlays.contains(&j)
            });
            match kept {
                Some(j) => self.applied.overlays.push(j),
                None => out.push(Step {
                    at_ns: now_ns,
                    effect: Effect::OverlayOff {
                        clip,
                        track: c.track.clone(),
                        scene: c.scene.clone(),
                        fade_ms: c.fade_out_ms.min(SHORT_FADE_MS),
                    },
                }),
            }
        }
        // Audio: kept by track and source where the new plan spans one; the rest go.
        for clip in std::mem::take(&mut self.applied.audio) {
            let Some(c) = old.audio.get(clip) else {
                continue;
            };
            let kept = target.audio.keys().copied().find(|&j| {
                new.audio
                    .get(j)
                    .is_some_and(|n| n.track == c.track && n.source == c.source)
                    && !self.applied.audio.contains(&j)
            });
            match kept {
                Some(j) => self.applied.audio.push(j),
                None => out.push(Step {
                    at_ns: now_ns,
                    effect: Effect::AudioOff {
                        clip,
                        track: c.track.clone(),
                        source: c.source.clone(),
                        fade_ms: SHORT_FADE_MS,
                    },
                }),
            }
        }
        // Sequences: kept by timeline where the new plan spans one; the rest stop.
        for (timeline, clip) in std::mem::take(&mut self.applied.sequences) {
            match target.sequences.get(&timeline) {
                Some(&(j, _)) => {
                    self.applied.sequences.insert(timeline, j);
                }
                None => out.push(Step {
                    at_ns: now_ns,
                    effect: Effect::SequenceStop { clip, timeline },
                }),
            }
        }
        // State events: the last one per key the new plan has before the head counts as
        // applied, so an edit never fires one again.
        let mut states = BTreeMap::new();
        for i in state_events_before(new, at.saturating_add(1)) {
            if let Some(key) = new.events.get(i).and_then(|e| e.action.state_key()) {
                let key = format!("{key:?}");
                if self.applied.states.contains_key(&key) {
                    states.insert(key, i);
                }
            }
        }
        self.applied.states = states;
        // Where it waits, loops and is armed, in the new plan's terms.
        if let Status::Paused {
            bookmark: Some(b),
            since_ns,
        } = self.status
        {
            let id = old.bookmarks.get(b).map(|b| &b.id);
            self.status = Status::Paused {
                bookmark: id.and_then(|id| new.bookmark(id)),
                since_ns,
            };
        }
        let pass = self.looping.map_or(0, |(_, pass)| pass);
        self.looping = new.loop_at(at).map(|i| (i, pass));
        self.put_up(new, &target, Mode::Play, None, now_ns, &mut out);
        let length = new.length_ms();
        if at >= length && self.status == Status::Playing {
            self.done_ms = Some(length);
            self.end(new, length, now_ns, &mut out);
        } else if self.done_ms.is_some_and(|d| d > at) {
            self.done_ms = at.checked_sub(1);
        }
        out
    }

    /// Where it is.
    pub fn status(&self) -> Status {
        self.status
    }

    /// Show time at `now_ns`, in milliseconds.
    pub fn position_ms(&self, now_ns: i64) -> u32 {
        let ms = self.position_ns(now_ns).max(0) / NS_PER_MS;
        u32::try_from(ms).unwrap_or(u32::MAX)
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
        if matches!(self.status, Status::Stopped | Status::Ended) {
            return Next::Nothing;
        }
        if let Some(at) = self.armed_ms {
            return Next::Jump(at);
        }
        match self.status {
            Status::Stopped | Status::Ended => Next::Nothing,
            Status::Paused { .. } => Next::Resume,
            Status::OffScript { .. } => {
                let position = self.position_ns(now_ns);
                (0..plan.program.len())
                    .map(|i| plan.program_start(i))
                    .find(|&start| ms_ns(start) > position)
                    .map_or(Next::Nothing, Next::Jump)
            }
            Status::Playing => {
                if let Some((i, _)) = self.looping
                    && !self.leaving
                    && i < plan.loops.len()
                {
                    return Next::LeaveLoop(i);
                }
                self.next_pausing(plan, now_ns)
                    .map_or(Next::Nothing, Next::Jump)
            }
        }
    }

    /// When it next needs a tick, on the animation clock: its next point, or a pausing
    /// bookmark's limit running out. `None` when nothing is due (stopped, ended, paused
    /// with no limit, off script).
    pub fn next_due<K: ShowKeys<Timeline = T>>(&self, plan: &Plan<K>) -> Option<i64> {
        match self.status {
            Status::Playing => next_point(plan, self.done_ms).map(|p| self.due_ns(p)),
            Status::Paused { .. } => self.limit_due(plan),
            Status::Stopped | Status::OffScript { .. } | Status::Ended => None,
        }
    }

    /// Show time at `now_ns`, in nanoseconds.
    fn position_ns(&self, now_ns: i64) -> i64 {
        match self.status {
            Status::Playing => self
                .anchor_show_ns
                .saturating_add(now_ns.saturating_sub(self.anchor_clock_ns)),
            _ => self.anchor_show_ns,
        }
    }

    /// The clock time point `p` falls due at, from the current anchor.
    fn due_ns(&self, p: u32) -> i64 {
        self.anchor_clock_ns
            .saturating_add(ms_ns(p).saturating_sub(self.anchor_show_ns))
    }

    /// Anchors show time `show_ms` at clock time `clock_ns`.
    fn anchor(&mut self, show_ms: u32, clock_ns: i64) {
        self.anchor_show_ns = ms_ns(show_ms);
        self.anchor_clock_ns = clock_ns;
    }

    /// When the limit of the bookmark it is paused at runs out, if it has one.
    fn limit_due<K: ShowKeys<Timeline = T>>(&self, plan: &Plan<K>) -> Option<i64> {
        let Status::Paused {
            bookmark: Some(b),
            since_ns,
        } = self.status
        else {
            return None;
        };
        let limit = plan.bookmarks.get(b)?.limit_ms?;
        Some(since_ns.saturating_add(ms_ns(limit)))
    }

    /// The next pausing bookmark strictly after the play head, in show time.
    fn next_pausing<K: ShowKeys<Timeline = T>>(&self, plan: &Plan<K>, now_ns: i64) -> Option<u32> {
        let position = self.position_ns(now_ns);
        plan.bookmarks
            .iter()
            .find(|b| b.pausing && ms_ns(b.at_ms) > position)
            .map(|b| b.at_ms)
    }

    /// Plays on from a pause, as of `at_ns`: the point it waits at does not pause again.
    fn resume<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let at_ms = self.position_ms(at_ns);
        self.status = Status::Playing;
        self.anchor_clock_ns = at_ns;
        self.resume_ms = Some(at_ms);
        out.push(Step {
            at_ns,
            effect: Effect::Resumed { at_ms },
        });
        if std::mem::take(&mut self.held) {
            // What the pause held plays on from where it was; program ran on through
            // the pause, so it isn't sought back.
            let target = spanning(plan, at_ms, Edge::At);
            self.put_up(plan, &target, Mode::Jump { seek: false }, None, at_ns, out);
        }
    }

    /// Processes the points that fell due by `now_ns`, while playing. It always ends:
    /// an end-of-show jump that plays on lands strictly before the end ([`Self::end`]),
    /// so each lap takes show time, and clock time, to come round again.
    fn run<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        now_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let length = plan.length_ms();
        while self.status == Status::Playing {
            let Some(p) = next_point(plan, self.done_ms) else {
                break;
            };
            if ms_ns(p) > self.position_ns(now_ns) {
                break;
            }
            let due = self.due_ns(p);
            self.point(plan, p, due, length, out);
        }
    }

    /// Everything at point `p`, in the module's order, due at `due` on the clock.
    fn point<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        p: u32,
        due: i64,
        length: u32,
        out: &mut Vec<Step<K>>,
    ) {
        // 1. A loop it is in, and is not leaving, that ends here wraps.
        if let Some((i, pass)) = self.looping
            && !self.leaving
            && let Some(region) = plan.loops.get(i)
            && region.to_ms == p
        {
            let from = region.from_ms;
            let pass = pass.saturating_add(1);
            self.anchor(from, due);
            self.looping = Some((i, pass));
            out.push(Step {
                at_ns: due,
                effect: Effect::Looped { region: i, pass },
            });
            // A sequence clip that ends here has run out: it stops as playing on would
            // stop it, and plays again from the loop's start if it spans it.
            let here = spanning(plan, p, Edge::At);
            let ended: Vec<(T, usize)> = self
                .applied
                .sequences
                .iter()
                .filter(|&(timeline, clip)| {
                    here.sequences
                        .get(timeline)
                        .is_none_or(|&(c, _)| c != *clip)
                })
                .map(|(timeline, &clip)| (timeline.clone(), clip))
                .collect();
            for (timeline, clip) in ended {
                self.applied.sequences.remove(&timeline);
                out.push(Step {
                    at_ns: due,
                    effect: Effect::SequenceStop { clip, timeline },
                });
            }
            let target = spanning(plan, from, Edge::At);
            let transition = region
                .wrap
                .clone()
                .or_else(|| plan.default_transition.clone());
            self.put_up(plan, &target, Mode::Wrap, transition, due, out);
            self.done_ms = from.checked_sub(1);
            self.resume_ms = Some(from);
            return;
        }
        // 2. A pausing bookmark pauses before anything here, but the program clip that
        //    starts here goes up first, so the show waits on the scene it marks.
        if self.resume_ms != Some(p)
            && let Some(b) = pausing_at(plan, p)
        {
            if let Some(clip) = clip_starting_at(plan, p) {
                let mut target = spanning(plan, p, Edge::Before);
                target.program = Some((clip, 0));
                target.keyframe = None;
                self.put_up(plan, &target, Mode::Play, None, due, out);
            }
            self.anchor(p, due);
            self.status = Status::Paused {
                bookmark: Some(b),
                since_ns: due,
            };
            out.push(Step {
                at_ns: due,
                effect: Effect::Paused {
                    bookmark: Some(b),
                    at_ms: p,
                },
            });
            return;
        }
        // 3. The spanning state.
        let target = spanning(plan, p, Edge::At);
        self.put_up(plan, &target, Mode::Play, None, due, out);
        // 4. The events.
        for (i, event) in plan.events.iter().enumerate() {
            if event.at_ms == p {
                self.fire(i, &event.action, due, out);
            }
        }
        self.done_ms = Some(p);
        if self.resume_ms.is_some_and(|r| r <= p) {
            self.resume_ms = None;
        }
        let here = plan.loop_at(p);
        if self.looping.map(|(i, _)| i) != here {
            self.looping = here.map(|i| (i, 0));
            self.leaving = false;
        }
        // 5. The end.
        if p >= length {
            self.end(plan, p, due, out);
        }
    }

    /// The show reached its end at `p`.
    fn end<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        p: u32,
        due: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let target = match &plan.end {
            EndOfShow::Stop => None,
            EndOfShow::Loop => Some(plan.bookmarks.first().map_or_else(
                || (0, Landing::PlayOn, plan.default_transition.clone()),
                |b| bookmark_landing(plan, b),
            )),
            EndOfShow::GoTo(id) => plan
                .bookmark(id)
                .and_then(|i| plan.bookmarks.get(i))
                .map(|b| bookmark_landing(plan, b)),
        };
        // A play-on landing at the end itself would reach the end again at once, so the
        // show ends there instead of spinning.
        let target = target.filter(|&(t, landing, _)| landing == Landing::Pausing || t < p);
        match target {
            Some((t, landing, transition)) => {
                let arrival = Arrival {
                    at_ms: t,
                    landing,
                    transition,
                    jumped_from: Some(p),
                };
                self.land(plan, arrival, due, out);
            }
            None => {
                self.anchor(p, due);
                self.status = Status::Ended;
                out.push(Step {
                    at_ns: due,
                    effect: Effect::Ended,
                });
            }
        }
    }

    /// Runs an event's action and records a state event's value.
    fn fire<K: ShowKeys<Timeline = T>>(
        &mut self,
        event: usize,
        action: &K::Action,
        at_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        if let Some(key) = action.state_key() {
            self.applied.states.insert(format!("{key:?}"), event);
        }
        out.push(Step {
            at_ns,
            effect: Effect::Action {
                event,
                action: action.clone(),
            },
        });
    }

    /// A jump from the play head to `at_ms`.
    fn jump_inner<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        at_ms: u32,
        landing: Landing,
        transition: Option<K::Preset>,
        now_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let arrival = Arrival {
            at_ms,
            landing,
            transition,
            jumped_from: Some(self.position_ms(now_ns)),
        };
        self.land(plan, arrival, now_ns, out);
    }

    /// Lands as `arrival` says, as of `at_ns`: [`Effect::Jumped`] when it jumped, the
    /// state there, then the pause or play on.
    fn land<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        arrival: Arrival<K::Preset>,
        at_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let Arrival {
            at_ms,
            landing,
            transition,
            jumped_from,
        } = arrival;
        let t = at_ms.min(plan.length_ms());
        if let Some(from_ms) = jumped_from {
            out.push(Step {
                at_ns,
                effect: Effect::Jumped { from_ms, to_ms: t },
            });
        }
        self.put_landing_state(plan, t, landing.edge(), transition, at_ns, out);
        self.anchor(t, at_ns);
        self.done_ms = t.checked_sub(1);
        self.looping = plan.loop_at(t).map(|i| (i, 0));
        self.leaving = false;
        self.armed_ms = None;
        self.held = false;
        match landing {
            Landing::Pausing => {
                let bookmark = pausing_at(plan, t);
                self.status = Status::Paused {
                    bookmark,
                    since_ns: at_ns,
                };
                self.resume_ms = None;
                out.push(Step {
                    at_ns,
                    effect: Effect::Paused { bookmark, at_ms: t },
                });
            }
            Landing::PlayOn => {
                self.status = Status::Playing;
                self.resume_ms = Some(t);
            }
        }
    }

    /// Puts up what a landing at `t` on `edge`'s side shows: the spanning state, then
    /// the state events before `t` whose last value the runner set differs.
    fn put_landing_state<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        t: u32,
        edge: Edge,
        transition: Option<K::Preset>,
        at_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let mut target = spanning(plan, t, edge);
        let mut seek = true;
        let program_end = plan.program_end();
        if target.program.is_none() && !plan.program.is_empty() && t >= program_end {
            // Past the program track, playing would have left its last clip up, at its
            // end, with that clip's last keyframe.
            let last = spanning(plan, program_end, Edge::Before);
            target.program = last.program;
            target.keyframe = last.keyframe;
            seek = false;
        }
        if edge == Edge::Before
            && pausing_at(plan, t).is_some()
            && let Some(clip) = clip_starting_at(plan, t)
        {
            // A pausing bookmark where a program clip starts waits on that clip, as
            // playing there does: what else sits at `t` happens on Continue.
            target.program = Some((clip, 0));
            target.keyframe = None;
        }
        self.put_up(plan, &target, Mode::Jump { seek }, transition, at_ns, out);
        for i in state_events_before(plan, t) {
            let Some(event) = plan.events.get(i) else {
                continue;
            };
            let Some(key) = event.action.state_key() else {
                continue;
            };
            if self.applied.states.get(&format!("{key:?}")) != Some(&i) {
                self.fire(i, &event.action, at_ns, out);
            }
        }
    }

    /// Diffs `target` against what it last put up, emitting the effects that take one
    /// to the other: playing, landing and wrapping all go through here. `transition`
    /// is for a program cut on a jump or a wrap; playing uses the clip's own.
    fn put_up<K: ShowKeys<Timeline = T>>(
        &mut self,
        plan: &Plan<K>,
        target: &Spanning<T>,
        mode: Mode,
        transition: Option<K::Preset>,
        at_ns: i64,
        out: &mut Vec<Step<K>>,
    ) {
        let mut push = |effect| out.push(Step { at_ns, effect });
        let playing = mode == Mode::Play;
        // Program and keyframe; a target with no program clip leaves them alone.
        if let Some((clip, offset_ms)) = target.program
            && let Some(c) = plan.program.get(clip)
        {
            let same = self.applied.program == Some(clip);
            // A jump or wrap back before the keyframe in force re-cuts to clear it.
            let clear = !playing && target.keyframe.is_none() && self.applied.keyframe.is_some();
            if !same || clear {
                let transition = if playing {
                    c.transition.clone()
                } else {
                    transition
                };
                push(Effect::Program {
                    clip,
                    scene: c.scene.clone(),
                    transition,
                    offset_ms,
                });
                self.applied.keyframe = None;
            } else if c.timed && matches!(mode, Mode::Wrap | Mode::Jump { seek: true }) {
                push(Effect::Seek {
                    clip,
                    scene: c.scene.clone(),
                    offset_ms,
                });
            }
            self.applied.program = Some(clip);
            if target.keyframe != self.applied.keyframe {
                if let Some(marker) = target.keyframe
                    && let Some(m) = plan.keyframes.get(marker)
                {
                    push(Effect::Keyframe {
                        marker,
                        keyframe: m.keyframe.clone(),
                        preset: m.preset.clone(),
                    });
                }
                self.applied.keyframe = target.keyframe;
            }
        }
        // Overlays: down first, then up.
        let fade = |ms: u32| if playing { ms } else { ms.min(SHORT_FADE_MS) };
        let old = std::mem::take(&mut self.applied.overlays);
        for &clip in &old {
            if target.overlays.contains_key(&clip) {
                continue;
            }
            if let Some(c) = plan.overlays.get(clip) {
                push(Effect::OverlayOff {
                    clip,
                    track: c.track.clone(),
                    scene: c.scene.clone(),
                    fade_ms: fade(c.fade_out_ms),
                });
            }
        }
        for (&clip, &offset_ms) in &target.overlays {
            let Some(c) = plan.overlays.get(clip) else {
                continue;
            };
            let up = old.contains(&clip);
            let fade_ms = if !up {
                fade(c.fade_in_ms)
            } else if c.timed && matches!(mode, Mode::Jump { .. }) {
                0
            } else {
                continue;
            };
            push(Effect::OverlayOn {
                clip,
                track: c.track.clone(),
                scene: c.scene.clone(),
                fade_ms,
                offset_ms,
            });
        }
        self.applied.overlays = target.overlays.keys().copied().collect();
        // Audio: down first, then up. A clip that ends while playing is already down by
        // its own fade out; one a jump, a wrap or a stop cuts off fades over the short
        // fade (ADR-0210 decision 6). One that stays up through a jump or a wrap is sought
        // to its place again.
        let old = std::mem::take(&mut self.applied.audio);
        for &clip in &old {
            if target.audio.contains_key(&clip) {
                continue;
            }
            if let Some(c) = plan.audio.get(clip) {
                push(Effect::AudioOff {
                    clip,
                    track: c.track.clone(),
                    source: c.source.clone(),
                    fade_ms: if playing { 0 } else { SHORT_FADE_MS },
                });
            }
        }
        for (&clip, &offset_ms) in &target.audio {
            let Some(c) = plan.audio.get(clip) else {
                continue;
            };
            if old.contains(&clip) && playing {
                continue;
            }
            push(Effect::AudioOn {
                clip,
                track: c.track.clone(),
                source: c.source.clone(),
                offset_ms,
            });
        }
        self.applied.audio = target.audio.keys().copied().collect();
        // Sequences: stops first, then plays.
        let old = std::mem::take(&mut self.applied.sequences);
        for (timeline, &clip) in &old {
            if target
                .sequences
                .get(timeline)
                .is_none_or(|&(c, _)| c != clip)
            {
                push(Effect::SequenceStop {
                    clip,
                    timeline: timeline.clone(),
                });
            }
        }
        for (timeline, &(clip, offset_ms)) in &target.sequences {
            let stays = old.get(timeline) == Some(&clip);
            if !stays || matches!(mode, Mode::Jump { .. }) {
                push(Effect::SequencePlay {
                    clip,
                    timeline: timeline.clone(),
                    offset_ms,
                });
            }
        }
        self.applied.sequences = target
            .sequences
            .iter()
            .map(|(timeline, &(clip, _))| (timeline.clone(), clip))
            .collect();
    }
}

/// The fade for an overlay taken down or put up by a jump, a wrap or a stop, at most.
const SHORT_FADE_MS: u32 = 300;

const NS_PER_MS: i64 = 1_000_000;

/// `ms` milliseconds in nanoseconds.
fn ms_ns(ms: u32) -> i64 {
    i64::from(ms).saturating_mul(NS_PER_MS)
}

/// How [`Runner::put_up`] reached its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Playing into it: clips' own transitions and fades, nothing re-seeks.
    Play,
    /// A landing: the landing transition, short fades, timed things re-seek (the program
    /// clip only when `seek`).
    Jump {
        /// Whether a timed program clip that stays seeks.
        seek: bool,
    },
    /// A loop wrap: the wrap transition, short fades; only the program clip re-seeks.
    Wrap,
}

/// The first pausing bookmark at `at_ms`, by index.
fn pausing_at<K: ShowKeys>(plan: &Plan<K>, at_ms: u32) -> Option<usize> {
    plan.bookmarks
        .iter()
        .position(|b| b.pausing && b.at_ms == at_ms)
}

/// The transition for a landing at `at_ms`: a bookmark's arrive there, else the show's
/// default.
fn arrive_at<K: ShowKeys>(plan: &Plan<K>, at_ms: u32) -> Option<K::Preset> {
    plan.bookmarks
        .iter()
        .find(|b| b.at_ms == at_ms && b.arrive.is_some())
        .and_then(|b| b.arrive.clone())
        .or_else(|| plan.default_transition.clone())
}

/// Where and how a jump to bookmark `b` lands, and its transition.
/// Where and how a landing arrives ([`Runner::land`]).
struct Arrival<P> {
    /// Where, at most the show's length.
    at_ms: u32,
    /// Whether it pauses there or plays on.
    landing: Landing,
    /// The transition a program cut it makes uses.
    transition: Option<P>,
    /// The play head it jumped from, for [`Effect::Jumped`]; `None` for a start.
    jumped_from: Option<u32>,
}

/// The program clip that starts at `t`, if one does.
fn clip_starting_at<K: ShowKeys>(plan: &Plan<K>, t: u32) -> Option<usize> {
    (0..plan.program.len()).find(|&i| plan.program_start(i) == t)
}

fn bookmark_landing<K: ShowKeys>(
    plan: &Plan<K>,
    b: &Bookmark<K>,
) -> (u32, Landing, Option<K::Preset>) {
    let landing = if b.pausing {
        Landing::Pausing
    } else {
        Landing::PlayOn
    };
    let transition = b.arrive.clone().or_else(|| plan.default_transition.clone());
    (b.at_ms, landing, transition)
}

/// The first point after `after` (or at or after 0 for `None`): a time at which
/// something starts, ends or fires, or the show's end.
fn next_point<K: ShowKeys>(plan: &Plan<K>, after: Option<u32>) -> Option<u32> {
    let mut best: Option<u32> = None;
    let mut consider = |t: u32| {
        if after.is_none_or(|a| t > a) && best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    };
    let mut start = 0u32;
    consider(start);
    for clip in &plan.program {
        start = start.saturating_add(clip.length_ms);
        consider(start);
    }
    for c in &plan.overlays {
        consider(c.start_ms);
        consider(c.start_ms.saturating_add(c.length_ms));
    }
    for c in &plan.sequences {
        consider(c.start_ms);
        consider(c.start_ms.saturating_add(c.length_ms));
    }
    for c in &plan.audio {
        consider(c.start_ms);
        consider(c.start_ms.saturating_add(c.length_ms));
    }
    plan.keyframes.iter().for_each(|m| consider(m.at_ms));
    plan.events.iter().for_each(|e| consider(e.at_ms));
    plan.bookmarks.iter().for_each(|b| consider(b.at_ms));
    for l in &plan.loops {
        consider(l.from_ms);
        consider(l.to_ms);
    }
    consider(plan.length_ms());
    best
}

impl<T: Ord + Clone> Default for Runner<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod unit {
    use super::*;
    use crate::show::{
        KeyframeMark, LoopRegion, OverlayClip, ProgramClip, SequenceClip, ShowEvent,
    };

    #[derive(Clone, Debug, PartialEq)]
    enum Act {
        Set(&'static str, u8),
        Ping,
    }

    impl CueAction for Act {
        type Key = &'static str;
        fn state_key(&self) -> Option<&'static str> {
            match self {
                Act::Set(k, _) => Some(k),
                Act::Ping => None,
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct K;
    impl ShowKeys for K {
        type Scene = &'static str;
        type Timeline = u8;
        type Bookmark = &'static str;
        type Loop = &'static str;
        type Track = u8;
        type Source = &'static str;
        type Preset = &'static str;
        type Action = Act;
    }

    const MS: i64 = 1_000_000;

    fn clip(scene: &'static str, length_ms: u32) -> ProgramClip<K> {
        ProgramClip {
            scene,
            length_ms,
            transition: Some("cut"),
            timed: false,
        }
    }

    fn plan() -> Plan<K> {
        Plan {
            program: vec![clip("a", 1000), clip("b", 1000), clip("c", 1000)],
            overlays: vec![OverlayClip {
                track: 2,
                scene: "logo",
                start_ms: 200,
                length_ms: 600,
                fade_in_ms: 500,
                fade_out_ms: 500,
                timed: false,
            }],
            audio: vec![],
            keyframes: vec![KeyframeMark {
                at_ms: 1500,
                keyframe: Some("b2"),
                preset: None,
            }],
            sequences: vec![SequenceClip {
                timeline: 1,
                start_ms: 1200,
                length_ms: 1000,
            }],
            events: vec![
                ShowEvent {
                    at_ms: 100,
                    action: Act::Set("x", 1),
                },
                ShowEvent {
                    at_ms: 1000,
                    action: Act::Ping,
                },
                ShowEvent {
                    at_ms: 1600,
                    action: Act::Set("x", 2),
                },
            ],
            bookmarks: vec![Bookmark {
                id: "one",
                at_ms: 1000,
                pausing: true,
                arrive: Some("fade"),
                limit_ms: None,
            }],
            loops: vec![],
            end: EndOfShow::Stop,
            default_transition: Some("dflt"),
        }
    }

    fn effects(steps: Vec<Step<K>>) -> Vec<Effect<K>> {
        steps.into_iter().map(|s| s.effect).collect()
    }

    #[test]
    fn plays_pauses_resumes_and_ends() {
        let p = plan();
        assert_eq!(p.check(), Ok(()));
        assert_eq!(p.length_ms(), 3000);
        let mut r = Runner::new();
        let s = effects(r.start(&p, 0, Landing::PlayOn, 0));
        assert!(matches!(
            s[..],
            [Effect::Program {
                clip: 0,
                offset_ms: 0,
                ..
            }]
        ));
        assert!(effects(r.tick(&p, 0)).is_empty());
        let s = r.tick(&p, 1500 * MS);
        // The clip starting at the bookmark goes up as the show pauses there.
        assert_eq!(s.len(), 5, "{s:?}");
        assert_eq!(s[0].at_ns, 100 * MS);
        assert!(matches!(s[3].effect, Effect::Program { clip: 1, .. }));
        assert!(matches!(
            s[4].effect,
            Effect::Paused {
                bookmark: Some(0),
                at_ms: 1000
            }
        ));
        assert_eq!(s[4].at_ns, 1000 * MS);
        assert_eq!(r.position_ms(5000 * MS), 1000);
        assert_eq!(r.next(&p, 5000 * MS), Next::Resume);
        let s = effects(r.continue_(&p, 5000 * MS));
        assert_eq!(s, vec![Effect::Resumed { at_ms: 1000 }]);
        let s = effects(r.tick(&p, 5000 * MS));
        assert!(matches!(s[..], [Effect::Action { event: 1, .. }]), "{s:?}");
        assert_eq!(r.next_due(&p), Some(5200 * MS));
        let s = effects(r.tick(&p, 7000 * MS));
        assert!(matches!(s.last(), Some(Effect::Ended)), "{s:?}");
        assert_eq!(r.status(), Status::Ended);
    }

    /// The show's own view after playing to `t` equals the view after jumping there.
    #[test]
    fn jump_equals_play() {
        let p = plan();
        for t in [0, 100, 150, 1000, 1200, 1500, 1700, 2500, 3000] {
            let mut played = Runner::new();
            played.start(&p, 0, Landing::PlayOn, 0);
            played.tick(&p, i64::from(t.min(999)) * MS);
            if t >= 1000 {
                played.tick(&p, 1000 * MS);
                played.continue_(&p, 1000 * MS);
            }
            played.tick(&p, i64::from(t) * MS);
            let mut jumped = Runner::new();
            jumped.start(&p, 0, Landing::PlayOn, 0);
            jumped.tick(&p, 50 * MS);
            jumped.jump(&p, t, Landing::PlayOn, 50 * MS);
            jumped.tick(&p, 50 * MS);
            assert_eq!(played.applied(), jumped.applied(), "at {t}");
        }
    }

    #[test]
    fn back_skip_and_go_to() {
        let p = plan();
        let mut r = Runner::new();
        r.start(&p, 0, Landing::PlayOn, 0);
        let s = effects(r.skip(&p, 0));
        assert_eq!(
            s[0],
            Effect::Jumped {
                from_ms: 0,
                to_ms: 1000
            }
        );
        assert!(matches!(
            s.last(),
            Some(Effect::Paused {
                bookmark: Some(0),
                at_ms: 1000
            })
        ));
        let s = effects(r.back(&p, 10 * MS));
        assert!(
            matches!(
                s.last(),
                Some(Effect::Paused {
                    bookmark: None,
                    at_ms: 0
                })
            ),
            "{s:?}"
        );
        let s = effects(r.go_to(&p, 0, 20 * MS));
        // x was set by the skip's landing already, so it isn't set again.
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(r.applied().states.get("\"x\""), Some(&0));
        let s = effects(r.stop(&p, 30 * MS));
        assert!(s.is_empty(), "{s:?}");
        assert_eq!(r.status(), Status::Stopped);
    }

    #[test]
    fn loops_wrap_and_leave() {
        let mut p = plan();
        p.bookmarks.clear();
        p.loops.push(LoopRegion {
            id: "l",
            from_ms: 100,
            to_ms: 400,
            leave: Leave::FinishPass,
            wrap: None,
        });
        let mut r = Runner::new();
        r.start(&p, 0, Landing::PlayOn, 0);
        let s = effects(r.tick(&p, 450 * MS));
        assert!(s.contains(&Effect::Looped { region: 0, pass: 1 }), "{s:?}");
        assert_eq!(r.position_ms(450 * MS), 150);
        assert_eq!(r.looping(), Some((0, 1)));
        assert_eq!(r.next(&p, 450 * MS), Next::LeaveLoop(0));
        r.continue_(&p, 450 * MS);
        let s = effects(r.tick(&p, 800 * MS));
        assert!(
            !s.iter().any(|e| matches!(e, Effect::Looped { .. })),
            "{s:?}"
        );
        assert_eq!(r.position_ms(800 * MS), 500);
        assert_eq!(r.looping(), None);
    }

    #[test]
    fn end_loop_does_not_spin() {
        let p = Plan::<K> {
            program: vec![],
            overlays: vec![],
            audio: vec![],
            keyframes: vec![],
            sequences: vec![],
            events: vec![],
            bookmarks: vec![],
            loops: vec![],
            end: EndOfShow::Loop,
            default_transition: None,
        };
        let mut r = Runner::new();
        r.start(&p, 0, Landing::PlayOn, 0);
        let s = effects(r.tick(&p, 0));
        assert_eq!(s, vec![Effect::Ended]);
        assert_eq!(r.next_due(&p), None);
    }
}
