//! Show runner tests: scenarios on a hand-driven clock, and property tests that hold
//! "jump equals play" over random shows.

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::*;
use crate::CueAction;

const MS: i64 = 1_000_000;

// ---------------------------------------------------------------------------------------
// The test keys
// ---------------------------------------------------------------------------------------

/// An event's action: `Set` is state (keyed), `Shot` a one-shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Set(u8, i32),
    Shot(u32),
}

impl CueAction for Act {
    type Key = u8;
    fn state_key(&self) -> Option<u8> {
        match *self {
            Act::Set(k, _) => Some(k),
            Act::Shot(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct K;

impl ShowKeys for K {
    type Scene = &'static str;
    type Timeline = u8;
    type Bookmark = u32;
    type Loop = u8;
    type Track = u8;
    type Preset = &'static str;
    type Action = Act;
}

// ---------------------------------------------------------------------------------------
// Plan builders
// ---------------------------------------------------------------------------------------

fn plan(program: Vec<ProgramClip<K>>) -> Plan<K> {
    Plan {
        program,
        overlays: vec![],
        keyframes: vec![],
        sequences: vec![],
        events: vec![],
        bookmarks: vec![],
        loops: vec![],
        end: EndOfShow::Stop,
        default_transition: None,
    }
}

fn clip(scene: &'static str, length_ms: u32) -> ProgramClip<K> {
    ProgramClip {
        scene,
        length_ms,
        transition: None,
        timed: false,
    }
}

fn clip_tr(scene: &'static str, length_ms: u32, tr: &'static str) -> ProgramClip<K> {
    ProgramClip {
        scene,
        length_ms,
        transition: Some(tr),
        timed: false,
    }
}

fn bm(id: u32, at_ms: u32, pausing: bool) -> Bookmark<K> {
    Bookmark {
        id,
        at_ms,
        pausing,
        arrive: None,
        limit_ms: None,
    }
}

fn ev(at_ms: u32, action: Act) -> ShowEvent<K> {
    ShowEvent { at_ms, action }
}

fn overlay(track: u8, scene: &'static str, start_ms: u32, length_ms: u32) -> OverlayClip<K> {
    OverlayClip {
        track,
        scene,
        start_ms,
        length_ms,
        fade_in_ms: 0,
        fade_out_ms: 0,
        timed: false,
    }
}

fn seq(timeline: u8, start_ms: u32, length_ms: u32) -> SequenceClip<K> {
    SequenceClip {
        timeline,
        start_ms,
        length_ms,
    }
}

fn lp(id: u8, from_ms: u32, to_ms: u32, leave: Leave) -> LoopRegion<K> {
    LoopRegion {
        id,
        from_ms,
        to_ms,
        leave,
        wrap: None,
    }
}

// ---------------------------------------------------------------------------------------
// The simulated world
// ---------------------------------------------------------------------------------------

/// What the engine would show after running the effects. Media positions are kept as
/// origins in show time (the point an effect ran at, minus its offset). It tracks the
/// show point from the effects that move it (`Jumped`, `Looped`, `Paused`, `Resumed`) and
/// from the anchors the rig sets on `start`, off script and back to script.
#[derive(Clone, Debug, Default)]
struct World {
    /// The program clip and its media origin; `None` when an effect put its media at
    /// its end.
    program: Option<(usize, Option<i64>)>,
    keyframe: Option<usize>,
    overlays: BTreeMap<usize, i64>,
    sequences: BTreeMap<u8, (usize, i64)>,
    states: BTreeMap<u8, i32>,
    shots: Vec<u32>,
    anchor_ms: i64,
    anchor_ns: i64,
    playing: bool,
}

/// What the show owns, as of show point `t`: comparable between two worlds.
#[derive(Debug, PartialEq)]
struct Owned {
    /// The program clip and, when it is timed, how far into it (clamped to its length).
    program: Option<(usize, Option<i64>)>,
    keyframe: Option<usize>,
    /// Overlay clips and how far into each.
    overlays: BTreeMap<usize, i64>,
    /// Sequences, their clip and how far into it.
    sequences: BTreeMap<u8, (usize, i64)>,
}

impl World {
    fn anchor(&mut self, at_ms: i64, now_ns: i64, playing: bool) {
        self.anchor_ms = at_ms;
        self.anchor_ns = now_ns;
        self.playing = playing;
    }

    fn point(&self, at_ns: i64) -> i64 {
        if self.playing {
            self.anchor_ms + (at_ns - self.anchor_ns).div_euclid(MS)
        } else {
            self.anchor_ms
        }
    }

    fn apply(&mut self, plan: &Plan<K>, steps: &[Step<K>]) {
        for w in steps.windows(2) {
            assert!(
                w[0].at_ns <= w[1].at_ns,
                "steps out of time order: {steps:#?}"
            );
        }
        let mut i = 0;
        while i < steps.len() {
            let at = steps[i].at_ns;
            let mut j = i;
            while j < steps.len() && steps[j].at_ns == at {
                j += 1;
            }
            let group = &steps[i..j];
            // Everything at one instant shares one point; take it from the group's last
            // effect that moves the play head, so the effects' order within it doesn't
            // matter.
            for s in group {
                match &s.effect {
                    Effect::Jumped { to_ms, .. } => self.anchor(i64::from(*to_ms), at, true),
                    Effect::Looped { region, .. } => {
                        self.anchor(i64::from(plan.loops[*region].from_ms), at, true)
                    }
                    Effect::Resumed { at_ms } => self.anchor(i64::from(*at_ms), at, true),
                    Effect::Paused { at_ms, .. } => self.anchor(i64::from(*at_ms), at, false),
                    _ => {}
                }
            }
            let p = self.point(at);
            for s in group {
                self.effect(plan, p, &s.effect);
            }
            if group.iter().any(|s| s.effect == Effect::Ended) {
                self.anchor(p, at, false);
            }
            i = j;
        }
    }

    fn effect(&mut self, plan: &Plan<K>, p: i64, e: &Effect<K>) {
        let origin = |clip: usize, offset_ms: u32| {
            (offset_ms < plan.program[clip].length_ms).then(|| p - i64::from(offset_ms))
        };
        match e {
            Effect::Program {
                clip, offset_ms, ..
            } => {
                self.program = Some((*clip, origin(*clip, *offset_ms)));
                self.keyframe = None;
            }
            Effect::Seek {
                clip, offset_ms, ..
            } => {
                assert_eq!(
                    self.program.map(|p| p.0),
                    Some(*clip),
                    "Seek of a clip not on program"
                );
                self.program = Some((*clip, origin(*clip, *offset_ms)));
            }
            Effect::Keyframe { marker, .. } => self.keyframe = Some(*marker),
            Effect::OverlayOff { clip, .. } => {
                assert!(
                    self.overlays.remove(clip).is_some(),
                    "OverlayOff of clip {clip}, not up"
                );
            }
            Effect::OverlayOn {
                clip, offset_ms, ..
            } => {
                self.overlays.insert(*clip, p - i64::from(*offset_ms));
            }
            Effect::SequenceStop { timeline, .. } => {
                self.sequences.remove(timeline);
            }
            Effect::SequencePlay {
                clip,
                timeline,
                offset_ms,
            } => {
                self.sequences
                    .insert(*timeline, (*clip, p - i64::from(*offset_ms)));
            }
            Effect::Action { action, .. } => match *action {
                Act::Set(k, v) => {
                    self.states.insert(k, v);
                }
                Act::Shot(n) => self.shots.push(n),
            },
            Effect::Paused { .. }
            | Effect::Resumed { .. }
            | Effect::Jumped { .. }
            | Effect::Looped { .. }
            | Effect::Ended => {}
        }
    }

    fn owned(&self, plan: &Plan<K>, t: u32) -> Owned {
        let t = i64::from(t);
        Owned {
            program: self.program.map(|(c, o)| {
                let len = i64::from(plan.program[c].length_ms);
                (
                    c,
                    plan.program[c]
                        .timed
                        .then(|| o.map_or(len, |o| (t - o).min(len))),
                )
            }),
            keyframe: self.keyframe,
            overlays: self
                .overlays
                .iter()
                .map(|(c, o)| (*c, if plan.overlays[*c].timed { t - o } else { 0 }))
                .collect(),
            sequences: self
                .sequences
                .iter()
                .map(|(tl, (c, o))| (*tl, (*c, t - o)))
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// The rig: a runner, its plan, a world and a hand-driven clock
// ---------------------------------------------------------------------------------------

struct Rig {
    plan: Plan<K>,
    r: Runner<u8>,
    w: World,
    now: i64,
}

impl Rig {
    fn new(plan: Plan<K>) -> Self {
        assert_eq!(plan.check(), Ok(()), "{plan:#?}");
        Rig {
            plan,
            r: Runner::new(),
            w: World::default(),
            now: 0,
        }
    }

    fn run(&mut self, steps: Vec<Step<K>>) -> Vec<Step<K>> {
        for s in &steps {
            assert!(
                s.at_ns <= self.now,
                "step due after now ({}): {s:?}",
                self.now
            );
        }
        self.w.apply(&self.plan, &steps);
        steps
    }

    fn effects(steps: Vec<Step<K>>) -> Vec<Effect<K>> {
        steps.into_iter().map(|s| s.effect).collect()
    }

    fn start(&mut self, at_ms: u32, landing: Landing) -> Vec<Effect<K>> {
        self.w
            .anchor(i64::from(at_ms), self.now, landing == Landing::PlayOn);
        let s = self.r.start(&self.plan, at_ms, landing, self.now);
        let mut e = Self::effects(self.run(s));
        e.extend(self.tick_ns(self.now));
        e
    }

    fn tick_steps(&mut self, now_ns: i64) -> Vec<Step<K>> {
        assert!(now_ns >= self.now, "the clock runs backwards");
        self.now = now_ns;
        let s = self.r.tick(&self.plan, now_ns);
        self.run(s)
    }

    fn tick_ns(&mut self, now_ns: i64) -> Vec<Effect<K>> {
        Self::effects(self.tick_steps(now_ns))
    }

    /// Ticks at `ms` milliseconds on the clock.
    fn at(&mut self, ms: i64) -> Vec<Effect<K>> {
        self.tick_ns(ms * MS)
    }

    /// Calls a command, then ticks at the same instant (what lands at a point happens
    /// on the next tick).
    fn cmd(
        &mut self,
        f: impl FnOnce(&mut Runner<u8>, &Plan<K>, i64) -> Vec<Step<K>>,
    ) -> Vec<Effect<K>> {
        let s = f(&mut self.r, &self.plan, self.now);
        let mut e = Self::effects(self.run(s));
        e.extend(self.tick_ns(self.now));
        e
    }

    fn cont(&mut self) -> Vec<Effect<K>> {
        self.cmd(|r, p, n| r.continue_(p, n))
    }

    fn back(&mut self) -> Vec<Effect<K>> {
        self.cmd(|r, p, n| r.back(p, n))
    }

    fn skip(&mut self) -> Vec<Effect<K>> {
        self.cmd(|r, p, n| r.skip(p, n))
    }

    fn go_to(&mut self, b: usize) -> Vec<Effect<K>> {
        self.cmd(|r, p, n| r.go_to(p, b, n))
    }

    fn jump(&mut self, t: u32, landing: Landing) -> Vec<Effect<K>> {
        self.cmd(|r, p, n| r.jump(p, t, landing, n))
    }

    fn stop(&mut self) -> Vec<Effect<K>> {
        let s = self.r.stop(&self.plan, self.now);
        Self::effects(self.run(s))
    }

    fn off_script(&mut self) {
        let p = self.pos();
        self.r.off_script(self.now);
        self.w.anchor(i64::from(p), self.now, false);
    }

    fn back_to_script(&mut self) -> Vec<Effect<K>> {
        let p = self.pos();
        self.w.anchor(i64::from(p), self.now, true);
        self.cmd(|r, pl, n| r.back_to_script(pl, n))
    }

    fn pos(&self) -> u32 {
        self.r.position_ms(self.now)
    }

    fn next(&self) -> Next {
        self.r.next(&self.plan, self.now)
    }

    fn program_clip(&self) -> Option<usize> {
        self.w.program.map(|p| p.0)
    }
}

fn has(e: &[Effect<K>], f: impl Fn(&Effect<K>) -> bool) -> bool {
    e.iter().any(f)
}

fn actions(e: &[Effect<K>]) -> Vec<usize> {
    e.iter()
        .filter_map(|e| match e {
            Effect::Action { event, .. } => Some(*event),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Plan, resolve
// ---------------------------------------------------------------------------------------

#[test]
fn plan_starts_length_and_loop_at() {
    let mut p = plan(vec![clip("A", 10), clip("B", 20), clip("C", 5)]);
    assert_eq!(p.program_start(0), 0);
    assert_eq!(p.program_start(1), 10);
    assert_eq!(p.program_start(2), 30);
    assert_eq!(p.length_ms(), 35);
    p.events.push(ev(50, Act::Shot(1)));
    assert_eq!(p.length_ms(), 50);
    p.overlays.push(overlay(0, "O", 40, 30));
    assert_eq!(p.length_ms(), 70);
    p.loops.push(lp(0, 10, 30, Leave::AtOnce));
    assert_eq!(p.loop_at(9), None);
    assert_eq!(p.loop_at(10), Some(0));
    assert_eq!(p.loop_at(29), Some(0));
    assert_eq!(p.loop_at(30), None);
    assert_eq!(plan(vec![]).length_ms(), 0);
}

#[test]
fn plan_check_errors() {
    let ok = plan(vec![clip("A", 10), clip("B", 10)]);
    assert_eq!(ok.check(), Ok(()));

    let p = plan(vec![clip("A", 10), clip("B", 0)]);
    assert_eq!(p.check(), Err(InvalidPlan::EmptyClip(1)));

    let mut p = ok.clone();
    p.overlays = vec![overlay(0, "O", 0, 5), overlay(1, "P", 2, 0)];
    assert_eq!(p.check(), Err(InvalidPlan::EmptyClip(1)));

    let mut p = ok.clone();
    p.sequences = vec![seq(0, 0, 0)];
    assert_eq!(p.check(), Err(InvalidPlan::EmptyClip(0)));

    let mut p = ok.clone();
    p.events = vec![
        ev(5, Act::Shot(1)),
        ev(5, Act::Shot(2)),
        ev(3, Act::Shot(3)),
    ];
    assert_eq!(p.check(), Err(InvalidPlan::Unsorted(2)));

    let mut p = ok.clone();
    p.bookmarks = vec![bm(0, 8, true), bm(1, 4, true)];
    assert_eq!(p.check(), Err(InvalidPlan::Unsorted(1)));

    let mut p = ok.clone();
    p.overlays = vec![
        overlay(0, "O", 0, 5),
        overlay(1, "P", 2, 5),
        overlay(0, "Q", 4, 5),
    ];
    assert_eq!(p.check(), Err(InvalidPlan::OverlayOverlap(2)));
    // End to end on one track, and overlapping on two tracks, are fine.
    p.overlays = vec![
        overlay(0, "O", 0, 5),
        overlay(1, "P", 2, 5),
        overlay(0, "Q", 5, 5),
    ];
    assert_eq!(p.check(), Ok(()));

    let mut p = ok.clone();
    p.loops = vec![lp(0, 5, 5, Leave::AtOnce)];
    assert_eq!(p.check(), Err(InvalidPlan::Loop(0)));
    p.loops = vec![lp(0, 2, 8, Leave::AtOnce), lp(1, 7, 12, Leave::AtOnce)];
    assert_eq!(p.check(), Err(InvalidPlan::Loop(1)));
    p.loops = vec![lp(0, 2, 8, Leave::AtOnce), lp(1, 8, 12, Leave::AtOnce)];
    assert_eq!(p.check(), Ok(()));

    let mut p = ok.clone();
    p.bookmarks = vec![bm(3, 5, true)];
    p.end = EndOfShow::GoTo(4);
    assert_eq!(p.check(), Err(InvalidPlan::EndBookmark));
    p.end = EndOfShow::GoTo(3);
    assert_eq!(p.check(), Ok(()));
}

#[test]
fn spanning_edges() {
    let mut p = plan(vec![clip("A", 10), clip("B", 10)]);
    p.keyframes = vec![KeyframeMark {
        at_ms: 15,
        keyframe: Some("k"),
        preset: None,
    }];
    p.overlays = vec![overlay(0, "O", 5, 5)];
    p.sequences = vec![seq(1, 0, 10), seq(1, 5, 10)];
    let at = spanning(&p, 10, Edge::At);
    assert_eq!(at.program, Some((1, 0)));
    assert!(at.overlays.is_empty());
    assert_eq!(at.sequences.get(&1), Some(&(1, 5)));
    let before = spanning(&p, 10, Edge::Before);
    assert_eq!(before.program, Some((0, 10)));
    assert_eq!(before.overlays.get(&0), Some(&5));
    assert_eq!(before.sequences.get(&1), Some(&(1, 5)));
    assert_eq!(spanning(&p, 0, Edge::Before), Spanning::default());
    assert_eq!(spanning(&p, 15, Edge::At).keyframe, Some(0));
    assert_eq!(spanning(&p, 15, Edge::Before).keyframe, None);
    assert_eq!(spanning(&p, 20, Edge::Before).keyframe, Some(0));

    p.events = vec![
        ev(2, Act::Set(0, 1)),
        ev(3, Act::Shot(9)),
        ev(4, Act::Set(1, 1)),
        ev(5, Act::Set(0, 2)),
    ];
    assert_eq!(state_events_before(&p, 5), vec![0, 2]);
    assert_eq!(state_events_before(&p, 6), vec![2, 3]);
    assert!(state_events_before(&p, 2).is_empty());
}

// ---------------------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------------------

#[test]
fn plays_through_cuts_with_each_clips_transition_at_exact_times() {
    let p = plan(vec![
        clip("A", 10),
        clip_tr("B", 10, "fade"),
        clip_tr("C", 5, "wipe"),
    ]);
    let mut g = Rig::new(p);
    let e = g.start(0, Landing::PlayOn);
    assert!(has(&e, |e| matches!(
        e,
        Effect::Program {
            clip: 0,
            scene: "A",
            offset_ms: 0,
            ..
        }
    )));
    assert_eq!(g.r.status(), Status::Playing);
    // A late tick still gives each step its exact due time.
    let steps = g.tick_steps(37 * MS);
    let due = |f: &dyn Fn(&Effect<K>) -> bool| steps.iter().find(|s| f(&s.effect)).map(|s| s.at_ns);
    assert_eq!(
        due(&|e| *e
            == Effect::Program {
                clip: 1,
                scene: "B",
                transition: Some("fade"),
                offset_ms: 0
            }),
        Some(10 * MS)
    );
    assert_eq!(
        due(&|e| *e
            == Effect::Program {
                clip: 2,
                scene: "C",
                transition: Some("wipe"),
                offset_ms: 0
            }),
        Some(20 * MS)
    );
    assert_eq!(due(&|e| *e == Effect::Ended), Some(25 * MS));
    assert_eq!(g.r.status(), Status::Ended);
    assert_eq!(g.w.program, Some((2, Some(20))));
}

#[test]
fn step_times_are_exact_from_an_odd_clock() {
    let mut p = plan(vec![clip("A", 10), clip("B", 10)]);
    p.events = vec![ev(13, Act::Shot(1))];
    let mut g = Rig::new(p);
    g.now = 1_234_567;
    g.start(0, Landing::PlayOn);
    let steps = g.tick_steps(1_234_567 + 14 * MS + 999);
    let at: Vec<i64> = steps.iter().map(|s| s.at_ns).collect();
    assert_eq!(at, vec![1_234_567 + 10 * MS, 1_234_567 + 13 * MS]);
    assert_eq!(g.pos(), 14);
}

fn pausing_plan(pausing: bool) -> Plan<K> {
    let mut p = plan(vec![clip("A", 10), clip("B", 10)]);
    p.bookmarks = vec![bm(0, 10, pausing)];
    p.events = vec![ev(10, Act::Set(0, 1)), ev(10, Act::Shot(7))];
    p.overlays = vec![overlay(0, "O", 10, 5)];
    p.sequences = vec![seq(1, 10, 5)];
    p
}

#[test]
fn pausing_bookmark_pauses_before_its_point_and_continue_runs_it() {
    let mut g = Rig::new(pausing_plan(true));
    g.start(0, Landing::PlayOn);
    let steps = g.tick_steps(15 * MS);
    assert!(steps.iter().any(|s| s.at_ns == 10 * MS
        && s.effect
            == Effect::Paused {
                bookmark: Some(0),
                at_ms: 10
            }));
    assert_eq!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(0),
            since_ns: 10 * MS
        }
    );
    assert_eq!(g.pos(), 10);
    assert_eq!(g.w.program, Some((0, Some(0))));
    assert!(g.w.overlays.is_empty() && g.w.sequences.is_empty());
    assert!(g.w.states.is_empty() && g.w.shots.is_empty());
    assert_eq!(g.next(), Next::Resume);
    assert_eq!(g.r.next_due(&g.plan), None);
    assert!(g.at(18).is_empty());

    g.at(20);
    let e = g.cont();
    assert!(has(&e, |e| *e == Effect::Resumed { at_ms: 10 }));
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.w.program, Some((1, Some(10))));
    assert_eq!(g.w.overlays, BTreeMap::from([(0, 10)]));
    assert_eq!(g.w.sequences, BTreeMap::from([(1, (0, 10))]));
    assert_eq!(g.w.states, BTreeMap::from([(0, 1)]));
    assert_eq!(g.w.shots, vec![7]);
    g.at(25);
    assert_eq!(g.pos(), 15);
}

#[test]
fn non_pausing_bookmark_plays_through() {
    let mut g = Rig::new(pausing_plan(false));
    g.start(0, Landing::PlayOn);
    let e = g.at(15);
    assert!(!has(&e, |e| matches!(e, Effect::Paused { .. })));
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.w.program, Some((1, Some(10))));
    assert_eq!(g.w.shots, vec![7]);
    assert_eq!(g.pos(), 15);
}

fn go_to_plan() -> Plan<K> {
    let mut p = plan(vec![clip("A", 10), clip("B", 10), clip_tr("C", 10, "wipe")]);
    p.default_transition = Some("dflt");
    p.bookmarks = vec![
        bm(0, 15, false),
        Bookmark {
            arrive: Some("arr"),
            ..bm(1, 20, true)
        },
    ];
    p.events = vec![
        ev(5, Act::Set(1, 3)),
        ev(20, Act::Set(1, 5)),
        ev(20, Act::Shot(1)),
    ];
    p
}

#[test]
fn go_to_pausing_bookmark_lands_before_it() {
    let mut g = Rig::new(go_to_plan());
    g.start(0, Landing::PlayOn);
    g.at(2);
    let e = g.go_to(1);
    assert!(has(&e, |e| *e
        == Effect::Program {
            clip: 1,
            scene: "B",
            transition: Some("arr"),
            offset_ms: 10
        }));
    assert!(has(&e, |e| *e
        == Effect::Paused {
            bookmark: Some(1),
            at_ms: 20
        }));
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    assert_eq!(g.pos(), 20);
    assert_eq!(g.w.program, Some((1, None)));
    assert_eq!(g.w.states, BTreeMap::from([(1, 3)]));
    assert!(g.w.shots.is_empty());

    g.at(4);
    let e = g.cont();
    assert!(has(&e, |e| *e
        == Effect::Program {
            clip: 2,
            scene: "C",
            transition: Some("wipe"),
            offset_ms: 0
        }));
    assert_eq!(g.w.states, BTreeMap::from([(1, 5)]));
    assert_eq!(g.w.shots, vec![1]);
}

#[test]
fn go_to_non_pausing_bookmark_plays_on() {
    let mut g = Rig::new(go_to_plan());
    g.start(0, Landing::PlayOn);
    let e = g.go_to(0);
    assert!(has(&e, |e| *e
        == Effect::Program {
            clip: 1,
            scene: "B",
            transition: Some("dflt"),
            offset_ms: 5
        }));
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.w.states, BTreeMap::from([(1, 3)]));
    // It plays on into the pausing bookmark at 20.
    let e = g.at(5);
    assert!(has(&e, |e| *e
        == Effect::Paused {
            bookmark: Some(1),
            at_ms: 20
        }));
}

#[test]
fn back_and_skip() {
    let mut p = plan(vec![clip("A", 30)]);
    p.bookmarks = vec![bm(0, 10, true), bm(1, 20, true)];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(10);
    g.cont();
    g.at(20);
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    g.cont();
    g.at(25);
    assert_eq!(g.pos(), 25);

    g.back();
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    assert_eq!(g.pos(), 20);
    g.back();
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(0),
            ..
        }
    ));
    assert_eq!(g.pos(), 10);
    g.back();
    assert!(matches!(
        g.r.status(),
        Status::Paused { bookmark: None, .. }
    ));
    assert_eq!(g.pos(), 0);

    g.skip();
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(0),
            ..
        }
    ));
    assert_eq!(g.pos(), 10);
    g.skip();
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    assert_eq!(g.pos(), 20);
    let e = g.skip();
    assert!(e.is_empty(), "{e:?}");
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    assert_eq!(g.pos(), 20);
}

