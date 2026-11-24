//! The Sequence panel (ADR-0184, UI-FEATURES SQ-01): the open sequence of the selected
//! scene, a timeline with lanes. Its top bar picks the sequence (or makes one), plays or
//! stops it, loops it, sets its length and adds a lane by searching the channels. Its
//! trigger bar lists what starts and stops it as sentences, with "+ Trigger" to add one.
//!
//! Each lane is one channel over the timeline's length: a double-click adds a point
//! (snapped to a tenth of a second, Shift for free), a drag moves one, and a click or a
//! right-click opens its menu (its curve, time and value, delete). A switch lane is its
//! on bars instead: a double-click in a gap adds one, a drag moves a bar or one of its
//! ends, and a click or a right-click opens its menu (on at, off at, delete). Each
//! gesture is one `timeline.set`, so one undo step. A lane with no point at 0 starts from the channel's value, drawn dashed from
//! where the channel is now. The play head follows `timeline.changed`, and a click on
//! the ruler seeks there.
//!
//! Docs: docs/adr/0184-sequences-lanes-and-triggers.md, docs/UI-FEATURES.md#automation

use crate::app::SessionContext;
use crate::dialog::Info;
use crate::icons::Icon;
use crate::menu::{Entry, Menus};
use crate::panels::channels::{live_values, new_channel, open_channel, search};
use crate::panels::lane_offers::{
    self, Grip, Offer, bar_points, drag_bar, followed_channel, insert_bar, is_switch, kinds_needed,
    offers, switch_bars, switch_points,
};
use crate::panels::learn::follows;
use crate::panels::number::NumberField;
use crate::session::Session;
use crate::store::{Collection, Studio, now_ms};
use jstream_control::protocol::MIN_LOOP_MS;
use jstream_control::protocol::{
    Action, ChannelEdge, ChannelValueInfo, Edit, Finite32, KeyframeEdge, LaneCurve, LanePoint,
    LaneTarget, Rev, SceneEdge, SceneId, TimelineEdge, TimelineId, TimelineInfo, TimelineLane,
    TimelineLoop, TimelineTrigger,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::JsCast;
use std::collections::HashMap;
use std::time::Duration;

/// How many lane colours there are: each lane takes `lane-N` (style.css), from the
/// palette's tokens, in lane order.
const COLOURS: usize = 4;

/// Where a dragged point snaps to without Shift, in ms.
const SNAP_MS: u32 = 100;

/// The scene whose sequences the panel lists for `scene`: a keyframe's parent.
pub fn family(collection: &Collection, scene: &SceneId) -> SceneId {
    collection
        .parent_of(scene)
        .map_or_else(|| scene.clone(), |p| p.scene_id.clone())
}

/// A timeline's name as the panel shows it.
pub fn timeline_name(t: &TimelineInfo) -> String {
    if t.name.is_empty() {
        t.timeline_id.to_string()
    } else {
        t.name.clone()
    }
}

/// A new sequence's ID and name: `seq_N` and "Sequence N", free in `collection`.
pub fn fresh_sequence(collection: &Collection) -> (TimelineId, String) {
    let taken = |id: &str| {
        collection
            .scenes
            .iter()
            .flat_map(|s| &s.timelines)
            .any(|t| t.timeline_id.as_str() == id)
    };
    let n = (1..).find(|n| !taken(&format!("seq_{n}"))).unwrap_or(1);
    (TimelineId::new(format!("seq_{n}")), format!("Sequence {n}"))
}

/// An empty sequence, six seconds long.
pub fn empty_sequence(id: TimelineId, name: String) -> TimelineInfo {
    TimelineInfo {
        folder: None,
        timeline_id: id,
        name,
        duration_ms: 6000,
        looping: TimelineLoop::Once,
        cues: Vec::new(),
        labels: Vec::new(),
        lanes: Vec::new(),
        starts: Vec::new(),
        stops: Vec::new(),
    }
}

/// `ms` as seconds, as a chip says it: "0.5 s".
pub(crate) fn seconds(ms: u32) -> String {
    let text = format!("{:.2}", f64::from(ms) / 1000.0);
    format!("{} s", text.trim_end_matches('0').trim_end_matches('.'))
}

/// A trigger as a chip's sentence: "Stage power rises", "Keyframe “Wide” loads · after
/// 0.5 s".
pub fn trigger_text(collection: &Collection, trigger: &TimelineTrigger) -> String {
    let scene_name = |id: &SceneId| {
        collection
            .scene(id)
            .map_or_else(|| id.to_string(), |s| s.name.clone())
    };
    let what = match trigger {
        TimelineTrigger::Channel {
            channel_id,
            edge,
            level,
            ..
        } => {
            let name = collection.channel_name(channel_id);
            let past = level
                .filter(|l| (l.get() - 0.5).abs() > f32::EPSILON)
                .map(|l| format!(" past {:.0} %", l.get() * 100.0))
                .unwrap_or_default();
            match edge {
                ChannelEdge::Rises => format!("{name} rises{past}"),
                ChannelEdge::Falls => format!("{name} falls{past}"),
                ChannelEdge::On => format!("{name} switches on"),
                ChannelEdge::Off => format!("{name} switches off"),
            }
        }
        TimelineTrigger::Scene { scene_id, edge, .. } => {
            let name = scene_name(scene_id);
            match edge {
                SceneEdge::Hidden => format!("Scene “{name}” leaves program"),
                _ => format!("Scene “{name}” goes on program"),
            }
        }
        TimelineTrigger::Keyframe {
            scene_id,
            keyframe_id,
            edge,
            ..
        } => {
            let verb = match edge {
                KeyframeEdge::Leaves => "leaves",
                _ => "loads",
            };
            match keyframe_id {
                Some(k) => format!("Keyframe “{}” {verb}", scene_name(k)),
                None => format!("Any keyframe of “{}” {verb}", scene_name(scene_id)),
            }
        }
        TimelineTrigger::Timeline {
            timeline_id, edge, ..
        } => {
            let name = collection
                .timeline(timeline_id)
                .map_or_else(|| timeline_id.to_string(), |(_, t)| timeline_name(t));
            let verb = match edge {
                TimelineEdge::Starts => "starts",
                TimelineEdge::Loops => "loops",
                _ => "ends",
            };
            format!("“{name}” {verb}")
        }
        _ => "Something".into(),
    };
    match trigger.delay_ms() {
        0 => what,
        ms => format!("{what} · after {}", seconds(ms)),
    }
}

/// `points` with a point at `at_ms` of `value` added in time order, replacing one
/// already there. Returns its index.
pub fn add_point(points: &mut Vec<LanePoint>, at_ms: u32, value: f32) -> usize {
    let point = LanePoint {
        at_ms,
        value: Finite32::new(value.clamp(0.0, 1.0)).unwrap_or(Finite32::ZERO),
        curve: LaneCurve::Linear,
    };
    match points.binary_search_by_key(&at_ms, |p| p.at_ms) {
        Ok(n) => {
            points[n].value = point.value;
            n
        }
        Err(n) => {
            points.insert(n, point);
            n
        }
    }
}

/// Point `n` moved to `at_ms` and `value`, kept between its neighbours so the points
/// stay in time order, and within `0..=duration_ms`.
pub fn move_point(points: &mut [LanePoint], n: usize, at_ms: u32, value: f32, duration_ms: u32) {
    let low = if n == 0 { 0 } else { points[n - 1].at_ms + 1 };
    let high = points
        .get(n + 1)
        .map_or(duration_ms, |p| p.at_ms.saturating_sub(1));
    if let Some(point) = points.get_mut(n) {
        point.at_ms = at_ms.clamp(low, high.max(low));
        point.value = Finite32::new(value.clamp(0.0, 1.0)).unwrap_or(Finite32::ZERO);
    }
}

/// Adds a lane on `channel_id` to `t`, holding `level` at its end, unless it has one:
/// whether it added it.
pub fn with_lane(t: &mut TimelineInfo, channel_id: &str, level: f32) -> bool {
    if t.lanes.iter().any(|l| l.target.channel == channel_id) {
        return false;
    }
    t.lanes.push(TimelineLane {
        target: LaneTarget {
            channel: channel_id.to_owned(),
        },
        points: vec![LanePoint {
            at_ms: t.duration_ms,
            value: Finite32::new(level.clamp(0.0, 1.0)).unwrap_or(Finite32::ZERO),
            curve: LaneCurve::Linear,
        }],
        on: true,
    });
    true
}

/// A loop range kept valid for a timeline `duration_ms` long: `to_ms` at most the
/// length, and at least [`MIN_LOOP_MS`] after `from_ms`. `None` when the timeline is
/// too short to hold one.
pub fn fit_range(from_ms: u32, to_ms: u32, duration_ms: u32) -> Option<TimelineLoop> {
    if duration_ms < MIN_LOOP_MS {
        return None;
    }
    let to_ms = to_ms.clamp(MIN_LOOP_MS, duration_ms);
    let from_ms = from_ms.min(to_ms - MIN_LOOP_MS);
    Some(TimelineLoop::Range { from_ms, to_ms })
}

/// What the loop button turns `looping` into: once, then the whole sequence, then a
/// range (its second half: an intro, then a loop), then once again.
pub fn next_loop(looping: TimelineLoop, duration_ms: u32) -> TimelineLoop {
    match looping {
        TimelineLoop::Once => TimelineLoop::Loop,
        TimelineLoop::Loop => {
            let half = snap(f64::from(duration_ms) / 2.0, false);
            fit_range(half, duration_ms, duration_ms).unwrap_or(TimelineLoop::Once)
        }
        _ => TimelineLoop::Once,
    }
}

/// Whether `a` and `b` watch the same thing the same way, whatever each waits first.
pub fn same_watch(a: &TimelineTrigger, b: &TimelineTrigger) -> bool {
    let mut a = serde_json::to_value(a).unwrap_or_default();
    let mut b = serde_json::to_value(b).unwrap_or_default();
    for v in [&mut a, &mut b] {
        if let Some(o) = v.as_object_mut() {
            o.remove("delay_ms");
        }
    }
    a == b
}

/// Adds `trigger` to `t`'s starts, or takes out the one that watches the same thing:
/// a menu line's check (A14).
pub fn toggle_start(t: &mut TimelineInfo, trigger: &TimelineTrigger) {
    if let Some(n) = t.starts.iter().position(|s| same_watch(s, trigger)) {
        t.starts.remove(n);
    } else {
        t.starts.push(trigger.clone());
    }
}

/// The "Play a sequence" submenu of a scene's or a keyframe's menu (A14, SQ-03): for
/// each of `watches` (a heading and the trigger it adds), every sequence of the
/// collection, checked while that trigger starts it; a click adds or removes the
/// trigger. Last, a new sequence in `family` that the first watch starts, opened in the
/// Sequence panel.
pub fn trigger_submenu(
    studio: Studio,
    session: SessionContext,
    family: SceneId,
    watches: Vec<(&'static str, TimelineTrigger)>,
) -> crate::menu::Entry {
    use crate::menu::Entry;
    let sequences: Vec<(SceneId, TimelineInfo)> = studio.collection.with_untracked(|c| {
        c.iter()
            .flat_map(|c| &c.scenes)
            .flat_map(|s| s.timelines.iter().map(|t| (s.scene_id.clone(), t.clone())))
            .collect()
    });
    let send = move |scene_id: SceneId, timeline: TimelineInfo| {
        if let Some(s) = session.get() {
            studio.apply(&s, vec![Edit::TimelineSet { scene_id, timeline }]);
        }
    };
    let mut lines = Vec::new();
    for (head, trigger) in &watches {
        lines.push(Entry::Head((*head).to_owned()));
        for (scene_id, t) in &sequences {
            let on = t.starts.iter().any(|s| same_watch(s, trigger));
            let (scene_id, id, trigger) =
                (scene_id.clone(), t.timeline_id.clone(), trigger.clone());
            lines.push(
                Entry::item(timeline_name(t), move || {
                    // Read again on the click, so another edit since the menu opened stays.
                    let now = studio.collection.with_untracked(|c| {
                        c.as_ref()
                            .and_then(|c| c.timeline(&id))
                            .map(|(_, t)| t.clone())
                    });
                    if let Some(mut t) = now {
                        toggle_start(&mut t, &trigger);
                        send(scene_id.clone(), t);
                    }
                })
                .checked(on),
            );
        }
    }
    if let Some((head, trigger)) = watches.into_iter().next() {
        if !sequences.is_empty() {
            lines.push(Entry::Sep);
        }
        lines.push(Entry::item(
            format!("New sequence: {}", head.to_lowercase()),
            move || {
                let Some((id, name)) = studio
                    .collection
                    .with_untracked(|c| c.as_ref().map(fresh_sequence))
                else {
                    return;
                };
                let mut t = empty_sequence(id.clone(), name);
                t.starts.push(trigger.clone());
                send(family.clone(), t);
                open_sequence(studio, id);
            },
        ));
    }
    Entry::submenu("Play a sequence", lines)
}

/// Opens sequence `timeline_id` in the Sequence panel, showing the panel.
pub fn open_sequence(studio: Studio, timeline_id: TimelineId) {
    studio.sequence.set(Some(timeline_id));
    if let Some(dock) = use_context::<crate::workspace::DockState>() {
        dock.0.update(|d| d.show(crate::dock::PanelId::Sequence));
    }
}

/// A time snapped to [`SNAP_MS`] unless `free`.
pub(crate) fn snap(at_ms: f64, free: bool) -> u32 {
    let ms = at_ms.max(0.0);
    if free {
        ms.round() as u32
    } else {
        ((ms / f64::from(SNAP_MS)).round() as u32) * SNAP_MS
    }
}

/// The lane's line in a `0 0 1000 100` box: sampled along each segment, holding after
/// the last point. Starts at `from` when the first point is after 0; that part is
/// returned apart so it can be drawn dashed.
pub fn lane_path(lane: &TimelineLane, duration_ms: u32, from: f32) -> (String, String) {
    let d = f64::from(duration_ms.max(1));
    let x = |ms: f64| ms / d * 1000.0;
    let y = |v: f32| (1.0 - f64::from(v)) * 100.0;
    let Some(first) = lane.points.first() else {
        return (
            String::new(),
            format!("M0 {:.2} L1000 {:.2}", y(from), y(from)),
        );
    };
    let lead = if first.at_ms > 0 {
        let mut path = format!("M0 {:.2}", y(from));
        let end = f64::from(first.at_ms);
        for step in 1..=24 {
            let ms = end * f64::from(step) / 24.0;
            path.push_str(&format!(" L{:.2} {:.2}", x(ms), y(lane.value_at(ms, from))));
        }
        path
    } else {
        String::new()
    };
    let mut path = format!(
        "M{:.2} {:.2}",
        x(f64::from(first.at_ms)),
        y(first.value.get())
    );
    for pair in lane.points.windows(2) {
        let (a, b) = (f64::from(pair[0].at_ms), f64::from(pair[1].at_ms));
        for step in 1..=24 {
            let ms = a + (b - a) * f64::from(step) / 24.0;
            path.push_str(&format!(" L{:.2} {:.2}", x(ms), y(lane.value_at(ms, from))));
        }
    }
    if let Some(last) = lane.points.last() {
        path.push_str(&format!(" L1000 {:.2}", y(last.value.get())));
    }
    (lead, path)
}

/// A curve's name in the point menu.
fn curve_name(curve: LaneCurve) -> &'static str {
    match curve {
        LaneCurve::EaseIn => "Ease in",
        LaneCurve::EaseOut => "Ease out",
        LaneCurve::EaseInOut => "Ease in and out",
        LaneCurve::Hold => "Hold, then jump",
        _ => "Linear",
    }
}

/// A point being dragged: its lane and index, where it is now, where the pointer went
/// down, and whether it has moved past a click's few pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    lane: usize,
    point: usize,
    at_ms: u32,
    value: f32,
    x0: f64,
    y0: f64,
    moved: bool,
}

