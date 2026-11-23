//! The Sequence panel (ADR-0184, UI-FEATURES SQ-01): the open sequence of the selected
//! scene, a timeline with lanes. Its top bar picks the sequence (or makes one), plays or
//! stops it, loops it, sets its length and adds a lane by searching the channels. Its
//! trigger bar lists what starts and stops it as sentences, with "+ Trigger" to add one.
//!
//! Each lane is one channel over the timeline's length: a double-click adds a point
//! (snapped to a tenth of a second, Shift for free), a drag moves one, and a right-click
//! sets the curve into it or deletes it. Each gesture is one `timeline.set`, so one undo
//! step. A lane with no point at 0 starts from the channel's value, drawn dashed from
//! where the channel is now. The play head follows `timeline.changed`, and a click on
//! the ruler seeks there.
//!
//! Docs: docs/adr/0184-sequences-lanes-and-triggers.md, docs/UI-FEATURES.md#automation

use crate::app::SessionContext;
use crate::icons::Icon;
use crate::panels::channels::{ChannelSearch, live_values, new_channel, open_channel};
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio, now_ms};
use jstream_control::protocol::{
    Action, ChannelEdge, ChannelValueInfo, Edit, Finite32, KeyframeEdge, LaneCurve, LanePoint,
    LaneTarget, SceneEdge, SceneId, TimelineEdge, TimelineId, TimelineInfo, TimelineLane,
    TimelineLoop, TimelineTrigger,
};
use leptos::prelude::*;
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

/// A point being dragged: its lane and index, and where it is now.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    lane: usize,
    point: usize,
    at_ms: u32,
    value: f32,
}