#[test]
fn early_continue_jumps_past_the_next_pause() {
    let mut p = plan(vec![clip("A", 10), clip("B", 20)]);
    p.bookmarks = vec![bm(0, 20, true)];
    p.events = vec![ev(20, Act::Shot(3))];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(5);
    assert_eq!(g.next(), Next::Jump(20));
    let e = g.cont();
    assert!(has(&e, |e| *e
        == Effect::Jumped {
            from_ms: 5,
            to_ms: 20
        }));
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 20);
    assert_eq!(g.w.program, Some((1, Some(10))));
    assert_eq!(g.w.shots, vec![3]);
    let e = g.at(12);
    assert!(!has(&e, |e| matches!(e, Effect::Paused { .. })));
    assert_eq!(g.pos(), 27);
}

#[test]
fn armed_spot() {
    let mut p = plan(vec![clip("A", 10), clip("B", 20)]);
    p.bookmarks = vec![bm(0, 20, true)];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(2);
    g.r.arm(Some(25));
    assert_eq!(g.r.armed_ms(), Some(25));
    assert_eq!(g.next(), Next::Jump(25));
    g.r.arm(None);
    assert_eq!(g.next(), Next::Jump(20));
    g.r.arm(Some(25));
    g.cont();
    assert_eq!(g.pos(), 25);
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.r.armed_ms(), None);
    assert_eq!(g.w.program, Some((1, Some(10))));
}