/// A switch lane's bars, as `(on_ms, off_ms)` in time order.
type Bars = Vec<(u32, u32)>;

/// How far a pointer goes, in px, before a press is a drag and not a click.
const CLICK_SLOP: f64 = 3.0;

/// A switch lane's bar being dragged (by its body or one end): the lane's bars as they
/// were, which one, where it is now, how far into it the pointer went down, and whether
/// it has moved past a click's few pixels.
#[derive(Clone, Debug, PartialEq)]
struct BarDrag {
    lane: usize,
    bar: usize,
    grip: Grip,
    held: Vec<(u32, u32)>,
    now: (u32, u32),
    offset_ms: i64,
    x0: f64,
    moved: bool,
}

/// The menu a click or right-click on a switch lane's bar opens.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BarMenu {
    lane: usize,
    bar: usize,
    x: f64,
    y: f64,
}

/// The menu a right-click on a point opens, at a point in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PointMenu {
    lane: usize,
    point: usize,
    x: f64,
    y: f64,
}

/// The edits the panel sent that the engine hasn't shown back yet: the open sequence as
/// they leave it, so the panel draws it, and builds the next edit on it, at once.
///
/// Each edit is a whole `timeline.set`, so one built on the collection's copy while an
/// earlier one is still on its way (the engine answers, then the panel reads the
/// collection again) would send that copy back and undo the earlier one: a second point
/// added quickly took the first away, and a second click on M put the lane back. The
/// draft goes once every edit has been answered and the collection has caught up with
/// the last, or at once when one is refused, which the problem bar shows.
#[derive(Clone, Copy)]
struct Pending {
    /// The open sequence as the edits leave it, with the scene that holds it.
    draft: RwSignal<Option<(SceneId, TimelineInfo)>>,
    /// How many edits are on their way.
    sent: StoredValue<u32>,
    /// The revision the last answered edit made.
    rev: StoredValue<Rev>,
}

impl Pending {
    fn new(studio: Studio) -> Self {
        let pending = Pending {
            draft: RwSignal::new(None),
            sent: StoredValue::new(0),
            rev: StoredValue::new(Rev(0)),
        };
        // The collection caught up: draw from it again.
        Effect::new(move |_| {
            let rev = studio.collection.with(|c| c.as_ref().map(|c| c.rev));
            if rev.is_some_and(|rev| pending.settled(rev)) {
                pending.draft.set(None);
            }
        });
        pending
    }

    /// Whether nothing is on its way and the collection at `rev` holds the last edit.
    fn settled(self, rev: Rev) -> bool {
        self.sent.get_value() == 0 && rev >= self.rev.get_value()
    }

    /// Sends `edits`, the last of which leaves the sequence as `draft` (in `scene`), and
    /// draws the draft until the engine shows it back.
    fn send(
        self,
        studio: Studio,
        session: &Session,
        scene: SceneId,
        draft: TimelineInfo,
        edits: Vec<Edit>,
    ) {
        self.draft.set(Some((scene, draft)));
        self.sent.update_value(|n| *n += 1);
        let session = session.clone();
        spawn_local(async move {
            let answer = crate::store::apply(&session, edits, None).await;
            self.sent.update_value(|n| *n = n.saturating_sub(1));
            match answer {
                Ok(rev) => {
                    self.rev.update_value(|r| *r = (*r).max(rev));
                    let now = studio
                        .collection
                        .with_untracked(|c| c.as_ref().map(|c| c.rev));
                    if now.is_some_and(|now| self.settled(now)) {
                        self.draft.set(None);
                    }
                }
                Err(error) => {
                    studio.report(&error);
                    self.draft.set(None);
                }
            }
        });
    }
}