/// The menu a right-click on a point opens, at a point in the window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PointMenu {
    lane: usize,
    point: usize,
    x: f64,
    y: f64,
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
    // The open sequence: the one picked, or this scene's first.
    let open = Memo::new(move |_| {
        let picked = studio.sequence.get();
        let found = picked.and_then(|id| {
            studio.collection.with(|c| {
                c.as_ref().and_then(|c| {
                    c.timeline(&id)
                        .map(|(s, t)| (s.scene_id.clone(), t.clone()))
                })
            })
        });
        found.or_else(|| {
            let scene = family_id()?;
            list().into_iter().next().map(|t| (scene, t))
        })
    });
    let apply = move |scene_id: SceneId, timeline: TimelineInfo| {
        if let Some(s) = session.0.get_value() {
            studio.apply(&s, vec![Edit::TimelineSet { scene_id, timeline }]);
        }
    };
    let change = move |f: &dyn Fn(&mut TimelineInfo)| {
        let Some((scene, mut t)) = open.get_untracked() else {
            return false;
        };
        f(&mut t);
        apply(scene, t);
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
        apply(scene, empty_sequence(id, name));
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
    let looped = move || {
        open.with(|o| {
            o.as_ref()
                .is_some_and(|(_, t)| !matches!(t.looping, TimelineLoop::Once))
        })
    };
    let toggle_loop = move |_| {
        change(&|t| {
            t.looping = if matches!(t.looping, TimelineLoop::Once) {
                TimelineLoop::Loop
            } else {
                TimelineLoop::Once
            };
        });
    };
    let length = Signal::derive(move || {
        open.with(|o| o.as_ref().map(|(_, t)| f64::from(t.duration_ms) / 1000.0))
    });
    let set_length = move |v: f64| {
        let ms = (v * 1000.0).round() as u32;
        change(&|t| {
            t.duration_ms = ms.max(100);
            for lane in &mut t.lanes {
                lane.points.retain(|p| p.at_ms <= t.duration_ms);
            }
            t.cues.retain(|c| c.at_ms <= t.duration_ms);
        })
    };
    let rename = move |e: leptos::ev::Event| {
        let name = event_target_value(&e);
        change(&|t| t.name = name.trim().to_owned());
    };
    let delete = move |_| {
        let Some((_, t)) = open.get_untracked() else {
            return;
        };
        if let Some(s) = session.0.get_value() {
            studio.sequence.set(None);
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
        let Some(s) = session.0.get_value() else {
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
        studio.apply(
            &s,
            vec![
                Edit::ChannelSet { channel },
                Edit::TimelineSet {
                    scene_id: scene,
                    timeline: t,
                },
            ],
        );
    };

    let none = move || open.with(Option::is_none);
    view! {
        <section class="panel seq-panel">
            <div class="seq-bar">
                <Icon name="sequence" size=14 />
                <select class="seq-select" aria-label="Sequence" on:change=pick>
                    {move || {
                        let current = open.with(|o| o.as_ref().map(|(_, t)| t.timeline_id.clone()));
                        let mut rows: Vec<_> = list()
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
                <Show when=move || !none()>
                    <input class="seq-name" aria-label="Sequence name"
                        prop:value=move || open.with(|o| o.as_ref().map(|(_, t)| t.name.clone()).unwrap_or_default())
                        on:change=rename />
                    <button type="button" class="seq-play" class:on=move || playing().is_some() on:click=play
                        title="Play from the start, or stop">
                        {move || if playing().is_some() { "■ Stop" } else { "▶ Play" }}
                    </button>
                    <button type="button" class="seq-loop" class:on=looped on:click=toggle_loop title="Loop">
                        {move || if looped() { "⟳ Loop on" } else { "⟳ Loop off" }}
                    </button>
                    <span class="seq-label">"Length"</span>
                    <NumberField named=Signal::derive(|| "Length".to_owned()) unit="s" value=length step=0.5 decimals=1 min=0.1 max=3600.0 send=set_length />
                    <span class="seq-spacer"></span>
                    <div class="seq-add">
                        <button type="button" class="seq-add-button" on:click=move |_| adding.update(|a| *a = !*a)>
                            <Icon name="search" size=13 />
                            "Add lane: a channel"
                        </button>
                        {move || adding.get().then(|| view! {
                            <div class="ctx-catcher" on:pointerdown=move |_| adding.set(false)></div>
                            <div class="context-menu seq-add-menu">
                                <ChannelSearch placeholder="Add lane: search channels" pick=add_lane make=make_lane values=values />
                            </div>
                        })}
                    </div>
                </Show>
            </div>
            {move || match open.get() {
                None => view! {
                    <p class="placeholder seq-empty">
                        "A sequence moves channels over time and starts itself when something happens. "
                        <button type="button" class="seq-new" on:click=move |_| create()>"+ New sequence"</button>
                    </p>
                }.into_any(),
                Some((scene, t)) => view! {
                    <Triggers timeline=t.clone() change=change />
                    <Lanes timeline=t scene=scene change=change values=values position=Signal::derive(position) />
                    <div class="seq-foot">
                        <span class="hint">"Double-click a lane to add a point · drag to move (Shift: free) · right-click for its curve · a control moved by hand takes its lane over"</span>
                        <button type="button" class="seq-delete" on:click=delete>"Delete sequence"</button>
                    </div>
                }.into_any(),
            }}
        </section>
    }
}

/// The trigger bar: what starts the sequence and what stops it.
#[component]
fn Triggers(
    timeline: TimelineInfo,
    change: impl Fn(&dyn Fn(&mut TimelineInfo)) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let editing = RwSignal::new(None::<bool>);
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
    view! {
        <div class="seq-triggers">
            <span class="eyebrow">"Starts when"</span>
            {starts}
            <button type="button" class="seq-plus" on:click=move |_| editing.set(Some(false))>"+ Trigger"</button>
            <span class="eyebrow seq-stops">"Stops when"</span>
            {stops}
            <button type="button" class="seq-plus" aria-label="Add a trigger that stops it" on:click=move |_| editing.set(Some(true))>"+"</button>
            {move || editing.get().map(|stop| {
                let own = own.clone();
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
                    <TriggerEditor stop=stop own=own done=done />
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
        (When::Timeline, "Another sequence"),
    ];
    view! {
        <div class="context-menu seq-trigger-editor" role="dialog" aria-label="New trigger">
            <div class="ctx-head">{if stop { "Stop it when" } else { "Play it when" }}</div>
            <div class="seq-kinds">
                {kinds.into_iter().map(|(k, label)| view! {
                    <button type="button" class="seq-kind" class:on=move || when.get() == k on:click=move |_| when.set(k)>{label}</button>
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
                }>{if stop { "Stop it" } else { "Play it" }}</button>
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
            let dots = move || {
                shown()
                    .points
                    .iter()
                    .enumerate()
                    .map(|(p, point)| {
                        let left = f64::from(point.at_ms) / f64::from(duration) * 100.0;
                        let top = (1.0 - f64::from(point.value.get())) * 100.0;
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
                            }));
                        };
                        let context = move |e: leptos::ev::MouseEvent| {
                            e.prevent_default();
                            e.stop_propagation();
                            menu.set(Some(PointMenu {
                                lane: n,
                                point: p,
                                x: f64::from(e.client_x()),
                                y: f64::from(e.client_y()),
                            }));
                        };
                        view! {
                            <span
                                class="seq-dot"
                                class:dragging=move || drag.get().is_some_and(|d| d.lane == n && d.point == p)
                                style:left=format!("{left:.3}%") style:top=format!("{top:.3}%")
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
                change(&|t| {
                    if let Some(l) = t.lanes.get_mut(n) {
                        add_point(&mut l.points, ms, v);
                    }
                });
            };
            let moving = move |e: leptos::ev::PointerEvent| {
                let Some(d) = drag.get_untracked().filter(|d| d.lane == n) else {
                    return;
                };
                let Some(el) = e.current_target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
                    return;
                };
                let (ms, v) = at(&e, &el);
                drag.set(Some(Drag { at_ms: ms, value: v, ..d }));
            };
            let up = move |_| {
                let Some(d) = drag.get_untracked().filter(|d| d.lane == n) else {
                    return;
                };
                let moved = lane
                    .with_value(|l| l.points.get(d.point).is_some_and(|p| p.at_ms != d.at_ms || (p.value.get() - d.value).abs() > 1e-4));
                if moved {
                    change(&|t| {
                        if let Some(l) = t.lanes.get_mut(n) {
                            move_point(&mut l.points, d.point, d.at_ms, d.value, t.duration_ms);
                        }
                    });
                }
                drag.set(None);
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
                        <button type="button" class="seq-lane-name" on:click=open title="Open the channel">
                            <span class="seq-swatch"></span>
                            {name}
                        </button>
                        <div class="seq-lane-meta">
                            <span class="hint">"channel · "{level}</span>
                            <span class="seq-meter"><span style:width=meter></span></span>
                            <button type="button" class="seq-mute" class:on=!on on:click=mute
                                title=if on { "Mute this lane" } else { "Unmute this lane" }>"M"</button>
                            <button type="button" class="chan-x" aria-label="Remove this lane" title="Remove this lane" on:click=remove>"×"</button>
                        </div>
                    </div>
                    <div
                        class="seq-area"
                        on:dblclick=add
                        on:pointermove=moving
                        on:pointerup=up
                        on:pointercancel=up
                    >
                        <svg viewBox="0 0 1000 100" preserveAspectRatio="none" class="seq-svg" aria-hidden="true">
                            {move || {
                                let (lead, path) = paths();
                                view! {
                                    <path d=lead.clone() class="seq-lead" />
                                    <path d=format!("{path} L1000 100 L{} 100 Z", path_start(&path)) class="seq-fill" />
                                    <path d=path class="seq-line" />
                                }
                            }}
                        </svg>
                        {dots}
                    </div>
                </div>
            }
        })
        .collect_view();
    let point_menu = move || {
        let m = menu.get()?;
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
            <div class="context-menu" role="menu" style:left=format!("{}px", m.x) style:top=format!("{}px", m.y)>
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
    let empty = timeline.lanes.is_empty();
    view! {
        <div class="seq-lanes">
            <div class="seq-ruler-row">
                <div class="seq-lane-head seq-ruler-head"></div>
                <div class="seq-ruler" on:click=seek title="Click to play from here">{ticks}</div>
            </div>
            <div class="seq-body">
                <crate::panels::events::Events timeline=timeline.clone() scene=scene change=change />
                {lanes}
                {empty.then(|| view! {
                    <p class="placeholder seq-empty">"No lanes yet. Add lane searches the channels; a lane starts from where its channel is."</p>
                })}
                <div class="seq-head-layer">{head}</div>
            </div>
            {point_menu}
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
