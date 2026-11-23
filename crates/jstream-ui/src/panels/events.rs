//! The Sequence panel's Events lane (ADR-0184, UI-FEATURES SQ-04): a sequence's cues,
//! one-off actions at a time, as chips on the lanes' ruler. A double-click on the lane
//! adds one there through a small editor (what happens, and to what); a click on a chip
//! edits it, a drag moves it (snapped to a tenth of a second, Shift for free), and the
//! editor deletes it. Each gesture is one `timeline.set`, so one undo step.
//!
//! The events offered are the cue actions an operator reaches for: show, hide or toggle
//! an item of the sequence's scene, load one of its keyframes, put a scene on program,
//! switch a channel, play or stop another sequence, and restart a source. A cue some
//! other client wrote with another action shows as its action's type, and keeps working.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#automation

use crate::panels::sequence::{seconds, snap, timeline_name};
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    Action, ChannelSwitch, ItemId, SceneId, SourceId, TimelineCue, TimelineId, TimelineInfo,
};
use leptos::prelude::*;
use leptos::wasm_bindgen::JsCast;

/// What an event does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// `item.show`.
    Show,
    /// `item.hide`.
    Hide,
    /// `item.toggle`.
    Toggle,
    /// `scene.set_keyframe`.
    Keyframe,
    /// `scene.switch`.
    Program,
    /// `channel.switch` on.
    ChannelOn,
    /// `channel.switch` off.
    ChannelOff,
    /// `timeline.play`.
    Play,
    /// `timeline.stop`.
    Stop,
    /// `source.restart`.
    Restart,
}

impl EventKind {
    /// Every kind, in the editor's order.
    pub const ALL: [EventKind; 10] = [
        EventKind::Show,
        EventKind::Hide,
        EventKind::Toggle,
        EventKind::Keyframe,
        EventKind::Program,
        EventKind::ChannelOn,
        EventKind::ChannelOff,
        EventKind::Play,
        EventKind::Stop,
        EventKind::Restart,
    ];

    /// Its name in the editor.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::Show => "Show an item",
            EventKind::Hide => "Hide an item",
            EventKind::Toggle => "Toggle an item",
            EventKind::Keyframe => "Load a keyframe",
            EventKind::Program => "Put a scene on program",
            EventKind::ChannelOn => "Switch a channel on",
            EventKind::ChannelOff => "Switch a channel off",
            EventKind::Play => "Play a sequence",
            EventKind::Stop => "Stop a sequence",
            EventKind::Restart => "Restart a source",
        }
    }
}

/// What an event of `kind` can act on, for a sequence of scene `scene` (its family's
/// root) other than `own`: each as (its value in the editor, its name).
pub fn objects(
    collection: &Collection,
    scene: &SceneId,
    own: &TimelineId,
    kind: EventKind,
) -> Vec<(String, String)> {
    let items = || {
        collection
            .scene(scene)
            .map(|s| s.items.clone())
            .unwrap_or_default()
    };
    match kind {
        EventKind::Show | EventKind::Hide | EventKind::Toggle => items()
            .into_iter()
            .map(|i| (i.item_id.to_string(), collection.source_name(&i.source_id)))
            .collect(),
        EventKind::Restart => {
            let mut seen = Vec::new();
            for item in items() {
                if !seen.contains(&item.source_id) {
                    seen.push(item.source_id);
                }
            }
            seen.into_iter()
                .map(|s| (s.to_string(), collection.source_name(&s)))
                .collect()
        }
        EventKind::Keyframe => std::iter::once((String::new(), "Its own values".to_owned()))
            .chain(
                collection
                    .keyframes_of(scene)
                    .map(|k| (k.scene_id.to_string(), k.name.clone())),
            )
            .collect(),
        EventKind::Program => collection
            .listed_scenes()
            .filter(|s| s.variant_of.is_none())
            .map(|s| (s.scene_id.to_string(), s.name.clone()))
            .collect(),
        EventKind::ChannelOn | EventKind::ChannelOff => collection
            .channels
            .iter()
            .map(|c| (c.channel_id.clone(), collection.channel_name(&c.channel_id)))
            .collect(),
        EventKind::Play | EventKind::Stop => collection
            .scenes
            .iter()
            .flat_map(|s| &s.timelines)
            .filter(|t| &t.timeline_id != own)
            .map(|t| (t.timeline_id.to_string(), timeline_name(t)))
            .collect(),
    }
}