/// The Sequence panel.
#[component]
pub fn Sequence() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let values = live_values();
    // The time now, for the play head, about 30 times a second.
    let clock = RwSignal::new(now_ms());
    if let Ok(handle) = set_interval_with_handle(
        move || {
            clock.try_set(now_ms());
        },
        Duration::from_millis(33),
    ) {
        on_cleanup(move || handle.clear());
    }

    let family_id = move || {
        let scene = studio.selection.with(|s| s.scene.clone())?;
        studio
            .collection
            .with(|c| c.as_ref().map(|c| family(c, &scene)))
    };
    let list = move || {
        let Some(scene) = family_id() else {
            return Vec::new();
        };
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| c.scene(&scene))
                .map(|s| s.timelines.clone())
                .unwrap_or_default()
        })
    };
    let pending = Pending::new(studio);
    // The open sequence: the one picked, or this scene's first; as the panel's edits on
    // their way leave it, so it shows them at once.
    let open = Memo::new(move |_| {
        let picked = studio.sequence.get();
        let found = picked.clone().and_then(|id| {
            studio.collection.with(|c| {
                c.as_ref().and_then(|c| {
                    c.timeline(&id)
                        .map(|(s, t)| (s.scene_id.clone(), t.clone()))
                })
            })
        });
        let found = found.or_else(|| {
            let scene = family_id()?;
            list().into_iter().next().map(|t| (scene, t))
        });
        let draft = pending.draft.get().filter(|(_, d)| match &found {
            Some((_, t)) => t.timeline_id == d.timeline_id,
            // A sequence just made, not in the collection yet.
            None => picked.as_ref() == Some(&d.timeline_id),
        });
        draft.or(found)
    });
    let apply = move |scene_id: SceneId, timeline: TimelineInfo, mut edits: Vec<Edit>| {
        if let Some(s) = session.0.get_value() {
            edits.push(Edit::TimelineSet {
                scene_id: scene_id.clone(),
                timeline: timeline.clone(),
            });
            pending.send(studio, &s, scene_id, timeline, edits);
        }
    };
    let change = move |f: &dyn Fn(&mut TimelineInfo)| {
        let Some((scene, mut t)) = open.get_untracked() else {
            return false;
        };
        let before = t.clone();
        f(&mut t);
        // An edit that changes nothing sends nothing, so it leaves no empty undo step.
        if t == before {
            return false;
        }
        apply(scene, t, Vec::new());
        true
    };
    let act = move |action: Action| {
        if let Some(s) = session.0.get_value() {
            studio.act(&s, action);
        }
    };
    let create = move || {
        let Some(scene) = untrack(family_id) else {
            studio.problem.set(Some(
                "Select a scene first: a sequence belongs to a scene.".into(),
            ));
            return;
        };
        let Some((id, name)) = studio
            .collection
            .with_untracked(|c| c.as_ref().map(fresh_sequence))
        else {
            return;
        };
        studio.sequence.set(Some(id.clone()));
        apply(scene, empty_sequence(id, name), Vec::new());
    };
    let pick = move |e: leptos::ev::Event| {
        let v = event_target_value(&e);
        if v == "+new" {
            create();
        } else {
            studio.sequence.set(Some(TimelineId::new(v)));
        }
    };
    let playing = move || {
        let id = open.with(|o| o.as_ref().map(|(_, t)| t.timeline_id.clone()))?;
        studio.playing.with(|p| p.get(&id).copied())
    };
    let position = move || {
        let (_, t) = open.get()?;
        let p = playing()?;
        Some(p.position_at(clock.get(), t.duration_ms))
    };
    let play = move |_| {
        let Some((_, t)) = open.get_untracked() else {
            return;
        };
        if untrack(playing).is_some() {
            act(Action::TimelineStop {
                timeline_id: t.timeline_id,
            });
        } else {
            act(Action::TimelinePlay {
                timeline_id: t.timeline_id,
                delay_ms: None,
            });
        }
    };
    let looping = move || open.with(|o| o.as_ref().map(|(_, t)| t.looping));
    let looped = move || looping().is_some_and(|l| !matches!(l, TimelineLoop::Once));
    // Its state is the button's: lit while it loops, and "Loop range" for a range.
    let loop_text = move || match looping() {
        Some(TimelineLoop::Range { .. }) => "Loop range",
        _ => "Loop",
    };
    let toggle_loop = move |_| {
        change(&|t| t.looping = next_loop(t.looping, t.duration_ms));
    };
    // A range's ends, in seconds: `None` while the loop isn't a range.
    let range_end = move |to: bool| {
        Signal::derive(move || match looping()? {
            TimelineLoop::Range { from_ms, to_ms } => {
                Some(f64::from(if to { to_ms } else { from_ms }) / 1000.0)
            }
            _ => None,
        })
    };
    let set_range_end = move |to: bool, v: f64| {
        let ms = snap(v * 1000.0, true);
        change(&|t| {
            if let TimelineLoop::Range { from_ms, to_ms } = t.looping {
                let (from, until) = if to { (from_ms, ms) } else { (ms, to_ms) };
                if let Some(range) = fit_range(from, until, t.duration_ms) {
                    t.looping = range;
                }
            }
        })
    };
    let ranged = move || matches!(looping(), Some(TimelineLoop::Range { .. }));
    let length = Signal::derive(move || {
        open.with(|o| o.as_ref().map(|(_, t)| f64::from(t.duration_ms) / 1000.0))
    });
    let set_length = move |v: f64| {
        let ms = (v * 1000.0).round() as u32;
        change(&|t| {
            t.duration_ms = ms.max(100);
            if let TimelineLoop::Range { from_ms, to_ms } = t.looping {
                t.looping = fit_range(from_ms, to_ms, t.duration_ms).unwrap_or(TimelineLoop::Once);
            }
            for lane in &mut t.lanes {
                lane.points.retain(|p| p.at_ms <= t.duration_ms);
            }
            t.cues.retain(|c| c.at_ms <= t.duration_ms);
        })
    };
    // Rename, from the ⋯ menu: the picker turns into the name's field until Enter, a
    // click away or Escape.
    let renaming = RwSignal::new(false);
    let rename = move |name: Option<String>| {
        renaming.set(false);
        if let Some(name) = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()) {
            change(&|t| t.name = name.clone());
        }
    };
    let delete = move || {
        let Some((_, t)) = open.get_untracked() else {
            return;
        };
        if let Some(s) = session.0.get_value() {
            studio.sequence.set(None);
            pending.draft.set(None);
            studio.apply(
                &s,
                vec![Edit::TimelineDelete {
                    timeline_id: t.timeline_id,
                }],
            );
        }
    };

    // Add lane.
    let adding = RwSignal::new(false);
    // Where Add lane's search opens: under its button's right end, in the window, so a
    // short dock doesn't clip it.
    let adding_at = RwSignal::new((0.0_f64, 0.0_f64));
    let add_lane = move |channel_id: String| {
        adding.set(false);
        let level = values.with_untracked(|v| v.get(&channel_id).map_or(0.0, |c| c.value.get()));
        change(&|t| {
            with_lane(t, &channel_id, level);
        });
    };
    let make_lane = move |name: String| {
        adding.set(false);
        let Some((scene, mut t)) = open.get_untracked() else {
            return;
        };
        let channel = studio.collection.with_untracked(|c| {
            new_channel(
                &name,
                Some(scene.clone()),
                0.0,
                c.as_ref().map_or(&[][..], |c| &c.channels[..]),
            )
        });
        t.lanes.push(TimelineLane {
            target: LaneTarget {
                channel: channel.channel_id.clone(),
            },
            points: vec![LanePoint {
                at_ms: t.duration_ms,
                value: Finite32::ONE,
                curve: LaneCurve::Linear,
            }],
            on: true,
        });
        apply(scene, t, vec![Edit::ChannelSet { channel }]);
    };

    // A setting from Add lane's Settings or Switches group (A13): the channel it
    // follows, or a new one it follows from now on, and a lane on that channel (a
    // switch lane for an on/off setting), in one batch.
    let add_offer = move |offer: Offer| {
        adding.set(false);
        let Some((scene, mut t)) = open.get_untracked() else {
            return;
        };
        let mut edits = Vec::new();
        let followed = studio
            .collection
            .with_untracked(|c| c.as_ref().and_then(|c| followed_channel(c, &offer.target)));
        let channel_id = match followed {
            Some(id) => id,
            None => {
                let channel = studio.collection.with_untracked(|c| {
                    new_channel(
                        &offer.label,
                        Some(scene.clone()),
                        offer.at,
                        c.as_ref().map_or(&[][..], |c| &c.channels[..]),
                    )
                });
                let id = channel.channel_id.clone();
                let binding = follows(&id, &offer.target, offer.range);
                edits.push(Edit::ChannelSet { channel });
                edits.push(Edit::BindingSetValue { binding });
                id
            }
        };
        let level = values
            .with_untracked(|v| v.get(&channel_id).map(|c| c.value.get()))
            .unwrap_or(offer.at);
        if !with_lane(&mut t, &channel_id, level) {
            if edits.is_empty() {
                return;
            }
        } else if offer.switch
            && let Some(lane) = t.lanes.last_mut()
        {
            lane.points = switch_points(level >= 0.5, t.duration_ms);
        }
        apply(scene, t, edits);
    };

    let none = move || open.with(Option::is_none);
    let menus = use_context::<Menus>();
    let more = move |event: leptos::ev::MouseEvent| {
        let Some(menus) = menus else { return };
        menus.open(
            &event,
            vec![
                Entry::item("Rename", move || renaming.set(true)),
                Entry::Sep,
                Entry::item("Delete sequence", delete).danger(),
            ],
        );
    };
    let name_now =
        move || open.with(|o| o.as_ref().map(|(_, t)| t.name.clone()).unwrap_or_default());
    view! {
        <section class="panel seq-panel">
            <div class="seq-bar">
                <Icon name="sequence" size=14 />
                <Show
                    when=move || renaming.get() && !none()
                    fallback=move || view! {
                        <select class="seq-select" aria-label="Sequence" on:change=pick>
                            {move || {
                                let shown = open.get().map(|(_, t)| t);
                                let current = shown.as_ref().map(|t| t.timeline_id.clone());
                                // The open one as the panel's edits leave it: a sequence just
                                // made or renamed shows before the engine says so.
                                let mut all = list();
                                if let Some(t) = shown {
                                    match all.iter_mut().find(|l| l.timeline_id == t.timeline_id) {
                                        Some(l) => *l = t,
                                        None => all.push(t),
                                    }
                                }
                                let mut rows: Vec<_> = all
                                    .into_iter()
                                    .map(|t| {
                                        let id = t.timeline_id.to_string();
                                        let on = current.as_ref() == Some(&t.timeline_id);
                                        view! { <option value=id selected=on>{timeline_name(&t)}</option> }.into_any()
                                    })
                                    .collect();
                                if current.is_none() {
                                    rows.insert(0, view! { <option value="" selected=true>"No sequence"</option> }.into_any());
                                }
                                rows.push(view! { <option value="+new">"+ New sequence"</option> }.into_any());
                                rows
                            }}
                        </select>
                    }
                >
                    <RenameField value=name_now() done=rename />
                </Show>
                <Show when=move || !none()>
                    <button type="button" class="row-action more seq-more" aria-label="More for this sequence" title="More"
                        on:click=more>"⋯"</button>
                    <span class="seq-sep"></span>
                    <button type="button" class="sm seq-play" class:on=move || playing().is_some()
                        aria-pressed=move || if playing().is_some() { "true" } else { "false" }
                        on:click=play
                        title=move || if playing().is_some() { "Stop it" } else { "Play it from the start" }>
                        <span class="seq-play-mark" class:stop=move || playing().is_some()></span>
                        {move || if playing().is_some() { "Stop" } else { "Play" }}
                    </button>
                    <button type="button" class="sm seq-loop" class:on=looped
                        aria-pressed=move || if looped() { "true" } else { "false" }
                        on:click=toggle_loop
                        title="Loop: off, the whole sequence, or a range (it plays from the start, then repeats the range)">
                        {loop_text}
                    </button>
                    <Show when=ranged>
                        <label class="seq-field">
                            <span class="seq-label">"From"</span>
                            <NumberField named=Signal::derive(|| "Loop from".to_owned()) unit="s" value=range_end(false) step=0.1 decimals=2 min=0.0 max=3600.0
                                send=move |v| set_range_end(false, v) />
                        </label>
                        <label class="seq-field">
                            <span class="seq-label">"to"</span>
                            <NumberField named=Signal::derive(|| "Loop to".to_owned()) unit="s" value=range_end(true) step=0.1 decimals=2 min=0.1 max=3600.0
                                send=move |v| set_range_end(true, v) />
                        </label>
                    </Show>
                    <label class="seq-field">
                        <span class="seq-label">"Length"</span>
                        <NumberField named=Signal::derive(|| "Length".to_owned()) unit="s" value=length step=0.5 decimals=1 min=0.1 max=3600.0 send=set_length />
                    </label>
                    <span class="seq-spacer"></span>
                    <div class="seq-add">
                        <button type="button" class="sm seq-add-button" aria-haspopup="dialog" aria-expanded=move || if adding.get() { "true" } else { "false" }
                            on:click=move |e: leptos::ev::MouseEvent| {
                                if let Some(b) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                                    let r = crate::ui_scale::rect(&b);
                                    adding_at.set((r.right(), r.bottom()));
                                }
                                adding.update(|a| *a = !*a);
                            }>
                            <Icon name="plus" size=12 />
                            "Add lane"
                        </button>
                        {move || adding.get().then(|| view! {
                            <div class="ctx-catcher" on:pointerdown=move |_| adding.set(false)></div>
                            <div class="context-menu seq-add-menu"
                                style:left=move || format!("{}px", (adding_at.get().0 - 300.0).max(4.0))
                                style:top=move || format!("{}px", adding_at.get().1 + 4.0)>
                                <AddLaneSearch scene=open.with_untracked(|o| o.as_ref().map(|(s, _)| s.clone())) pick=add_lane make=make_lane offer=add_offer values=values />
                            </div>
                        })}
                    </div>
                    <Info text="Double-click a lane to add a point there (snapped to a tenth of a second; Shift for free), drag a point to move it, and click or right-click it for its curve, time and value. On a switch lane, double-click a gap to add a bar, drag a bar or one of its ends, and click it for its times. Double-click the Events lane to add a one-off event. A control moved by hand takes its lane over until the sequence plays again." />
                </Show>
            </div>
            <div class="seq-scroll">
                {move || match open.get() {
                    None => view! {
                        <div class="empty-state seq-empty-state">
                            <Icon name="sequence" size=18 />
                            <p class="empty-title">"No sequence in this scene"</p>
                            <p class="empty-note">"A sequence moves channels over time, runs one-off events and can start itself when something happens."</p>
                            <button type="button" class="sm seq-new" on:click=move |_| create()>"+ New sequence"</button>
                        </div>
                    }.into_any(),
                    Some((scene, t)) => view! {
                        <Triggers timeline=t.clone() change=change />
                        <Lanes timeline=t scene=scene change=change values=values position=Signal::derive(position) />
                    }.into_any(),
                }}
            </div>
        </section>
    }
}