/// P0 0..10, P1 10..30 (the loop), P2 30..40; overlay 0 spans the whole loop, overlay 1
/// and the sequence sit inside it; pausing bookmarks at the loop's start and end.
fn loop_plan(leave: Leave) -> Plan<K> {
    let mut p = plan(vec![clip("P0", 10), clip("P1", 20), clip("P2", 10)]);
    p.overlays = vec![overlay(0, "O0", 5, 35), overlay(1, "O1", 15, 5)];
    p.sequences = vec![seq(1, 12, 5)];
    p.bookmarks = vec![bm(0, 10, true), bm(1, 30, true)];
    p.events = vec![ev(20, Act::Shot(9))];
    p.loops = vec![lp(0, 10, 30, leave)];
    p
}

/// Plays `loop_plan` into its second pass, at clock 55 ms (show time 15).
fn into_loop(leave: Leave) -> Rig {
    let mut g = Rig::new(loop_plan(leave));
    g.start(0, Landing::PlayOn);
    g.at(10);
    assert!(
        matches!(
            g.r.status(),
            Status::Paused {
                bookmark: Some(0),
                ..
            }
        ),
        "pauses on entry"
    );
    assert_eq!(g.w.program, Some((0, Some(0))));
    g.cont();
    assert_eq!(g.w.program, Some((1, Some(10))));
    g.at(29);
    let e = g.at(30);
    assert!(
        has(&e, |e| *e == Effect::Looped { region: 0, pass: 1 }),
        "{e:?}"
    );
    assert!(
        !has(&e, |e| matches!(e, Effect::Paused { .. })),
        "a loop's end never pauses a pass"
    );
    assert!(!has(&e, |e| matches!(
        e,
        Effect::Program { .. } | Effect::Seek { .. }
    )));
    assert!(!has(&e, |e| matches!(
        e,
        Effect::OverlayOff { clip: 0, .. }
    )));
    assert_eq!(g.r.looping(), Some((0, 1)));
    assert_eq!(
        g.r.status(),
        Status::Playing,
        "the loop's start doesn't pause on a wrap"
    );
    assert_eq!(g.pos(), 10);
    assert_eq!(g.w.program, Some((1, Some(10))));
    assert_eq!(g.w.overlays, BTreeMap::from([(0, 5)]));
    g.at(32);
    assert_eq!(
        g.w.sequences,
        BTreeMap::from([(1, (0, 12))]),
        "the sequence restarts"
    );
    g.at(35);
    assert_eq!(
        g.w.overlays,
        BTreeMap::from([(0, 5), (1, 15)]),
        "overlay 1 restarts"
    );
    g.at(40);
    assert_eq!(g.w.shots, vec![9, 9]);
    g.at(50);
    assert_eq!(g.r.looping(), Some((0, 2)));
    assert_eq!(g.r.status(), Status::Playing);
    g.at(55);
    assert_eq!(g.pos(), 15);
    assert_eq!(g.next(), Next::LeaveLoop(0));
    g
}