/// The action an event of `kind` on `object` (a value from [`objects`]) runs, for a
/// sequence of scene `scene`. `None` when it needs an object and has none.
pub fn action(kind: EventKind, scene: &SceneId, object: &str) -> Option<Action> {
    if object.is_empty() && kind != EventKind::Keyframe {
        return None;
    }
    let item = || ItemId::new(object);
    let in_scene = || Some(scene.clone());
    Some(match kind {
        EventKind::Show => Action::ItemShow {
            item_id: item(),
            scene_id: in_scene(),
        },
        EventKind::Hide => Action::ItemHide {
            item_id: item(),
            scene_id: in_scene(),
        },
        EventKind::Toggle => Action::ItemToggle {
            item_id: item(),
            scene_id: in_scene(),
        },
        EventKind::Keyframe => Action::SceneSetKeyframe {
            scene_id: scene.clone(),
            keyframe_id: (!object.is_empty()).then(|| SceneId::new(object)),
            preset_id: None,
        },
        EventKind::Program => Action::SceneSwitch {
            scene_id: SceneId::new(object),
            preset_id: None,
            via: Vec::new(),
        },
        EventKind::ChannelOn | EventKind::ChannelOff => Action::ChannelSwitch {
            channel_id: object.to_owned(),
            to: if kind == EventKind::ChannelOn {
                ChannelSwitch::On
            } else {
                ChannelSwitch::Off
            },
        },
        EventKind::Play => Action::TimelinePlay {
            timeline_id: TimelineId::new(object),
            delay_ms: None,
        },
        EventKind::Stop => Action::TimelineStop {
            timeline_id: TimelineId::new(object),
        },
        EventKind::Restart => Action::SourceRestart {
            source_id: SourceId::new(object),
        },
    })
}

/// The kind and object of a cue's action, for the editor; `None` for one it doesn't
/// offer.
pub fn kind_of(action: &Action) -> Option<(EventKind, String)> {
    Some(match action {
        Action::ItemShow { item_id, .. } => (EventKind::Show, item_id.to_string()),
        Action::ItemHide { item_id, .. } => (EventKind::Hide, item_id.to_string()),
        Action::ItemToggle { item_id, .. } => (EventKind::Toggle, item_id.to_string()),
        Action::SceneSetKeyframe { keyframe_id, .. } => (
            EventKind::Keyframe,
            keyframe_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        ),
        Action::SceneSwitch { scene_id, .. } => (EventKind::Program, scene_id.to_string()),
        Action::ChannelSwitch {
            channel_id,
            to: ChannelSwitch::On,
        } => (EventKind::ChannelOn, channel_id.clone()),
        Action::ChannelSwitch {
            channel_id,
            to: ChannelSwitch::Off,
        } => (EventKind::ChannelOff, channel_id.clone()),
        Action::TimelinePlay { timeline_id, .. } => (EventKind::Play, timeline_id.to_string()),
        Action::TimelineStop { timeline_id } => (EventKind::Stop, timeline_id.to_string()),
        Action::SourceRestart { source_id } => (EventKind::Restart, source_id.to_string()),
        _ => return None,
    })
}

/// A cue as a chip's few words: "Show Title", "Keyframe Wide", "Lights on".
pub fn text(collection: &Collection, cue: &TimelineCue) -> String {
    let Some(action) = cue.action() else {
        return "Unknown event".into();
    };
    let item = |id: &ItemId| {
        collection
            .scenes
            .iter()
            .flat_map(|s| &s.items)
            .find(|i| &i.item_id == id)
            .map_or_else(|| id.to_string(), |i| collection.source_name(&i.source_id))
    };
    let scene = |id: &SceneId| {
        collection
            .scene(id)
            .map_or_else(|| id.to_string(), |s| s.name.clone())
    };
    let sequence = |id: &TimelineId| {
        collection
            .timeline(id)
            .map_or_else(|| id.to_string(), |(_, t)| timeline_name(t))
    };
    match &action {
        Action::ItemShow { item_id, .. } => format!("Show {}", item(item_id)),
        Action::ItemHide { item_id, .. } => format!("Hide {}", item(item_id)),
        Action::ItemToggle { item_id, .. } => format!("Toggle {}", item(item_id)),
        Action::SceneSetKeyframe { keyframe_id, .. } => match keyframe_id {
            Some(k) => format!("Keyframe {}", scene(k)),
            None => "Own values".into(),
        },
        Action::SceneSwitch { scene_id, .. } => format!("Program {}", scene(scene_id)),
        Action::ChannelSwitch { channel_id, to } => {
            let state = match to {
                ChannelSwitch::On => "on",
                ChannelSwitch::Off => "off",
                _ => "toggle",
            };
            format!("{} {state}", collection.channel_name(channel_id))
        }
        Action::ChannelSetValue {
            channel_id, value, ..
        } => format!(
            "{} to {:.0} %",
            collection.channel_name(channel_id),
            value.get() * 100.0
        ),
        Action::TimelinePlay { timeline_id, .. } => format!("Play {}", sequence(timeline_id)),
        Action::TimelineStop { timeline_id } => format!("Stop {}", sequence(timeline_id)),
        Action::SourceRestart { source_id } => {
            format!("Restart {}", collection.source_name(source_id))
        }
        other => other.name().to_owned(),
    }
}