/// A sequence's name being renamed in place of the picker: Enter or a click away keeps
/// it, Escape drops it.
#[component]
fn RenameField(
    /// The name now.
    value: String,
    /// Takes the new name, or `None` when dropped.
    done: impl Fn(Option<String>) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            _ = el.focus();
            el.select();
        }
    });
    // Enter and Escape end it before the blur that follows, which then sends nothing.
    let ended = StoredValue::new(false);
    let finish = move |name: Option<String>| {
        if !ended.get_value() {
            ended.set_value(true);
            done(name);
        }
    };
    view! {
        <input class="seq-name" aria-label="Sequence name" node_ref=input prop:value=value
            on:keydown=move |e: leptos::ev::KeyboardEvent| match e.key().as_str() {
                "Enter" => {
                    e.prevent_default();
                    finish(Some(event_target_value(&e)));
                }
                "Escape" => {
                    e.prevent_default();
                    e.stop_propagation();
                    finish(None);
                }
                _ => {}
            }
            on:blur=move |e| finish(Some(event_target_value(&e))) />
    }
}

/// Add lane's search (A13, SQ-01): one box over three groups, "Channels", "Settings
/// (makes a channel)" and "Switches", with "+ New channel" at its foot. The settings are
/// read from the scene's sources' kind descriptors, which it asks for as it opens.
#[component]
fn AddLaneSearch(
    /// The sequence's scene, whose sources it offers.
    scene: Option<SceneId>,
    /// Takes a picked channel's ID.
    pick: impl Fn(String) + Copy + Send + Sync + 'static,
    /// Takes the name typed for a new channel.
    make: impl Fn(String) + Copy + Send + Sync + 'static,
    /// Takes a picked setting.
    offer: impl Fn(Offer) + Copy + Send + Sync + 'static,
    /// The live values, for the level beside each channel.
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let text = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            _ = el.focus();
        }
    });
    let scene = StoredValue::new(scene);
    // The descriptors the Settings and Switches groups need, each read once.
    if let (Some(s), Some(scene)) = (session.0.get_value(), scene.get_value()) {
        let kinds = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| kinds_needed(c, &scene))
                .unwrap_or_default()
        });
        leptos::task::spawn_local(async move {
            for kind in kinds {
                // A kind that can't be described offers nothing; the channels still work.
                _ = studio.descriptor(&s, &kind).await;
            }
        });
    }
    let found = move || -> (Vec<(String, String)>, Vec<Offer>, Vec<Offer>) {
        let q = text.get();
        let Some(scene) = scene.get_value() else {
            return Default::default();
        };
        studio.collection.with(|c| {
            let Some(c) = c else {
                return Default::default();
            };
            let (here, rest) = search(&c.channels, &q, Some(&scene));
            let channels = here
                .into_iter()
                .chain(rest)
                .map(|ch| (ch.channel_id.clone(), c.channel_name(&ch.channel_id)))
                .collect();
            let all = studio.descriptors.with(|d| offers(c, &scene, d));
            let (switches, settings) = all
                .into_iter()
                .filter(|o| lane_offers::matches(o, &q))
                .partition(|o| o.switch);
            (channels, settings, switches)
        })
    };
    let level = move |id: &str| {
        values
            .with(|v| v.get(id).map(|c| c.value.get()))
            .map(|v| format!("{:.0} %", v * 100.0))
            .unwrap_or_default()
    };
    let offer_rows = move |list: Vec<Offer>| {
        list.into_iter()
            .map(|o| {
                let label = o.label.clone();
                let o = StoredValue::new(o);
                view! {
                    <button type="button" class="menuitem chan-pick" on:click=move |_| offer(o.get_value())>
                        <span class="ctx-label">{label}</span>
                    </button>
                }
            })
            .collect_view()
    };
    let groups = move || {
        let (channels, settings, switches) = found();
        let channels_view = (!channels.is_empty()).then(|| {
            view! {
                <div class="ctx-head">"Channels"</div>
                {channels.into_iter().map(|(id, name)| {
                    let shown = id.clone();
                    view! {
                        <button type="button" class="menuitem chan-pick" on:click=move |_| pick(id.clone())>
                            <span class="ctx-label">{name}</span>
                            <span class="hint">{move || level(&shown)}</span>
                        </button>
                    }
                }).collect_view()}
            }
        });
        let settings_view = (!settings.is_empty()).then(|| {
            view! {
                <div class="ctx-head">"Settings (makes a channel)"</div>
                {offer_rows(settings)}
            }
        });
        let switches_view = (!switches.is_empty()).then(|| {
            view! {
                <div class="ctx-head">"Switches"</div>
                {offer_rows(switches)}
            }
        });
        view! { {channels_view} {settings_view} {switches_view} }
    };
    let create = move || {
        let q = text.get_untracked();
        make(if q.trim().is_empty() {
            "Channel".into()
        } else {
            q.trim().to_owned()
        });
    };
    let new_label = move || {
        let q = text.get();
        if q.trim().is_empty() {
            "+ New channel".to_owned()
        } else {
            format!("+ New channel “{}”", q.trim())
        }
    };
    view! {
        <div class="chan-search">
            <div class="tree-search">
                <Icon name="search" size=13 />
                <input
                    node_ref=input
                    type="search"
                    placeholder="Add lane: a channel, a setting or a switch"
                    aria-label="Search channels and settings"
                    prop:value=move || text.get()
                    on:input=move |e| text.set(event_target_value(&e))
                    on:keydown=move |e: leptos::ev::KeyboardEvent| {
                        if e.key() == "Enter" {
                            let (channels, settings, switches) = untrack(found);
                            if let Some((id, _)) = channels.into_iter().next() {
                                pick(id);
                            } else if let Some(o) = settings.into_iter().chain(switches).next() {
                                offer(o);
                            } else {
                                create();
                            }
                        }
                    }
                />
            </div>
            <div class="chan-search-list">{groups}</div>
            <div class="menu-sep"></div>
            <button type="button" class="menuitem chan-new" on:click=move |_| create()>
                {new_label}
            </button>
        </div>
    }
}