#[test]
fn loop_wraps_and_continue_at_once_lands_at_its_end() {
    // With a pausing bookmark at the loop's end, leaving at once lands pausing there,
    // as finishing the pass would.
    let mut g = into_loop(Leave::AtOnce);
    let e = g.cont();
    assert!(
        has(&e, |e| *e
            == Effect::Jumped {
                from_ms: 15,
                to_ms: 30
            }),
        "{e:?}"
    );
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(1),
            ..
        }
    ));
    assert_eq!(g.r.looping(), None);
    assert_eq!(g.w.program, Some((1, Some(10))));
    g.cont();
    assert_eq!(g.w.program, Some((2, Some(30))));

    // Without one, it plays on from the end.
    let mut p = loop_plan(Leave::AtOnce);
    p.bookmarks.truncate(1);
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(10);
    g.cont();
    g.at(35);
    assert_eq!(g.r.looping(), Some((0, 1)));
    assert_eq!(g.pos(), 15);
    g.cont();
    assert_eq!(g.pos(), 30);
    assert_eq!(g.r.looping(), None);
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.w.program, Some((2, Some(30))));
    assert_eq!(g.w.overlays, BTreeMap::from([(0, 5)]));
    let e = g.at(36);
    assert!(!has(&e, |e| matches!(e, Effect::Paused { .. })));
    assert_eq!(g.pos(), 31);
}