/// The cue editor's state: which cue (or a new one), where, and where it opened.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Editing {
    /// The cue's index, or `None` for a new one.
    cue: Option<usize>,
    at_ms: u32,
    x: f64,
    y: f64,
}

/// A chip being dragged: its index, where it is now, and whether it moved.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Moving {
    cue: usize,
    at_ms: u32,
    moved: bool,
}

/// The Events lane of `timeline`, which belongs to scene `scene`; `change` edits the
/// timeline as one `timeline.set`.
#[component]
pub fn Events(
    /// The sequence whose cues it shows.
    timeline: TimelineInfo,
    /// The scene the sequence belongs to.
    scene: SceneId,
    /// Edits the sequence, as one `timeline.set`.
    change: impl Fn(&dyn Fn(&mut TimelineInfo)) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let duration = timeline.duration_ms.max(1);
    let editing = RwSignal::new(None::<Editing>);
    let moving = RwSignal::new(None::<Moving>);
    let cues = StoredValue::new(timeline.cues.clone());
    let own = StoredValue::new(timeline.timeline_id.clone());
    let scene = StoredValue::new(scene);
    let at = move |e: &leptos::ev::MouseEvent, el: &web_sys::Element| {
        let rect = el.get_bounding_client_rect();
        let f = ((f64::from(e.client_x()) - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        snap(f * f64::from(duration), e.shift_key()).min(duration)
    };
    let area_of = |e: &leptos::ev::MouseEvent| {
        e.current_target()
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    };
    let add = move |e: leptos::ev::MouseEvent| {
        let Some(el) = area_of(&e) else { return };
        editing.set(Some(Editing {
            cue: None,
            at_ms: at(&e, &el),
            x: f64::from(e.client_x()),
            y: f64::from(e.client_y()),
        }));
    };
    let drag_to = move |e: leptos::ev::PointerEvent| {
        let Some(m) = moving.get_untracked() else {
            return;
        };
        let Some(el) = area_of(&e) else { return };
        let at_ms = at(&e, &el);
        if at_ms != m.at_ms {
            moving.set(Some(Moving {
                at_ms,
                moved: true,
                ..m
            }));
        }
    };
    let drop = move |e: leptos::ev::PointerEvent| {
        let Some(m) = moving.get_untracked() else {
            return;
        };
        moving.set(None);
        if m.moved {
            change(&|t| {
                if let Some(cue) = t.cues.get_mut(m.cue) {
                    cue.at_ms = m.at_ms;
                }
                t.cues.sort_by_key(|c| c.at_ms);
            });
        } else {
            editing.set(Some(Editing {
                cue: Some(m.cue),
                at_ms: m.at_ms,
                x: f64::from(e.client_x()),
                y: f64::from(e.client_y()),
            }));
        }
    };
    let chips = move || {
        cues.get_value()
            .into_iter()
            .enumerate()
            .map(|(n, cue)| {
                let shown_at = move || {
                    moving
                        .get()
                        .filter(|m| m.cue == n)
                        .map_or(cue.at_ms, |m| m.at_ms)
                };
                let left = move || {
                    format!(
                        "{:.3}%",
                        f64::from(shown_at()) / f64::from(duration) * 100.0
                    )
                };
                let words = studio
                    .collection
                    .with_untracked(|c| c.as_ref().map(|c| text(c, &cue)).unwrap_or_default());
                let title = format!(
                    "{words} at {} · click to edit, drag to move",
                    seconds(cue.at_ms)
                );
                let at_ms = cue.at_ms;
                let down = move |e: leptos::ev::PointerEvent| {
                    if e.button() != 0 {
                        return;
                    }
                    e.stop_propagation();
                    e.prevent_default();
                    if let Some(chip) = e
                        .current_target()
                        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    {
                        chip.set_pointer_capture(e.pointer_id()).ok();
                    }
                    moving.set(Some(Moving {
                        cue: n,
                        at_ms,
                        moved: false,
                    }));
                };
                view! {
                    <button
                        type="button"
                        class="seq-event"
                        class:dragging=move || moving.get().is_some_and(|m| m.cue == n && m.moved)
                        style:left=left
                        title=title
                        on:pointerdown=down
                        on:dblclick=|e: leptos::ev::MouseEvent| e.stop_propagation()
                    >
                        {words}
                    </button>
                }
            })
            .collect_view()
    };
    let editor = move || {
        let e = editing.get()?;
        let existing = e
            .cue
            .and_then(|n| cues.with_value(|c| c.get(n).cloned()))
            .and_then(|c| c.action())
            .and_then(|a| kind_of(&a));
        let kind = RwSignal::new(existing.as_ref().map_or(EventKind::Show, |(k, _)| *k));
        let object = RwSignal::new(existing.map(|(_, o)| o).unwrap_or_default());
        let options = move || {
            studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| {
                        own.with_value(|own| scene.with_value(|s| objects(c, s, own, kind.get())))
                    })
                    .unwrap_or_default()
            })
        };
        // A new kind picks its first object, unless the one set is one of its own.
        Effect::new(move |_| {
            let list = options();
            if !list.iter().any(|(v, _)| v == &object.get_untracked()) {
                object.set(list.first().map(|(v, _)| v.clone()).unwrap_or_default());
            }
        });
        let save = move |_| {
            let built =
                scene.with_value(|s| action(kind.get_untracked(), s, &object.get_untracked()));
            let Some(a) = built else {
                studio
                    .problem
                    .set(Some("Pick what the event acts on first.".into()));
                return;
            };
            editing.set(None);
            let cue = TimelineCue::new(e.at_ms, &a);
            change(&|t| {
                match e.cue {
                    Some(n) if n < t.cues.len() => t.cues[n] = cue.clone(),
                    _ => t.cues.push(cue.clone()),
                }
                t.cues.sort_by_key(|c| c.at_ms);
            });
        };
        let remove = move |_| {
            editing.set(None);
            if let Some(n) = e.cue {
                change(&|t| {
                    if n < t.cues.len() {
                        t.cues.remove(n);
                    }
                });
            }
        };
        let (left, top) = place(e.x, e.y);
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| editing.set(None)></div>
            <div class="context-menu seq-event-editor" role="dialog" aria-label="Event" style:left=left style:top=top>
                <div class="ctx-head">{format!("Event at {}", seconds(e.at_ms))}</div>
                <label class="seq-row">
                    <span>"What"</span>
                    <select on:change=move |ev| {
                        let n: usize = event_target_value(&ev).parse().unwrap_or(0);
                        kind.set(EventKind::ALL[n.min(EventKind::ALL.len() - 1)]);
                    }>
                        {EventKind::ALL.iter().enumerate().map(|(n, k)| {
                            let k = *k;
                            view! { <option value=n.to_string() selected=move || kind.get() == k>{k.label()}</option> }
                        }).collect_view()}
                    </select>
                </label>
                <label class="seq-row">
                    <span>"Which"</span>
                    <select on:change=move |ev| object.set(event_target_value(&ev))>
                        {move || {
                            let list = options();
                            if list.is_empty() {
                                return view! { <option value="">"Nothing to pick"</option> }.into_any();
                            }
                            list.into_iter().map(|(v, name)| {
                                let value = v.clone();
                                view! { <option value=v selected=move || object.get() == value>{name}</option> }
                            }).collect_view().into_any()
                        }}
                    </select>
                </label>
                <div class="seq-editor-buttons">
                    {e.cue.is_some().then(|| view! {
                        <button type="button" class="seq-delete" on:click=remove>"Delete"</button>
                    })}
                    <button type="button" on:click=move |_| editing.set(None)>"Cancel"</button>
                    <button type="button" class="primary" on:click=save>
                        {if e.cue.is_some() { "Save" } else { "Add" }}
                    </button>
                </div>
            </div>
        })
    };
    let empty = timeline.cues.is_empty();
    view! {
        <div class="seq-lane seq-events">
            <div class="seq-lane-head">
                <span class="seq-lane-name seq-events-name">"Events"</span>
                <span class="hint">"One-off steps: double-click"</span>
            </div>
            <div
                class="seq-area seq-events-area"
                title="Double-click to add an event here"
                on:dblclick=add
                on:pointermove=drag_to
                on:pointerup=drop
                on:pointercancel=move |_| moving.set(None)
            >
                {chips}
                {empty.then(|| view! { <span class="hint seq-events-empty">"Double-click to add an event"</span> })}
            </div>
        </div>
        {editor}
    }
}