/// The trigger bar: what starts the sequence and what stops it.
#[component]
fn Triggers(
    timeline: TimelineInfo,
    change: impl Fn(&dyn Fn(&mut TimelineInfo)) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    // The editor open: whether it adds a stop, and where it opens (below its button).
    let editing = RwSignal::new(None::<(bool, f64, f64)>);
    let open_editor = move |stop: bool, e: leptos::ev::MouseEvent| {
        let at = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            .map(|b| crate::ui_scale::rect(&b));
        let (x, y) = at.map_or_else(|| crate::ui_scale::pointer(&e), |r| (r.left(), r.bottom()));
        editing.set(Some((stop, x, y)));
    };
    let chips = move |stop: bool, list: Vec<TimelineTrigger>| {
        list.into_iter()
            .enumerate()
            .map(|(n, trigger)| {
                let text = studio.collection.with_untracked(|c| {
                    c.as_ref()
                        .map_or_else(String::new, |c| trigger_text(c, &trigger))
                });
                let channel = matches!(trigger, TimelineTrigger::Channel { .. });
                let remove = move |_| {
                    change(&|t| {
                        let list = if stop { &mut t.stops } else { &mut t.starts };
                        if n < list.len() {
                            list.remove(n);
                        }
                    });
                };
                view! {
                    <span class="seq-chip" class:channel=channel>
                        {text}
                        <button type="button" class="chip-x" aria-label="Remove this trigger" on:click=remove>"×"</button>
                    </span>
                }
            })
            .collect_view()
    };
    let starts = chips(false, timeline.starts.clone());
    let stops = chips(true, timeline.stops.clone());
    let own = timeline.timeline_id.clone();
    let driven: Vec<String> = timeline
        .lanes
        .iter()
        .map(|l| l.target.channel.clone())
        .collect();
    view! {
        <div class="seq-triggers">
            <div class="seq-trigger-group">
                <span class="eyebrow">"Starts when"</span>
                {starts}
                <button type="button" class="seq-plus" aria-label="Add a trigger that starts it" on:click=move |e| open_editor(false, e)>"+ Trigger"</button>
            </div>
            <div class="seq-trigger-group">
                <span class="eyebrow">"Stops when"</span>
                {stops}
                <button type="button" class="seq-plus" aria-label="Add a trigger that stops it" on:click=move |e| open_editor(true, e)>"+ Trigger"</button>
            </div>
            {move || editing.get().map(|(stop, x, y)| {
                let own = own.clone();
                let driven = driven.clone();
                let done = move |trigger: Option<TimelineTrigger>| {
                    editing.set(None);
                    if let Some(trigger) = trigger {
                        change(&|t| {
                            let list = if stop { &mut t.stops } else { &mut t.starts };
                            list.push(trigger.clone());
                        });
                    }
                };
                view! {
                    <div class="ctx-catcher" on:pointerdown=move |_| editing.set(None)></div>
                    <TriggerEditor stop=stop own=own driven=driven x=x y=y done=done />
                }
            })}
        </div>
    }
}

/// What a new trigger fires on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    Channel,
    Scene,
    Keyframe,
    Timeline,
}

/// The small editor "+ Trigger" opens: when, the object, the direction or level, and
/// a wait first.
#[component]
fn TriggerEditor(
    /// Whether it stops the sequence.
    stop: bool,
    /// The sequence itself, which a trigger can't name.
    own: TimelineId,
    /// The channels its own lanes drive, which can't start or stop it: the engine
    /// refuses them.
    driven: Vec<String>,
    /// Where it opens, in the window: below this point, kept inside the window.
    x: f64,
    y: f64,
    /// Takes the new trigger, or `None` when cancelled.
    done: impl Fn(Option<TimelineTrigger>) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let when = RwSignal::new(When::Channel);
    let object = RwSignal::new(String::new());
    let edge = RwSignal::new(0_usize);
    let level = RwSignal::new(50.0_f64);
    let wait = RwSignal::new(0.0_f64);
    let own = StoredValue::new(own);
    let driven = StoredValue::new(driven);
    // The objects for each kind: (value, name).
    let objects = move || -> Vec<(String, String)> {
        studio.collection.with(|c| {
            let Some(c) = c else {
                return Vec::new();
            };
            match when.get() {
                When::Channel => c
                    .channels
                    .iter()
                    .filter(|ch| driven.with_value(|d| !d.contains(&ch.channel_id)))
                    .map(|ch| (ch.channel_id.clone(), c.channel_name(&ch.channel_id)))
                    .collect(),
                When::Scene => c
                    .scenes
                    .iter()
                    .filter(|s| s.variant_of.is_none())
                    .map(|s| (s.scene_id.to_string(), s.name.clone()))
                    .collect(),
                When::Keyframe => c
                    .scenes
                    .iter()
                    .filter_map(|s| {
                        let parent = s.variant_of.as_ref()?;
                        let parent_name =
                            c.scene(parent).map_or_else(String::new, |p| p.name.clone());
                        Some((
                            format!("{parent}|{}", s.scene_id),
                            format!("{parent_name} › {}", s.name),
                        ))
                    })
                    .collect(),
                When::Timeline => c
                    .scenes
                    .iter()
                    .flat_map(|s| &s.timelines)
                    .filter(|t| own.with_value(|o| &t.timeline_id != o))
                    .map(|t| (t.timeline_id.to_string(), timeline_name(t)))
                    .collect(),
            }
        })
    };
    let edges = move || -> &'static [&'static str] {
        match when.get() {
            When::Channel => &["rises past", "falls past", "switches on", "switches off"],
            When::Scene => &["goes on program", "leaves program"],
            When::Keyframe => &["loads", "leaves"],
            When::Timeline => &["ends", "starts", "loops"],
        }
    };
    // The first object, once the kind changes.
    Effect::new(move |_| {
        when.track();
        // Untracked, so an edit elsewhere doesn't reset what was picked.
        let first = untrack(objects)
            .into_iter()
            .next()
            .map(|(v, _)| v)
            .unwrap_or_default();
        object.set(first);
        edge.set(0);
    });
    let build = move || -> Option<TimelineTrigger> {
        let o = object.get_untracked();
        if o.is_empty() {
            return None;
        }
        let delay_ms = (wait.get_untracked() * 1000.0).round().clamp(0.0, 60000.0) as u32;
        let e = edge.get_untracked();
        Some(match when.get_untracked() {
            When::Channel => TimelineTrigger::Channel {
                channel_id: o,
                edge: [
                    ChannelEdge::Rises,
                    ChannelEdge::Falls,
                    ChannelEdge::On,
                    ChannelEdge::Off,
                ][e.min(3)],
                level: (e < 2)
                    .then(|| Finite32::new((level.get_untracked() / 100.0) as f32))
                    .flatten()
                    .filter(|l| (l.get() - 0.5).abs() > f32::EPSILON),
                delay_ms,
            },
            When::Scene => TimelineTrigger::Scene {
                scene_id: SceneId::new(o),
                edge: [SceneEdge::Shown, SceneEdge::Hidden][e.min(1)],
                delay_ms,
            },
            When::Keyframe => {
                let (scene, keyframe) = o.split_once('|')?;
                TimelineTrigger::Keyframe {
                    scene_id: SceneId::new(scene),
                    keyframe_id: Some(SceneId::new(keyframe)),
                    edge: [KeyframeEdge::Loads, KeyframeEdge::Leaves][e.min(1)],
                    delay_ms,
                }
            }
            When::Timeline => TimelineTrigger::Timeline {
                timeline_id: TimelineId::new(o),
                edge: [
                    TimelineEdge::Ends,
                    TimelineEdge::Starts,
                    TimelineEdge::Loops,
                ][e.min(2)],
                delay_ms,
            },
        })
    };
    let kinds = [
        (When::Channel, "A channel"),
        (When::Scene, "A scene"),
        (When::Keyframe, "A keyframe"),
        (When::Timeline, "A sequence"),
    ];
    let (left, top) = crate::panels::events::place(x, y, 300.0);
    view! {
        <div class="context-menu seq-trigger-editor" role="dialog" aria-label="New trigger"
            style:left=left style:top=top
            on:keydown=move |e: leptos::ev::KeyboardEvent| if e.key() == "Escape" {
                e.prevent_default();
                e.stop_propagation();
                done(None);
            }>
            <div class="ctx-head">{if stop { "Stop it when" } else { "Start it when" }}</div>
            <div class="segmented full seq-kinds" role="group" aria-label="What it watches">
                {kinds.into_iter().map(|(k, label)| view! {
                    <button type="button" aria-pressed=move || if when.get() == k { "true" } else { "false" } on:click=move |_| when.set(k)>{label}</button>
                }).collect_view()}
            </div>
            <label class="seq-row">
                <span>"Which"</span>
                <select aria-label="Which" on:change=move |e| object.set(event_target_value(&e))>
                    {move || {
                        let current = object.get();
                        let rows = objects();
                        if rows.is_empty() {
                            return view! { <option value="">"None yet"</option> }.into_any();
                        }
                        rows.into_iter().map(|(v, name)| {
                            let on = v == current;
                            view! { <option value=v selected=on>{name}</option> }
                        }).collect_view().into_any()
                    }}
                </select>
            </label>
            <label class="seq-row">
                <span>"When it"</span>
                <select aria-label="When it" on:change=move |e| edge.set(event_target_value(&e).parse().unwrap_or(0))>
                    {move || edges().iter().enumerate().map(|(n, label)| {
                        view! { <option value=n.to_string() selected=move || edge.get() == n>{*label}</option> }
                    }).collect_view()}
                </select>
            </label>
            <Show when=move || when.get() == When::Channel && edge.get() < 2>
                <div class="seq-row">
                    <span>"Level"</span>
                    <NumberField named=Signal::derive(|| "Level".to_owned()) unit="%" value=Signal::derive(move || Some(level.get())) step=1.0 decimals=0 min=0.0 max=100.0
                        send=move |v| { level.set(v); true } />
                </div>
            </Show>
            <div class="seq-row">
                <span>"Wait first"</span>
                <NumberField named=Signal::derive(|| "Wait first".to_owned()) unit="s" value=Signal::derive(move || Some(wait.get())) step=0.1 decimals=1 min=0.0 max=60.0
                    send=move |v| { wait.set(v); true } />
            </div>
            <div class="seq-editor-buttons">
                <button type="button" on:click=move |_| done(None)>"Cancel"</button>
                <button type="button" class="primary" on:click=move |_| {
                    match build() {
                        Some(trigger) => done(Some(trigger)),
                        None => studio.problem.set(Some("Pick what the trigger watches first.".into())),
                    }
                }>"Add trigger"</button>
            </div>
        </div>
    }
}