#[test]
fn loop_finish_pass_plays_the_pass_then_goes_on() {
    let mut g = into_loop(Leave::FinishPass);
    let e = g.cont();
    assert!(!has(&e, |e| matches!(e, Effect::Jumped { .. })));
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 15);
    let e = g.at(69);
    assert!(!has(&e, |e| matches!(e, Effect::Looped { .. })));
    assert_eq!(g.w.program, Some((1, Some(10))));
    let e = g.at(70);
    assert!(!has(&e, |e| matches!(e, Effect::Looped { .. })));
    // Leaving, the play head reaches the loop's end outside a pass: its bookmark pauses.
    assert!(
        matches!(
            g.r.status(),
            Status::Paused {
                bookmark: Some(1),
                ..
            }
        ),
        "{e:?}"
    );
    g.cont();
    assert_eq!(g.r.looping(), None);
    assert_eq!(g.w.program, Some((2, Some(30))));
    g.at(75);
    assert_eq!(g.pos(), 35);
}

#[test]
fn end_of_show_loop_and_go_to() {
    let mut p = plan(vec![clip("A", 10), clip("B", 10)]);
    p.end = EndOfShow::Loop;
    let mut g = Rig::new(p.clone());
    g.start(0, Landing::PlayOn);
    let e = g.at(20);
    assert!(
        has(&e, |e| *e
            == Effect::Jumped {
                from_ms: 20,
                to_ms: 0
            }),
        "{e:?}"
    );
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.w.program, Some((0, Some(0))));
    g.at(25);
    assert_eq!(g.pos(), 5);

    // Loop with a pausing first bookmark lands paused there.
    p.bookmarks = vec![bm(0, 5, true), bm(1, 15, false)];
    let mut g = Rig::new(p.clone());
    g.start(0, Landing::PlayOn);
    g.at(5);
    g.cont();
    g.at(20);
    assert!(matches!(
        g.r.status(),
        Status::Paused {
            bookmark: Some(0),
            ..
        }
    ));
    assert_eq!(g.pos(), 5);
    assert_eq!(g.w.program, Some((0, Some(0))));

    // GoTo a non-pausing bookmark plays on from it.
    p.end = EndOfShow::GoTo(1);
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(5);
    g.cont();
    g.at(20);
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 15);
    assert_eq!(g.w.program, Some((1, Some(10))));
}

#[test]
fn pause_limit_continues_by_itself() {
    let mut p = plan(vec![clip("A", 20)]);
    p.bookmarks = vec![Bookmark {
        limit_ms: Some(500),
        ..bm(0, 10, true)
    }];
    p.events = vec![ev(10, Act::Shot(1))];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(10);
    assert!(matches!(g.r.status(), Status::Paused { .. }));
    assert_eq!(g.r.next_due(&g.plan), Some(510 * MS));
    assert!(g.at(509).is_empty());
    let steps = g.tick_steps(515 * MS);
    assert!(
        steps
            .iter()
            .any(|s| s.at_ns == 510 * MS && s.effect == Effect::Resumed { at_ms: 10 })
    );
    assert!(
        steps
            .iter()
            .any(|s| s.at_ns == 510 * MS && matches!(s.effect, Effect::Action { .. }))
    );
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 15);
    assert_eq!(g.w.shots, vec![1]);
}

#[test]
fn off_script_continue_goes_to_the_next_clip() {
    let mut g = Rig::new(plan(vec![clip("A", 10), clip("B", 10)]));
    g.start(0, Landing::PlayOn);
    g.at(5);
    g.off_script();
    assert_eq!(g.r.status(), Status::OffScript { since_ns: 5 * MS });
    assert!(g.at(50).is_empty());
    assert_eq!(g.pos(), 5);
    assert_eq!(g.r.next_due(&g.plan), None);
    assert_eq!(g.next(), Next::Jump(10));
    g.cont();
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 10);
    assert_eq!(g.w.program, Some((1, Some(10))));
}

#[test]
fn back_to_script_recuts_the_play_heads_clip() {
    let mut g = Rig::new(plan(vec![clip("A", 10), clip("B", 10)]));
    g.start(0, Landing::PlayOn);
    g.at(5);
    g.off_script();
    g.at(50);
    let e = g.back_to_script();
    assert!(
        has(&e, |e| matches!(
            e,
            Effect::Program {
                clip: 0,
                offset_ms: 5,
                ..
            }
        )),
        "{e:?}"
    );
    assert_eq!(g.r.status(), Status::Playing);
    assert_eq!(g.pos(), 5);
    g.at(55);
    assert_eq!(g.w.program, Some((1, Some(10))));
}

#[test]
fn stop_takes_overlays_down_and_stops_sequences() {
    let mut p = plan(vec![clip("A", 30)]);
    p.overlays = vec![overlay(0, "O", 0, 30)];
    p.sequences = vec![seq(2, 0, 30)];
    p.events = vec![ev(1, Act::Set(0, 4))];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(5);
    assert_eq!(g.w.overlays.len(), 1);
    assert_eq!(g.w.sequences.len(), 1);
    let e = g.stop();
    assert!(has(&e, |e| matches!(e, Effect::OverlayOff { clip: 0, .. })));
    assert!(has(&e, |e| matches!(
        e,
        Effect::SequenceStop {
            clip: 0,
            timeline: 2
        }
    )));
    assert!(g.w.overlays.is_empty() && g.w.sequences.is_empty());
    assert_eq!(g.w.program, Some((0, Some(0))));
    assert_eq!(g.w.states, BTreeMap::from([(0, 4)]));
    assert_eq!(g.r.status(), Status::Stopped);
    assert!(g.at(10).is_empty());
    assert_eq!(g.next(), Next::Nothing);
    assert_eq!(g.r.next_due(&g.plan), None);
}