/// Where the editor opens for a click at (`x`, `y`): below it, kept inside the window.
fn place(x: f64, y: f64) -> (String, String) {
    let (w, h) = web_sys::window()
        .map(|w| {
            let n = |v: Result<leptos::wasm_bindgen::JsValue, leptos::wasm_bindgen::JsValue>| {
                v.ok().and_then(|v| v.as_f64())
            };
            (
                n(w.inner_width()).unwrap_or(1600.0),
                n(w.inner_height()).unwrap_or(1000.0),
            )
        })
        .unwrap_or((1600.0, 1000.0));
    let left = (x - 20.0).min(w - 330.0).max(4.0);
    let top = if y + 190.0 > h {
        (y - 190.0).max(4.0)
    } else {
        y + 10.0
    };
    (format!("{left}px"), format!("{top}px"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{ChannelInfo, ItemInfo, SceneInfo};

    fn scene(id: &str, name: &str) -> SceneInfo {
        let mut s: SceneInfo = serde_json::from_value(serde_json::json!({
            "scene_id": id, "name": name, "role": "scene", "items": []
        }))
        .expect("a scene");
        s.name = name.into();
        s
    }

    fn collection() -> Collection {
        let mut main = scene("main", "Main");
        let item = ItemInfo {
            motion: Default::default(),
            item_id: "title_1".into(),
            source_id: "title".into(),
            placement: jstream_control::protocol::PlacementForm::Legacy,
            transform: jstream_control::protocol::ItemTransform::IDENTITY,
            geometry: None,
            crop: jstream_control::protocol::Crop::NONE,
            visible: true,
            effects: Vec::new(),
            opacity: jstream_control::protocol::Finite32::ONE,
            blend: Default::default(),
            transition: Default::default(),
            locked: false,
            effects_space: jstream_control::protocol::EffectsSpace::Box,
        };
        main.items = vec![item];
        main.timelines = vec![
            crate::panels::sequence::empty_sequence("intro".into(), "Intro".into()),
            crate::panels::sequence::empty_sequence("outro".into(), "Outro".into()),
        ];
        let mut wide = scene("wide", "Wide");
        wide.variant_of = Some("main".into());
        Collection {
            scenes: vec![main, wide],
            channels: vec![ChannelInfo::new("lights")],
            ..Collection::default()
        }
    }

    /// Each kind offers what it acts on, builds its action, and reads back.
    #[test]
    fn each_event_kind_builds_its_cue_and_reads_back() {
        let c = collection();
        let main = SceneId::new("main");
        let own = TimelineId::new("intro");
        assert_eq!(
            objects(&c, &main, &own, EventKind::Play),
            [("outro".to_owned(), "Outro".to_owned())],
            "not the sequence itself"
        );
        assert_eq!(objects(&c, &main, &own, EventKind::Keyframe).len(), 2);
        assert_eq!(objects(&c, &main, &own, EventKind::Show)[0].0, "title_1");
        for kind in EventKind::ALL {
            let object = objects(&c, &main, &own, kind)
                .into_iter()
                .last()
                .map(|(v, _)| v)
                .unwrap_or_default();
            let a = action(kind, &main, &object)
                .unwrap_or_else(|| panic!("{kind:?} builds with {object:?}"));
            assert_eq!(a.cue_problem(), None, "{kind:?} is a cue");
            assert_eq!(kind_of(&a), Some((kind, object)), "{kind:?} reads back");
        }
        assert_eq!(action(EventKind::Show, &main, ""), None);
        let cue = TimelineCue::new(
            500,
            &Action::ChannelSwitch {
                channel_id: "lights".into(),
                to: ChannelSwitch::On,
            },
        );
        assert_eq!(text(&c, &cue), "lights on");
        let cue = TimelineCue::new(
            0,
            &action(EventKind::Keyframe, &main, "wide").unwrap_or_else(|| panic!("keyframe")),
        );
        assert_eq!(text(&c, &cue), "Keyframe Wide");
    }
}