/// The ruler, the lanes and the play head.
#[component]
fn Lanes(
    timeline: TimelineInfo,
    scene: SceneId,
    change: impl Fn(&dyn Fn(&mut TimelineInfo)) -> bool + Copy + Send + Sync + 'static,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    position: Signal<Option<u32>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let duration = timeline.duration_ms.max(1);
    let id = StoredValue::new(timeline.timeline_id.clone());
    let drag = RwSignal::new(None::<Drag>);
    let menu = RwSignal::new(None::<PointMenu>);
    let bar_drag = RwSignal::new(None::<BarDrag>);
    let bar_menu = RwSignal::new(None::<BarMenu>);
    let live = move |channel: &str| values.with(|v| v.get(channel).map(|c| c.value.get()));
    let ticks = {
        let step = match duration {
            0..=10_000 => 1000,
            10_001..=60_000 => 5000,
            _ => 30_000,
        };
        (0..=duration / step)
            .map(|n| {
                let ms = n * step;
                let left = f64::from(ms) / f64::from(duration) * 100.0;
                view! { <span class="seq-tick" style:left=format!("{left:.3}%")>{seconds(ms)}</span> }
            })
            .collect_view()
    };
    let seek = move |e: leptos::ev::MouseEvent| {
        let Some(el) = e
            .current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let f = ((f64::from(e.client_x()) - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        let at_ms = snap(f * f64::from(duration), e.shift_key()).min(duration.saturating_sub(1));
        if let Some(s) = session.0.get_value() {
            studio.act(
                &s,
                Action::TimelineSeek {
                    timeline_id: id.get_value(),
                    at_ms,
                },
            );
        }
    };
    // A loop range, shaded on the ruler.
    let band = match timeline.looping {
        TimelineLoop::Range { from_ms, to_ms } => {
            let left = f64::from(from_ms) / f64::from(duration) * 100.0;
            let width = f64::from(to_ms.saturating_sub(from_ms)) / f64::from(duration) * 100.0;
            Some(view! {
                <span class="seq-range" style:left=format!("{left:.3}%") style:width=format!("{width:.3}%")
                    title=format!("Repeats {} to {}", seconds(from_ms), seconds(to_ms))></span>
            })
        }
        _ => None,
    };
    let head = move || {
        position.get().map(|ms| {
            let left = f64::from(ms) / f64::from(duration) * 100.0;
            view! { <div class="seq-head" style:left=format!("{left:.3}%")></div> }
        })
    };
    // Where in a lane's area a pointer is: (ms, value).
    let at = move |e: &leptos::ev::MouseEvent, el: &web_sys::Element| {
        let rect = el.get_bounding_client_rect();
        let fx = ((f64::from(e.client_x()) - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        let fy = ((f64::from(e.client_y()) - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        (
            snap(fx * f64::from(duration), e.shift_key()).min(duration),
            (1.0 - fy) as f32,
        )
    };
    let lanes = timeline
        .lanes
        .iter()
        .cloned()
        .enumerate()
        .map(|(n, lane)| {
            let colour = format!("seq-lane lane-{}", n % COLOURS);
            let channel = lane.target.channel.clone();
            let name = studio.collection.with_untracked(|c| {
                c.as_ref().map_or_else(|| channel.clone(), |c| c.channel_name(&channel))
            });
            // A switch lane (A13): bars where it is on, and points that sit at 0 or 1.
            let switch = is_switch(&lane);
            let lane = StoredValue::new(lane);
            let ch = StoredValue::new(channel);
            // The points as shown: the drag's point where it is now.
            let shown = move || {
                let mut l = lane.get_value();
                if let Some(d) = drag.get().filter(|d| d.lane == n) {
                    move_point(&mut l.points, d.point, d.at_ms, d.value, duration);
                }
                l
            };
            let paths = move || {
                let from = ch.with_value(|c| live(c)).unwrap_or(0.0);
                lane_path(&shown(), duration, from)
            };
            // A switch lane's bars: where it is on, as drawn from its points (off before
            // the first, whatever the channel was), with the dragged one where it is now.
            let held_bars = move || lane.with_value(|l| switch_bars(&l.points, duration, false));
            let set_bars = move |bars: Vec<(u32, u32)>| {
                change(&|t| {
                    let duration = t.duration_ms;
                    if let Some(l) = t.lanes.get_mut(n) {
                        l.points = bar_points(&bars, duration);
                    }
                })
            };
            let grab = move |grip: Grip, b: usize, e: leptos::ev::PointerEvent| {
                if e.button() != 0 {
                    return;
                }
                e.stop_propagation();
                e.prevent_default();
                let Some(el) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                    return;
                };
                el.set_pointer_capture(e.pointer_id()).ok();
                let Some(area) = el.closest(".seq-area").ok().flatten() else {
                    return;
                };
                let (ms, _) = at(&e, &area);
                let held = held_bars();
                let Some(&now) = held.get(b) else {
                    return;
                };
                bar_drag.set(Some(BarDrag {
                    lane: n,
                    bar: b,
                    grip,
                    held,
                    now,
                    offset_ms: i64::from(ms) - i64::from(now.0),
                    x0: f64::from(e.client_x()),
                    moved: false,
                }));
            };
            // Drawn once per edit; a drag moves its bar's style, so the bar it holds
            // stays put and keeps the pointer.
            let bars = move || {
                held_bars()
                    .into_iter()
                    .enumerate()
                    .map(|(b, (from, to))| {
                        let span = move || {
                            bar_drag.with(|d| {
                                d.as_ref().filter(|d| d.lane == n && d.bar == b).map_or((from, to), |d| d.now)
                            })
                        };
                        let left = move || format!("{:.3}%", f64::from(span().0) / f64::from(duration) * 100.0);
                        let width = move || {
                            let (a, z) = span();
                            format!("{:.3}%", f64::from(z - a) / f64::from(duration) * 100.0)
                        };
                        let label = format!("On from {} to {}", seconds(from), seconds(to));
                        // From the keyboard: Enter, Space, the Menu key or Shift+F10
                        // open the bar's menu beside it, and Delete removes it.
                        let key = move |e: leptos::ev::KeyboardEvent| {
                            let k = e.key();
                            if k == "Enter" || k == " " || crate::menu::asks_for_menu(&e) {
                                e.prevent_default();
                                e.stop_propagation();
                                let Some(el) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                                    return;
                                };
                                let r = crate::ui_scale::rect(&el);
                                bar_menu.set(Some(BarMenu { lane: n, bar: b, x: r.left(), y: r.bottom() }));
                            } else if k == "Delete" || k == "Backspace" {
                                e.prevent_default();
                                e.stop_propagation();
                                let mut bars = held_bars();
                                if b < bars.len() {
                                    bars.remove(b);
                                    set_bars(bars);
                                }
                            }
                        };
                        let context = move |e: leptos::ev::MouseEvent| {
                            e.prevent_default();
                            e.stop_propagation();
                            let (x, y) = crate::ui_scale::pointer(&e);
                            bar_menu.set(Some(BarMenu { lane: n, bar: b, x, y }));
                        };
                        let title = format!("{label} · drag to move, drag an end to change it, click for more");
                        view! {
                            <div
                                class="seq-switch-bar"
                                class:dragging=move || bar_drag.with(|d| d.as_ref().is_some_and(|d| d.lane == n && d.bar == b && d.moved))
                                role="button"
                                tabindex="0"
                                aria-haspopup="menu"
                                aria-label=label
                                title=title
                                style:left=left style:width=width
                                on:pointerdown=move |e| grab(Grip::Body, b, e)
                                on:keydown=key
                                on:contextmenu=context
                                on:dblclick=|e: leptos::ev::MouseEvent| e.stop_propagation()
                            >
                                <span class="seq-bar-end start" title="Drag to change where it switches on"
                                    on:pointerdown=move |e| grab(Grip::Start, b, e)></span>
                                <span class="seq-bar-end end" title="Drag to change where it switches off"
                                    on:pointerdown=move |e| grab(Grip::End, b, e)></span>
                            </div>
                        }
                    })
                    .collect_view()
            };
            // Drawn once per edit; a drag moves its dot's style, so the dot it holds
            // stays put and keeps the pointer.
            let dots = move || {
                lane.get_value()
                    .points
                    .iter()
                    .enumerate()
                    .map(|(p, point)| {
                        let place = move || {
                            shown().points.get(p).map_or((0.0, 0.0), |q| {
                                (f64::from(q.at_ms) / f64::from(duration) * 100.0, (1.0 - f64::from(q.value.get())) * 100.0)
                            })
                        };
                        let left = move || format!("{:.3}%", place().0);
                        let top = move || format!("{:.3}%", place().1);
                        let title = format!(
                            "{} · {:.0} % · {}",
                            seconds(point.at_ms),
                            point.value.get() * 100.0,
                            curve_name(point.curve)
                        );
                        let down = move |e: leptos::ev::PointerEvent| {
                            if e.button() != 0 {
                                return;
                            }
                            e.stop_propagation();
                            e.prevent_default();
                            // Keep the pointer while dragging, so overshooting the lane's
                            // edge still moves the point instead of ending the drag.
                            if let Some(dot) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) {
                                dot.set_pointer_capture(e.pointer_id()).ok();
                            }
                            let l = lane.get_value();
                            let Some(pt) = l.points.get(p) else {
                                return;
                            };
                            drag.set(Some(Drag {
                                lane: n,
                                point: p,
                                at_ms: pt.at_ms,
                                value: pt.value.get(),
                                x0: f64::from(e.client_x()),
                                y0: f64::from(e.client_y()),
                                moved: false,
                            }));
                        };
                        let context = move |e: leptos::ev::MouseEvent| {
                            e.prevent_default();
                            e.stop_propagation();
                            menu.set(Some(PointMenu {
                                lane: n,
                                point: p,
                                x: crate::ui_scale::pointer(&e).0,
                                y: crate::ui_scale::pointer(&e).1,
                            }));
                        };
                        // From the keyboard: Enter, Space, the Menu key or Shift+F10
                        // open the point's menu beside it, and Delete removes it.
                        let key = move |e: leptos::ev::KeyboardEvent| {
                            let k = e.key();
                            if k == "Enter" || k == " " || crate::menu::asks_for_menu(&e) {
                                e.prevent_default();
                                e.stop_propagation();
                                let Some(dot) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                                    return;
                                };
                                let r = crate::ui_scale::rect(&dot);
                                menu.set(Some(PointMenu {
                                    lane: n,
                                    point: p,
                                    x: r.right(),
                                    y: r.bottom(),
                                }));
                            } else if k == "Delete" || k == "Backspace" {
                                e.prevent_default();
                                e.stop_propagation();
                                change(&|t| {
                                    if let Some(l) = t.lanes.get_mut(n)
                                        && p < l.points.len()
                                    {
                                        l.points.remove(p);
                                    }
                                });
                            }
                        };
                        let label = title.clone();
                        view! {
                            <span
                                class="seq-dot"
                                tabindex="0"
                                role="button"
                                aria-haspopup="menu"
                                aria-label=label
                                on:keydown=key
                                class:dragging=move || drag.get().is_some_and(|d| d.lane == n && d.point == p)
                                style:left=left style:top=top
                                title=title
                                on:pointerdown=down
                                on:contextmenu=context
                                on:dblclick=|e: leptos::ev::MouseEvent| e.stop_propagation()
                            ></span>
                        }
                    })
                    .collect_view()
            };
            let add = move |e: leptos::ev::MouseEvent| {
                let Some(el) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                    return;
                };
                let (ms, v) = at(&e, &el);
                if switch {
                    // A one-second bar from here, cut short before the next one.
                    match insert_bar(&held_bars(), ms, 1000, duration) {
                        Some((bars, _)) => {
                            set_bars(bars);
                        }
                        None => studio.problem.set(Some(
                            "There's no room for a bar there: it needs a tenth of a second before the next one.".into(),
                        )),
                    }
                    return;
                }
                change(&|t| {
                    if let Some(l) = t.lanes.get_mut(n) {
                        add_point(&mut l.points, ms, v);
                    }
                });
            };
            let moving = move |e: leptos::ev::PointerEvent| {
                let Some(el) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                    return;
                };
                let (ms, v) = at(&e, &el);
                if let Some(d) = bar_drag.get_untracked().filter(|d| d.lane == n) {
                    // A few pixels are still a click.
                    if !d.moved && (f64::from(e.client_x()) - d.x0).abs() <= CLICK_SLOP {
                        return;
                    }
                    let to = match d.grip {
                        Grip::Body => (i64::from(ms) - d.offset_ms).clamp(0, i64::from(duration)) as u32,
                        _ => ms,
                    };
                    let now = drag_bar(&d.held, d.bar, d.grip, to, duration);
                    bar_drag.set(Some(BarDrag { now, moved: true, ..d }));
                    return;
                }
                let Some(d) = drag.get_untracked().filter(|d| d.lane == n) else {
                    return;
                };
                if !d.moved
                    && (f64::from(e.client_x()) - d.x0).hypot(f64::from(e.client_y()) - d.y0) <= CLICK_SLOP
                {
                    return;
                }
                drag.set(Some(Drag { at_ms: ms, value: v, moved: true, ..d }));
            };
            // Letting go ends a drag as one edit; a press that didn't move is a click,
            // which opens the point's or bar's menu where it was clicked.
            let up = move |e: leptos::ev::PointerEvent| {
                let (x, y) = crate::ui_scale::pointer(&e);
                if let Some(d) = bar_drag.get_untracked().filter(|d| d.lane == n) {
                    bar_drag.set(None);
                    if !d.moved {
                        bar_menu.set(Some(BarMenu { lane: n, bar: d.bar, x, y }));
                    } else if d.held.get(d.bar) != Some(&d.now) {
                        let mut bars = d.held;
                        bars[d.bar] = d.now;
                        set_bars(bars);
                    }
                    return;
                }
                let Some(d) = drag.get_untracked().filter(|d| d.lane == n) else {
                    return;
                };
                drag.set(None);
                if !d.moved {
                    menu.set(Some(PointMenu { lane: n, point: d.point, x, y }));
                    return;
                }
                let moved = lane
                    .with_value(|l| l.points.get(d.point).is_some_and(|p| p.at_ms != d.at_ms || (p.value.get() - d.value).abs() > 1e-4));
                if moved {
                    change(&|t| {
                        if let Some(l) = t.lanes.get_mut(n) {
                            move_point(&mut l.points, d.point, d.at_ms, d.value, t.duration_ms);
                        }
                    });
                }
            };
            let cancel = move |_| {
                drag.set(None);
                bar_drag.set(None);
            };
            let on = lane.with_value(|l| l.on);
            let mute = move |_| {
                change(&|t| {
                    if let Some(l) = t.lanes.get_mut(n) {
                        l.on = !l.on;
                    }
                });
            };
            let remove = move |_| {
                change(&|t| {
                    if n < t.lanes.len() {
                        t.lanes.remove(n);
                    }
                });
            };
            let level = move || {
                ch.with_value(|c| live(c))
                    .map_or_else(|| "–".to_owned(), |v| format!("{:.0} %", v * 100.0))
            };
            let meter = move || {
                let v = ch.with_value(|c| live(c)).unwrap_or(0.0);
                format!("{:.1}%", v * 100.0)
            };
            let open = move |_| open_channel(studio, ch.get_value());
            view! {
                <div class=colour class:off=!on>
                    <div class="seq-lane-head">
                        <div class="seq-lane-title">
                            <span class="seq-swatch"></span>
                            <button type="button" class="seq-lane-name" on:click=open title="Open the channel in the Channels panel">
                                {name}
                            </button>
                            <button type="button" class="seq-mute" class:on=!on on:click=mute
                                aria-pressed=if on { "false" } else { "true" }
                                title=if on { "Mute this lane: it stops moving its channel" } else { "Unmute this lane" }>"M"</button>
                            <button type="button" class="seq-lane-x" aria-label="Remove this lane" title="Remove this lane" on:click=remove>
                                <Icon name="close" size=12 />
                            </button>
                        </div>
                        <div class="seq-lane-meta">
                            <span class="hint">{if switch { "Switch" } else { "Channel" }}</span>
                            <span class="seq-meter"><span style:width=meter></span></span>
                            <span class="seq-level">{level}</span>
                        </div>
                    </div>
                    <div
                        class="seq-area"
                        on:dblclick=add
                        on:pointermove=moving
                        on:pointerup=up
                        on:pointercancel=cancel
                    >
                        <svg viewBox="0 0 1000 100" preserveAspectRatio="none" class="seq-svg" aria-hidden="true">
                            {move || if switch {
                                ().into_any()
                            } else {
                                let (lead, path) = paths();
                                view! {
                                    <path d=lead.clone() class="seq-lead" />
                                    <path d=format!("{path} L1000 100 L{} 100 Z", path_start(&path)) class="seq-fill" />
                                    <path d=path class="seq-line" />
                                }
                                .into_any()
                            }}
                        </svg>
                        {move || if switch { bars().into_any() } else { dots().into_any() }}
                    </div>
                </div>
            }
        })
        .collect_view();
    let close_menu = move || menu.set(None);
    let point_keys = crate::menu::keys(close_menu);
    let snapshot = StoredValue::new(timeline.clone());
    let point_menu = move || {
        let m = menu.get()?;
        let point = snapshot.with_value(|t| {
            t.lanes
                .get(m.lane)
                .and_then(|l| l.points.get(m.point))
                .map(|p| (p.at_ms, p.value.get()))
        })?;
        let (point_ms, point_value) = point;
        // A typed time (s) or value (%), kept between the point's neighbours (A11).
        let set_point = move |at_ms: Option<u32>, value: Option<f32>| {
            menu.set(None);
            change(&|t| {
                let duration = t.duration_ms;
                if let Some(l) = t.lanes.get_mut(m.lane)
                    && let Some((was_ms, was)) =
                        l.points.get(m.point).map(|p| (p.at_ms, p.value.get()))
                {
                    let at = at_ms.unwrap_or(was_ms);
                    let v = value.unwrap_or(was);
                    move_point(&mut l.points, m.point, at, v, duration);
                }
            })
        };
        // The fields take arrow keys, Home and End, which the menu's keys would take.
        let field_keys = |e: leptos::ev::KeyboardEvent| {
            if e.key() != "Escape" {
                e.stop_propagation();
            }
        };
        let node = NodeRef::<leptos::html::Div>::new();
        crate::menu::focus_first(node);
        // Beside the click, kept in the window: a point near the bottom or the right
        // edge opens it above or to the left.
        let (left, top) = crate::panels::events::place(m.x, m.y, 300.0);
        let set_curve = move |curve: LaneCurve| {
            menu.set(None);
            change(&|t| {
                if let Some(p) = t
                    .lanes
                    .get_mut(m.lane)
                    .and_then(|l| l.points.get_mut(m.point))
                {
                    p.curve = curve;
                }
            });
        };
        let remove = move |_| {
            menu.set(None);
            change(&|t| {
                if let Some(l) = t.lanes.get_mut(m.lane)
                    && m.point < l.points.len()
                {
                    l.points.remove(m.point);
                }
            });
        };
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| menu.set(None)></div>
            <div class="context-menu" role="menu" aria-label="Point" node_ref=node on:keydown=point_keys
                style:left=left style:top=top>
                <div class="seq-point-fields" on:keydown=field_keys>
                    <span class="seq-label">"Time"</span>
                    <NumberField named=Signal::derive(|| "Point time".to_owned()) unit="s"
                        value=Signal::derive(move || Some(f64::from(point_ms) / 1000.0))
                        step=0.1 decimals=2 min=0.0 max=3600.0
                        send=move |v| set_point(Some(snap(v * 1000.0, true)), None) />
                    <span class="seq-label">"Value"</span>
                    <NumberField named=Signal::derive(|| "Point value".to_owned()) unit="%"
                        value=Signal::derive(move || Some(f64::from(point_value) * 100.0))
                        step=1.0 decimals=1 min=0.0 max=100.0
                        send=move |v| set_point(None, Some((v / 100.0) as f32)) />
                </div>
                <div class="menu-sep"></div>
                <div class="ctx-head">"Curve into this point"</div>
                {[LaneCurve::Linear, LaneCurve::EaseIn, LaneCurve::EaseOut, LaneCurve::EaseInOut, LaneCurve::Hold]
                    .into_iter()
                    .map(|c| view! {
                        <button type="button" class="menuitem" role="menuitem" on:click=move |_| set_curve(c)>
                            <span class="ctx-check"></span>
                            <span class="ctx-label">{curve_name(c)}</span>
                        </button>
                    })
                    .collect_view()}
                <div class="menu-sep"></div>
                <button type="button" class="menuitem danger" role="menuitem" on:click=remove>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Delete point"</span>
                </button>
            </div>
        })
    };
    // A switch lane's bar: where it switches on and off, typed, and Delete. No curves:
    // a switch only jumps.
    let close_bar_menu = move || bar_menu.set(None);
    let bar_keys = crate::menu::keys(close_bar_menu);
    let bar_menu_view = move || {
        let m = bar_menu.get()?;
        let (from, to) = snapshot.with_value(|t| {
            t.lanes
                .get(m.lane)
                .map(|l| switch_bars(&l.points, t.duration_ms.max(1), false))
                .and_then(|b| b.get(m.bar).copied())
        })?;
        // Edits the bar as it is in the sequence now, kept between its neighbours.
        let edit = move |f: &dyn Fn(&mut Bars, u32)| {
            bar_menu.set(None);
            change(&|t| {
                let duration = t.duration_ms.max(1);
                if let Some(l) = t.lanes.get_mut(m.lane) {
                    let mut bars = switch_bars(&l.points, duration, false);
                    if m.bar < bars.len() {
                        f(&mut bars, duration);
                        l.points = bar_points(&bars, duration);
                    }
                }
            })
        };
        let set_end = move |grip: Grip, ms: u32| {
            edit(&|bars, duration| bars[m.bar] = drag_bar(bars, m.bar, grip, ms, duration))
        };
        let field_keys = |e: leptos::ev::KeyboardEvent| {
            if e.key() != "Escape" {
                e.stop_propagation();
            }
        };
        let node = NodeRef::<leptos::html::Div>::new();
        crate::menu::focus_first(node);
        let (left, top) = crate::panels::events::place(m.x, m.y, 140.0);
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| bar_menu.set(None)></div>
            <div class="context-menu" role="menu" aria-label="Bar" node_ref=node on:keydown=bar_keys
                style:left=left style:top=top>
                <div class="seq-point-fields" on:keydown=field_keys>
                    <span class="seq-label">"On at"</span>
                    <NumberField named=Signal::derive(|| "Switches on at".to_owned()) unit="s"
                        value=Signal::derive(move || Some(f64::from(from) / 1000.0))
                        step=0.1 decimals=2 min=0.0 max=3600.0
                        send=move |v| set_end(Grip::Start, snap(v * 1000.0, true)) />
                    <span class="seq-label">"Off at"</span>
                    <NumberField named=Signal::derive(|| "Switches off at".to_owned()) unit="s"
                        value=Signal::derive(move || Some(f64::from(to) / 1000.0))
                        step=0.1 decimals=2 min=0.0 max=3600.0
                        send=move |v| set_end(Grip::End, snap(v * 1000.0, true)) />
                </div>
                <div class="menu-sep"></div>
                <button type="button" class="menuitem danger" role="menuitem" on:click=move |_| {
                    edit(&|bars, _| {
                        bars.remove(m.bar);
                    });
                }>
                    <span class="ctx-check"></span>
                    <span class="ctx-label">"Delete bar"</span>
                </button>
            </div>
        })
    };
    let empty = timeline.lanes.is_empty();
    view! {
        <div class="seq-lanes">
            <div class="seq-ruler-row">
                <div class="seq-lane-head seq-ruler-head"></div>
                <div class="seq-ruler" on:click=seek title="Click to play from here">{band}{ticks}</div>
            </div>
            <div class="seq-body">
                <crate::panels::events::Events timeline=timeline.clone() scene=scene change=change />
                {lanes}
                {empty.then(|| view! {
                    <p class="hint seq-empty">"No lanes yet. Add lane picks a channel or a setting; a lane starts from where its channel is."</p>
                })}
                <div class="seq-head-layer">{head}</div>
            </div>
            {point_menu}
            {bar_menu_view}
        </div>
    }
}