#[test]
fn one_shots_are_not_replayed_on_a_jump() {
    let mut p = plan(vec![clip("A", 20)]);
    p.events = vec![ev(5, Act::Shot(1)), ev(12, Act::Shot(2))];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(15);
    assert_eq!(g.w.shots, vec![1, 2]);
    g.jump(2, Landing::PlayOn);
    g.jump(8, Landing::PlayOn);
    assert_eq!(g.w.shots, vec![1, 2]);
    // A play-on landing fires what is at its point; a pausing one waits for Continue.
    g.jump(12, Landing::PlayOn);
    assert_eq!(g.w.shots, vec![1, 2, 2]);
    g.jump(5, Landing::Pausing);
    assert_eq!(g.w.shots, vec![1, 2, 2]);
    g.cont();
    assert_eq!(g.w.shots, vec![1, 2, 2, 1]);
}

#[test]
fn state_events_are_reapplied_last_per_key() {
    let mut p = plan(vec![clip("A", 20)]);
    p.events = vec![
        ev(2, Act::Set(0, 1)),
        ev(3, Act::Set(1, 9)),
        ev(4, Act::Set(0, 2)),
        ev(5, Act::Shot(4)),
        ev(8, Act::Set(0, 3)),
    ];
    let mut g = Rig::new(p);
    let e = g.start(6, Landing::PlayOn);
    assert_eq!(actions(&e), vec![1, 2]);
    assert_eq!(g.w.states, BTreeMap::from([(0, 2), (1, 9)]));
    assert!(g.w.shots.is_empty());
}

#[test]
fn a_hand_set_value_is_left_alone() {
    let mut p = plan(vec![clip("A", 20)]);
    p.events = vec![
        ev(2, Act::Set(0, 1)),
        ev(3, Act::Set(1, 5)),
        ev(6, Act::Set(0, 3)),
    ];
    let mut g = Rig::new(p);
    g.start(0, Landing::PlayOn);
    g.at(4);
    // By hand: key 0 (the show set it to 1) and key 2 (the show never sets it).
    g.w.states.insert(0, 99);
    g.w.states.insert(2, 7);
    let e = g.jump(3, Landing::PlayOn);
    assert_eq!(
        actions(&e),
        vec![1],
        "only the event at the landing point fires"
    );
    assert_eq!(g.w.states, BTreeMap::from([(0, 99), (1, 5), (2, 7)]));
    // Once the show has set key 0 again, a jump back re-applies its earlier value.
    g.at(7);
    assert_eq!(g.w.states.get(&0), Some(&3));
    let e = g.jump(4, Landing::PlayOn);
    assert_eq!(actions(&e), vec![0]);
    assert_eq!(g.w.states, BTreeMap::from([(0, 1), (1, 5), (2, 7)]));
}

#[test]
fn next_and_next_due() {
    let mut p = plan(vec![clip("A", 10), clip("B", 10)]);
    p.bookmarks = vec![bm(0, 15, true)];
    let mut g = Rig::new(p);
    assert_eq!(g.r.status(), Status::Stopped);
    assert_eq!(g.next(), Next::Nothing);
    assert_eq!(g.r.next_due(&g.plan), None);
    let c0 = 1000;
    g.now = c0;
    g.start(0, Landing::PlayOn);
    assert_eq!(g.r.next_due(&g.plan), Some(c0 + 10 * MS));
    assert_eq!(g.next(), Next::Jump(15));
    g.tick_ns(c0 + 10 * MS);
    assert_eq!(g.r.next_due(&g.plan), Some(c0 + 15 * MS));
    g.tick_ns(c0 + 15 * MS);
    assert_eq!(g.next(), Next::Resume);
    assert_eq!(g.r.next_due(&g.plan), None);
    g.tick_ns(c0 + 17 * MS);
    g.cont();
    assert_eq!(
        g.r.next_due(&g.plan),
        Some(c0 + 22 * MS),
        "the end, 5 ms of show time on"
    );
    assert_eq!(g.next(), Next::Nothing);
    g.tick_ns(c0 + 22 * MS);
    assert_eq!(g.r.status(), Status::Ended);
    assert_eq!(g.next(), Next::Nothing);
    assert_eq!(g.r.next_due(&g.plan), None);
}

// ---------------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------------

const SCENES: [&str; 6] = ["s0", "s1", "s2", "s3", "s4", "s5"];
const PRESETS: [Option<&str>; 3] = [None, Some("fade"), Some("wipe")];

type RawProgram = Vec<(u32, bool, usize)>;
type RawOverlays = Vec<(u8, u32, u32, bool, u32)>;
type RawMarks = Vec<(u32, usize)>;
type RawSeqs = Vec<(u8, u32, u32)>;
type RawEvents = Vec<(u32, u8, i32)>;
type RawBookmarks = Vec<(u32, bool, usize, Option<u32>)>;
type RawLoops = Vec<(u32, u32, bool)>;
type RawPlan = (
    RawProgram,
    RawOverlays,
    RawMarks,
    RawSeqs,
    RawEvents,
    RawBookmarks,
    RawLoops,
);

/// The show variety to generate.
#[derive(Clone, Copy)]
struct Variety {
    loops: bool,
    /// Random leave modes, ends, limits; else FinishPass, Stop and no limits.
    anything: bool,
}

fn build(raw: RawPlan, end: u8, v: Variety) -> Plan<K> {
    let (prog, ovs, marks, seqs, evs, bms, loops) = raw;
    let mut p = plan(
        prog.into_iter()
            .enumerate()
            .map(|(i, (length_ms, timed, tr))| ProgramClip {
                scene: SCENES[i % SCENES.len()],
                length_ms,
                transition: PRESETS[tr % 3],
                timed,
            })
            .collect(),
    );
    p.default_transition = Some("dflt");
    let mut ovs = ovs;
    ovs.sort_by_key(|o| o.1);
    let mut track_end: BTreeMap<u8, u32> = BTreeMap::new();
    for (track, start, len, timed, fade) in ovs {
        if track_end.get(&track).is_some_and(|e| start < *e) {
            continue;
        }
        track_end.insert(track, start + len);
        p.overlays.push(OverlayClip {
            track,
            scene: SCENES[usize::from(track)],
            start_ms: start,
            length_ms: len,
            fade_in_ms: fade,
            fade_out_ms: fade,
            timed,
        });
    }
    let mut marks = marks;
    marks.sort_by_key(|m| m.0);
    p.keyframes = marks
        .into_iter()
        .map(|(at_ms, s)| KeyframeMark {
            at_ms,
            keyframe: SCENES.get(s).copied(),
            preset: None,
        })
        .collect();
    let mut seqs = seqs;
    seqs.sort_by_key(|s| s.1);
    p.sequences = seqs.into_iter().map(|(tl, s, l)| seq(tl, s, l)).collect();
    let mut evs = evs;
    evs.sort_by_key(|e| e.0);
    p.events = evs
        .into_iter()
        .map(|(at, kind, val)| {
            ev(
                at,
                if kind < 3 {
                    Act::Set(kind, val)
                } else {
                    Act::Shot(u32::from(kind))
                },
            )
        })
        .collect();
    let mut bms = bms;
    bms.sort_by_key(|b| b.0);
    p.bookmarks = bms
        .into_iter()
        .enumerate()
        .map(|(i, (at_ms, pausing, arrive, limit))| Bookmark {
            id: i as u32,
            at_ms,
            pausing,
            arrive: PRESETS[arrive % 3],
            limit_ms: if v.anything { limit } else { None },
        })
        .collect();
    let len = p.length_ms();
    if v.loops {
        let mut loops = loops;
        loops.sort_by_key(|l| l.0);
        let mut last_end = 0;
        for (from, l, at_once) in loops {
            let to = from + l;
            if from < last_end || to > len {
                continue;
            }
            last_end = to;
            let leave = if v.anything && at_once {
                Leave::AtOnce
            } else {
                Leave::FinishPass
            };
            p.loops.push(LoopRegion {
                id: p.loops.len() as u8,
                from_ms: from,
                to_ms: to,
                leave,
                wrap: PRESETS[usize::from(at_once)],
            });
        }
    }
    if v.anything {
        p.end = match end % 3 {
            0 => EndOfShow::Stop,
            1 => EndOfShow::Loop,
            _ if p.bookmarks.is_empty() => EndOfShow::Stop,
            _ => EndOfShow::GoTo(u32::from(end) % p.bookmarks.len() as u32),
        };
    }
    p
}

fn arb_plan(v: Variety) -> impl Strategy<Value = Plan<K>> {
    use proptest::collection::vec;
    (
        (
            vec((1u32..30, any::<bool>(), 0usize..3), 1..5),
            vec((0u8..3, 0u32..100, 1u32..40, any::<bool>(), 0u32..4), 0..5),
            vec((0u32..100, 0usize..7), 0..4),
            vec((0u8..3, 0u32..100, 1u32..40), 0..4),
            vec((0u32..100, 0u8..6, 0i32..4), 0..8),
            vec(
                (
                    1u32..100,
                    any::<bool>(),
                    0usize..3,
                    proptest::option::of(0u32..20),
                ),
                0..5,
            ),
            vec((0u32..100, 1u32..40, any::<bool>()), 0..3),
        ),
        any::<u8>(),
    )
        .prop_map(move |(raw, end)| build(raw, end, v))
}

/// Where world A plays to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// Until it pauses at a pausing bookmark at `t`.
    PauseAt(u32),
    /// Until the play head is at `t` and the things at `t` have happened.
    At(u32),
    /// Until the play head is just before `t` (a nanosecond short).
    Before(u32),
}

/// Plays from wherever `g` is to `tgt`, ticking in steps of `steps` milliseconds,
/// pressing Continue at every pause and to leave a loop the target is past.
fn play_until(g: &mut Rig, tgt: Target, steps: &[u32]) {
    let t = match tgt {
        Target::PauseAt(t) | Target::At(t) | Target::Before(t) => t,
    };
    let len = g.plan.length_ms();
    let mut left = vec![];
    for i in 0..20_000 {
        let pos = g.pos();
        match g.r.status() {
            Status::Paused { .. } => {
                if pos == t && tgt == Target::PauseAt(t) {
                    return;
                }
                g.cont();
                if pos == t && tgt == Target::At(t) {
                    return;
                }
                continue;
            }
            Status::Ended => {
                assert!(
                    tgt == Target::At(len) && t == len,
                    "ended at {pos} short of {tgt:?}: {:#?}",
                    g.plan
                );
                return;
            }
            Status::Playing => {}
            s => panic!("unexpected status {s:?} on the way to {tgt:?}"),
        }
        if let Next::LeaveLoop(l) = g.next() {
            let to = g.plan.loops[l].to_ms;
            let past = match tgt {
                Target::Before(t) => t > to,
                _ => t >= to,
            };
            if past && !left.contains(&l) {
                left.push(l);
                g.cont();
                continue;
            }
        }
        match tgt {
            Target::At(t) if pos == t => return,
            Target::Before(_) | Target::At(_) | Target::PauseAt(_) => {
                assert!(
                    pos < t,
                    "passed {tgt:?} at {pos} without stopping: {:#?}",
                    g.plan
                );
            }
        }
        let short = if matches!(tgt, Target::Before(_)) {
            1
        } else {
            0
        };
        let goal = g.now + i64::from(t - pos) * MS - short;
        let mut to = goal.min(g.now + i64::from(steps[i % steps.len()]) * MS);
        // Stop at a loop's start, to leave it before its first wrap if the target is
        // past it.
        if let Some(l) = g
            .plan
            .loops
            .iter()
            .find(|l| l.from_ms > pos && l.from_ms < t)
        {
            to = to.min(g.now + i64::from(l.from_ms - pos) * MS);
        }
        g.tick_ns(to);
        if to == goal && short == 1 && g.r.status() == Status::Playing {
            return;
        }
    }
    panic!("never reached {tgt:?}: {:#?}", g.plan);
}

/// World B's final move.
#[derive(Clone, Copy, Debug)]
enum Land {
    GoTo(usize),
    Jump(u32, Landing),
}

fn jump_equals_play(
    plan: Plan<K>,
    pick: (bool, u32, bool),
    prior: u32,
    prior_jump: Option<(u32, bool)>,
    steps_a: &[u32],
    steps_b: &[u32],
) -> Result<(), TestCaseError> {
    let len = plan.length_ms();
    let (use_bm, a, pausing) = pick;
    let (tgt, land) = if use_bm && !plan.bookmarks.is_empty() {
        let i = a as usize % plan.bookmarks.len();
        let b = &plan.bookmarks[i];
        let tgt = if b.pausing {
            Target::PauseAt(b.at_ms)
        } else {
            Target::At(b.at_ms)
        };
        (tgt, Land::GoTo(i))
    } else {
        let t = a % (len + 1);
        if pausing && t > 0 {
            (Target::Before(t), Land::Jump(t, Landing::Pausing))
        } else {
            (Target::At(t), Land::Jump(t, Landing::PlayOn))
        }
    };
    let t = match tgt {
        Target::PauseAt(t) | Target::At(t) | Target::Before(t) => t,
    };

    let mut ga = Rig::new(plan.clone());
    ga.start(0, Landing::PlayOn);
    play_until(&mut ga, tgt, steps_a);

    let mut gb = Rig::new(plan);
    gb.start(0, Landing::PlayOn);
    play_until(&mut gb, Target::At(prior % (len + 1)), steps_b);
    if let Some((j, pausing)) = prior_jump {
        let landing = if pausing {
            Landing::Pausing
        } else {
            Landing::PlayOn
        };
        gb.jump(j % (len + 1), landing);
        gb.at(gb.now / MS + 3);
    }
    gb.at(gb.now / MS + 1);
    match land {
        Land::GoTo(i) => gb.go_to(i),
        Land::Jump(t, l) => gb.jump(t, l),
    };

    prop_assert_eq!(
        ga.w.owned(&ga.plan, t),
        gb.w.owned(&gb.plan, t),
        "target {:?} via {:?}, B's prior {} then {:?}",
        tgt,
        land,
        prior % (len + 1),
        prior_jump
    );
    for (k, v) in &ga.w.states {
        prop_assert_eq!(gb.w.states.get(k), Some(v), "state key {}", k);
    }
    if !matches!(tgt, Target::At(_)) {
        prop_assert!(
            matches!(gb.r.status(), Status::Paused { .. }),
            "B: {:?}",
            gb.r.status()
        );
        prop_assert_eq!(gb.pos(), t);
    } else if gb.r.status() != Status::Ended {
        prop_assert_eq!(gb.pos(), t);
    }
    Ok(())
}