/// Where a path from [`lane_path`] starts, as its x.
fn path_start(path: &str) -> String {
    path.trim_start_matches('M')
        .split_whitespace()
        .next()
        .unwrap_or("0")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::ChannelInfo;

    fn point(at_ms: u32, value: f32) -> LanePoint {
        LanePoint {
            at_ms,
            value: Finite32::new(value).expect("finite"),
            curve: LaneCurve::Linear,
        }
    }

    #[test]
    fn points_stay_in_time_order() {
        let mut points = vec![point(1000, 0.5)];
        assert_eq!(add_point(&mut points, 500, 0.2), 0);
        assert_eq!(add_point(&mut points, 2000, 1.4), 2);
        assert_eq!(points[2].value.get(), 1.0);
        assert_eq!(add_point(&mut points, 1000, 0.9), 1);
        assert_eq!(points.len(), 3);
        move_point(&mut points, 1, 5000, 0.3, 6000);
        assert_eq!(points[1].at_ms, 1999, "kept before the next point");
        move_point(&mut points, 0, 0, -1.0, 6000);
        assert_eq!((points[0].at_ms, points[0].value.get()), (0, 0.0));
        assert_eq!(snap(1234.0, false), 1200);
        assert_eq!(snap(1234.4, true), 1234);
    }

    #[test]
    fn a_menu_line_adds_or_removes_its_trigger() {
        let mut t = empty_sequence(TimelineId::new("seq_1"), "S".into());
        let shown = TimelineTrigger::Scene {
            scene_id: SceneId::new("intro"),
            edge: SceneEdge::Shown,
            delay_ms: 0,
        };
        toggle_start(&mut t, &shown);
        assert_eq!(t.starts, vec![shown.clone()]);
        // One the operator gave a wait is still the same watch.
        t.starts[0] = TimelineTrigger::Scene {
            scene_id: SceneId::new("intro"),
            edge: SceneEdge::Shown,
            delay_ms: 500,
        };
        let hidden = TimelineTrigger::Scene {
            scene_id: SceneId::new("intro"),
            edge: SceneEdge::Hidden,
            delay_ms: 0,
        };
        assert!(same_watch(&t.starts[0], &shown));
        assert!(!same_watch(&t.starts[0], &hidden));
        toggle_start(&mut t, &shown);
        assert!(t.starts.is_empty(), "a second click takes it out");
    }

    #[test]
    fn the_loop_button_steps_through_a_range() {
        let looped = next_loop(TimelineLoop::Once, 6000);
        assert_eq!(looped, TimelineLoop::Loop);
        let range = next_loop(looped, 6000);
        assert_eq!(
            range,
            TimelineLoop::Range {
                from_ms: 3000,
                to_ms: 6000
            },
            "an intro, then the second half again and again"
        );
        assert_eq!(next_loop(range, 6000), TimelineLoop::Once);
        assert_eq!(
            fit_range(5950, 9000, 6000),
            Some(TimelineLoop::Range {
                from_ms: 5900,
                to_ms: 6000
            }),
            "kept to the length, a minimum loop apart"
        );
        assert_eq!(
            fit_range(0, 0, 6000),
            Some(TimelineLoop::Range {
                from_ms: 0,
                to_ms: 100
            })
        );
        assert_eq!(fit_range(0, 50, 50), None, "too short for a loop");
    }

    #[test]
    fn a_lane_after_0_starts_from_the_channel_dashed() {
        let lane = TimelineLane {
            target: LaneTarget {
                channel: "c".into(),
            },
            points: vec![point(500, 0.0)],
            on: true,
        };
        let (lead, line) = lane_path(&lane, 1000, 1.0);
        assert!(lead.starts_with("M0 0.00"), "{lead}");
        assert!(lead.ends_with("L500.00 100.00"), "{lead}");
        assert_eq!(line, "M500.00 100.00 L1000 100.00");
        assert_eq!(path_start(&line), "500.00");
    }

    #[test]
    fn a_trigger_reads_as_a_sentence() {
        let mut collection = Collection::default();
        let mut power = ChannelInfo::new("power");
        power.name = "Stage power".into();
        collection.channels.push(power);
        let rises = TimelineTrigger::Channel {
            channel_id: "power".into(),
            edge: ChannelEdge::Rises,
            level: None,
            delay_ms: 0,
        };
        assert_eq!(trigger_text(&collection, &rises), "Stage power rises");
        let later = TimelineTrigger::Channel {
            channel_id: "power".into(),
            edge: ChannelEdge::Falls,
            level: Finite32::new(0.2),
            delay_ms: 500,
        };
        assert_eq!(
            trigger_text(&collection, &later),
            "Stage power falls past 20 % · after 0.5 s"
        );
        let (id, name) = fresh_sequence(&collection);
        assert_eq!((id.as_str(), name.as_str()), ("seq_1", "Sequence 1"));
    }
}