/// A random operation on a running show.
#[derive(Clone, Debug)]
enum Op {
    Tick(i64),
    Continue,
    Back,
    Skip,
    GoTo(usize),
    Jump(u32, bool),
    Arm(Option<u32>),
    OffScript,
    BackToScript,
    Stop,
    Start(u32, bool),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0i64..30 * MS).prop_map(Op::Tick),
        3 => (0i64..60).prop_map(|ms| Op::Tick(ms * MS)),
        2 => Just(Op::Continue),
        1 => Just(Op::Back),
        1 => Just(Op::Skip),
        1 => any::<usize>().prop_map(Op::GoTo),
        1 => (0u32..200, any::<bool>()).prop_map(|(t, p)| Op::Jump(t, p)),
        1 => proptest::option::of(0u32..200).prop_map(Op::Arm),
        1 => Just(Op::OffScript),
        1 => Just(Op::BackToScript),
        1 => Just(Op::Stop),
        1 => (0u32..200, any::<bool>()).prop_map(|(t, p)| Op::Start(t, p)),
    ]
}

fn landing(pausing: bool) -> Landing {
    if pausing {
        Landing::Pausing
    } else {
        Landing::PlayOn
    }
}

fn run_ops(plan: Plan<K>, ops: Vec<Op>) -> Result<(), TestCaseError> {
    let mut g = Rig::new(plan);
    g.now = 7;
    g.start(0, Landing::PlayOn);
    for op in ops {
        match op {
            Op::Tick(d) => {
                g.tick_ns(g.now + d);
            }
            Op::Continue => {
                g.cont();
            }
            Op::Back => {
                g.back();
            }
            Op::Skip => {
                g.skip();
            }
            Op::GoTo(i) => {
                if !g.plan.bookmarks.is_empty() {
                    g.go_to(i % g.plan.bookmarks.len());
                }
            }
            Op::Jump(t, p) => {
                g.jump(t, landing(p));
            }
            Op::Arm(a) => g.r.arm(a),
            Op::OffScript => {
                if matches!(g.r.status(), Status::Playing | Status::Paused { .. }) {
                    g.off_script();
                }
            }
            Op::BackToScript => {
                if matches!(g.r.status(), Status::OffScript { .. }) {
                    g.back_to_script();
                }
            }
            Op::Stop => {
                g.stop();
            }
            Op::Start(t, p) => {
                // A start begins from a fresh view of what is up.
                g.w = World {
                    states: std::mem::take(&mut g.w.states),
                    ..World::default()
                };
                g.start(t, landing(p));
            }
        }
        let _ = g.next();
        if let Some(due) = g.r.next_due(&g.plan) {
            let paused = matches!(g.r.status(), Status::Paused { .. });
            prop_assert!(
                due >= g.now || paused,
                "next_due {} before now {}",
                due,
                g.now
            );
        }
        // The runner's view of what it put up matches what its effects did.
        let applied = g.r.applied();
        let mut overlays = applied.overlays.clone();
        overlays.sort_unstable();
        prop_assert_eq!(overlays, g.w.overlays.keys().copied().collect::<Vec<_>>());
        prop_assert_eq!(
            applied.sequences.clone(),
            g.w.sequences
                .iter()
                .map(|(t, (c, _))| (*t, *c))
                .collect::<BTreeMap<_, _>>()
        );
        if let Some(c) = applied.program {
            prop_assert_eq!(g.program_clip(), Some(c));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn jump_equals_play_without_loops(
        plan in arb_plan(Variety { loops: false, anything: false }),
        pick in (any::<bool>(), any::<u32>(), any::<bool>()),
        prior in any::<u32>(),
        prior_jump in proptest::option::of((any::<u32>(), any::<bool>())),
        steps_a in proptest::collection::vec(1u32..15, 1..8),
        steps_b in proptest::collection::vec(1u32..15, 1..8),
    ) {
        jump_equals_play(plan, pick, prior, prior_jump, &steps_a, &steps_b)?;
    }

    #[test]
    fn jump_equals_play_with_loops(
        plan in arb_plan(Variety { loops: true, anything: false }),
        pick in (any::<bool>(), any::<u32>(), any::<bool>()),
        prior in any::<u32>(),
        prior_jump in proptest::option::of((any::<u32>(), any::<bool>())),
        steps_a in proptest::collection::vec(1u32..15, 1..8),
        steps_b in proptest::collection::vec(1u32..15, 1..8),
    ) {
        jump_equals_play(plan, pick, prior, prior_jump, &steps_a, &steps_b)?;
    }

    #[test]
    fn random_operations_never_panic(
        plan in arb_plan(Variety { loops: true, anything: true }),
        ops in proptest::collection::vec(arb_op(), 0..40),
    ) {
        run_ops(plan, ops)?;
    }
}

#[test]
fn end_loop_to_a_bookmark_at_the_end_does_not_spin() {
    let mut p = plan(vec![clip("A", 1)]);
    p.bookmarks = vec![bm(0, 1, false)];
    p.end = EndOfShow::Loop;
    let mut g = Rig::new(p);
    g.now = 7;
    g.start(0, Landing::PlayOn);
    g.tick_ns(1_000_008);
    for n in [1_000_009, 2_000_000, 5_000_000] {
        let before = g.now;
        let steps = g.tick_steps(n);
        assert!(
            steps.iter().all(|s| s.at_ns > before),
            "stale steps at {n}: {steps:?}"
        );
        assert!(
            g.pos() <= g.plan.length_ms(),
            "position {} past the end",
            g.pos()
        );
        if let Some(due) = g.r.next_due(&g.plan) {
            assert!(due >= n, "next_due {due} before now {n}");
        }
    }
}

#[test]
fn a_loop_wrap_plays_again_a_sequence_clip_that_ends_at_the_loops_end() {
    let mut p = plan(vec![clip("A", 100)]);
    p.sequences.push(seq(1, 10, 20));
    p.loops.push(lp(0, 10, 30, Leave::AtOnce));
    let mut rig = Rig::new(p);
    rig.start(0, Landing::PlayOn);
    let at_start = rig.at(10);
    assert!(
        at_start.contains(&Effect::SequencePlay {
            clip: 0,
            timeline: 1,
            offset_ms: 0
        }),
        "{at_start:?}"
    );
    for (pass, at) in [(1, 30), (2, 50)] {
        let wrap = rig.at(at);
        assert_eq!(
            wrap,
            vec![
                Effect::Looped { region: 0, pass },
                Effect::SequenceStop {
                    clip: 0,
                    timeline: 1
                },
                Effect::SequencePlay {
                    clip: 0,
                    timeline: 1,
                    offset_ms: 0
                },
            ],
            "pass {pass}"
        );
    }
}
