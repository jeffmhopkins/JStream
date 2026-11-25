//! The Sequence panel's Events lane (ADR-0184, UI-FEATURES SQ-04): a sequence's cues,
//! one-off actions at a time, as chips on the lanes' ruler. A double-click on the lane
//! adds one there through a small editor (what happens, and to what); a click or a
//! right-click on a chip edits it, a drag moves it (snapped to a tenth of a second, Shift for free), and the
//! editor deletes it. Each gesture is one `timeline.set`, so one undo step.
//!
//! The events offered are the cue actions an operator reaches for: show, hide or toggle
//! an item of the sequence's scene, load one of its keyframes, put a scene on program,
//! switch a channel, play or stop another sequence, and restart a source. A cue the
//! editor can't build again as it is (another action, an item of another scene, a play
//! with a delay) shows as its action's type and keeps working; a click on it opens it
//! read-only, so it can be moved or deleted but never overwritten. An event whose object
//! is gone keeps it, marked "(missing)", until another is picked. Each edit finds its
//! cue by time and action, not by where it was drawn, and sends nothing if another
//! client changed it meanwhile. From the keyboard, a chip opens its editor with Enter,
//! Space, the Menu key or Shift+F10, and Delete removes it.
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

/// The kind and object of `cue` for the editor of a sequence of scene `scene`, when the
/// editor can show it as it is: building it again from them gives the same action.
/// `None` for one it doesn't offer, or one it would change (an item of another scene, a
/// play with a delay, a switch through a preset), which opens read-only.
pub fn editable(scene: &SceneId, cue: &TimelineCue) -> Option<(EventKind, String)> {
    let held = cue.action()?;
    let (kind, object) = kind_of(&held)?;
    (action(kind, scene, &object).as_ref() == Some(&held)).then_some((kind, object))
}

/// Where `cue` is in `t`'s cues now, matched by its time and action rather than by
/// where it was drawn, so an edit lands on the cue it was made on.
pub fn position(t: &TimelineInfo, cue: &TimelineCue) -> Option<usize> {
    t.cues.iter().position(|c| c == cue)
}

/// A cue as a chip's few words: "Show Title", "Keyframe Wide", "Lights on".
pub fn text(collection: &Collection, cue: &TimelineCue) -> String {
    match cue.action() {
        Some(action) => action_text(collection, &action),
        None => "Unknown event".into(),
    }
}

/// An action as a chip's few words, as [`text`] reads a cue's.
pub fn action_text(collection: &Collection, action: &Action) -> String {
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
    match action {
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
#[derive(Clone, Debug, PartialEq)]
struct Editing {
    /// The cue as it was drawn, or `None` for a new one.
    cue: Option<TimelineCue>,
    at_ms: u32,
    x: f64,
    y: f64,
}

/// A chip being dragged: its place among the chips, the cue as it was drawn, where it
/// is now, how far into it the pointer went down (so it doesn't jump to the pointer),
/// where the pointer went down, and whether it moved past a click's few pixels.
#[derive(Clone, Debug, PartialEq)]
struct Moving {
    cue: usize,
    held: TimelineCue,
    at_ms: u32,
    offset_ms: i64,
    x0: f64,
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
    /// The part of the sequence in view (`seq_zoom`): the lane's frame follows it.
    zoom: RwSignal<crate::panels::show::time::Viewport>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let duration = timeline.duration_ms.max(1);
    let inset = crate::panels::sequence::AREA_INSET_PX;
    let frame = move || crate::panels::seq_zoom::frame(zoom.get(), inset);
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
    // Edits cue `held` where it is in the sequence now; if another client changed or
    // removed it meanwhile, nothing is sent and the operator is told.
    let on_cue = move |held: &TimelineCue, f: &dyn Fn(&mut TimelineInfo, usize)| {
        let gone = std::cell::Cell::new(false);
        change(&|t| match position(t, held) {
            Some(n) => f(t, n),
            None => gone.set(true),
        });
        if gone.get() {
            studio.problem.set(Some(
                "That event was changed elsewhere meanwhile, so it was left as it is.".into(),
            ));
        }
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
            x: crate::ui_scale::pointer(&e).0,
            y: crate::ui_scale::pointer(&e).1,
        }));
    };
    let drag_to = move |e: leptos::ev::PointerEvent| {
        let Some(m) = moving.get_untracked() else {
            return;
        };
        // A few pixels are still a click.
        if !m.moved && (f64::from(e.client_x()) - m.x0).abs() <= 3.0 {
            return;
        }
        let Some(el) = area_of(&e) else { return };
        let at_ms = (i64::from(at(&e, &el)) - m.offset_ms).clamp(0, i64::from(duration)) as u32;
        if at_ms != m.at_ms || !m.moved {
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
        if m.moved && m.at_ms != m.held.at_ms {
            on_cue(&m.held, &|t, n| {
                t.cues[n].at_ms = m.at_ms;
                t.cues.sort_by_key(|c| c.at_ms);
            });
        } else if !m.moved {
            editing.set(Some(Editing {
                cue: Some(m.held),
                at_ms: m.at_ms,
                x: crate::ui_scale::pointer(&e).0,
                y: crate::ui_scale::pointer(&e).1,
            }));
        }
    };
    let chips = move || {
        cues.get_value()
            .into_iter()
            .enumerate()
            .map(|(n, cue)| {
                let drawn_at = cue.at_ms;
                let shown_at = move || {
                    moving.with(|m| {
                        m.as_ref()
                            .filter(|m| m.cue == n)
                            .map_or(drawn_at, |m| m.at_ms)
                    })
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
                let held = cue.clone();
                let keyed = cue.clone();
                let clicked = cue.clone();
                // A right-click opens the editor too, as it opens a point's menu.
                let context = move |e: leptos::ev::MouseEvent| {
                    e.prevent_default();
                    e.stop_propagation();
                    editing.set(Some(Editing {
                        cue: Some(clicked.clone()),
                        at_ms,
                        x: crate::ui_scale::pointer(&e).0,
                        y: crate::ui_scale::pointer(&e).1,
                    }));
                };
                // From the keyboard: Enter, Space, the Menu key or Shift+F10 open the
                // editor under the chip, and Delete removes the event.
                let key = move |e: leptos::ev::KeyboardEvent| {
                    let k = e.key();
                    if k == "Enter" || k == " " || crate::menu::asks_for_menu(&e) {
                        e.prevent_default();
                        e.stop_propagation();
                        let Some(chip) = e
                            .current_target()
                            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                        else {
                            return;
                        };
                        let r = crate::ui_scale::rect(&chip);
                        editing.set(Some(Editing {
                            cue: Some(keyed.clone()),
                            at_ms,
                            x: r.left(),
                            y: r.bottom(),
                        }));
                    } else if k == "Delete" || k == "Backspace" {
                        e.prevent_default();
                        e.stop_propagation();
                        on_cue(&keyed, &|t, n| {
                            t.cues.remove(n);
                        });
                    }
                };
                let down = move |e: leptos::ev::PointerEvent| {
                    if e.button() != 0 {
                        return;
                    }
                    e.stop_propagation();
                    e.prevent_default();
                    let Some(chip) = e
                        .current_target()
                        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    else {
                        return;
                    };
                    chip.set_pointer_capture(e.pointer_id()).ok();
                    let offset_ms = chip
                        .closest(".seq-events-area")
                        .ok()
                        .flatten()
                        .map_or(0, |area| i64::from(at(&e, &area)) - i64::from(at_ms));
                    moving.set(Some(Moving {
                        cue: n,
                        held: held.clone(),
                        at_ms,
                        offset_ms,
                        x0: f64::from(e.client_x()),
                        moved: false,
                    }));
                };
                view! {
                    <button
                        type="button"
                        class="seq-event"
                        class:dragging=move || moving.with(|m| m.as_ref().is_some_and(|m| m.cue == n && m.moved))
                        style:left=left
                        title=title
                        on:pointerdown=down
                        on:keydown=key
                        on:contextmenu=context
                        on:dblclick=|e: leptos::ev::MouseEvent| e.stop_propagation()
                    >
                        {words}
                    </button>
                }
            })
            .collect_view()
    };
    // Escape closes the editor, as a click outside it does.
    let escape = move |ev: leptos::ev::KeyboardEvent| {
        if ev.key() == "Escape" {
            ev.prevent_default();
            ev.stop_propagation();
            editing.set(None);
        }
    };
    let editor = move || {
        let e = editing.get()?;
        // It opens with the focus on its first control, so it works from the keyboard.
        let dialog = NodeRef::<leptos::html::Div>::new();
        Effect::new(move |_| {
            if let Some(el) = dialog.get()
                && let Ok(Some(first)) = el.query_selector("select, button")
                && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
            {
                _ = first.focus();
            }
        });
        let held = e.cue.clone();
        let existing = held
            .as_ref()
            .and_then(|cue| scene.with_value(|s| editable(s, cue)));
        // A cue the editor can't show as it is opens read-only, so a Save can't replace
        // it with something it never was: it can only be moved or deleted here.
        if let (Some(cue), None) = (held.clone(), existing.as_ref()) {
            let words = studio
                .collection
                .with_untracked(|c| c.as_ref().map(|c| text(c, &cue)).unwrap_or_default());
            let remove = move |_| {
                editing.set(None);
                on_cue(&cue, &|t, n| {
                    t.cues.remove(n);
                });
            };
            let (left, top) = place(e.x, e.y, 190.0);
            return Some(view! {
                <div class="ctx-catcher" on:pointerdown=move |_| editing.set(None)></div>
                <div class="context-menu seq-event-editor" role="dialog" aria-label="Event" node_ref=dialog on:keydown=escape style:left=left style:top=top>
                    <div class="ctx-head">{format!("Event at {}", seconds(e.at_ms))}</div>
                    <p class="seq-event-note">{words}</p>
                    <p class="hint">"This kind of event is set elsewhere, by an agent or another client. It runs as it is; here it can only be moved or deleted."</p>
                    <div class="seq-editor-buttons">
                        <button type="button" class="seq-delete" on:click=remove>"Delete"</button>
                        <button type="button" on:click=move |_| editing.set(None)>"Close"</button>
                    </div>
                </div>
            }
            .into_any());
        }
        let kind = RwSignal::new(existing.as_ref().map_or(EventKind::Show, |(k, _)| *k));
        let object = RwSignal::new(
            existing
                .as_ref()
                .map(|(_, o)| o.clone())
                .unwrap_or_default(),
        );
        let kept = StoredValue::new(existing);
        let options = move || {
            let mut list = studio.collection.with(|c| {
                c.as_ref()
                    .map(|c| {
                        own.with_value(|own| scene.with_value(|s| objects(c, s, own, kind.get())))
                    })
                    .unwrap_or_default()
            });
            // An existing cue's object that is no longer there stays listed, marked, so
            // a Save never points the cue at something else without being asked to.
            kept.with_value(|kept| {
                if let Some((k, o)) = kept
                    && *k == kind.get()
                    && !o.is_empty()
                    && !list.iter().any(|(v, _)| v == o)
                {
                    list.insert(0, (o.clone(), format!("{o} (missing)")));
                }
            });
            list
        };
        // A new kind picks its first object, unless the one set is one of its own.
        Effect::new(move |_| {
            let list = options();
            if !list.iter().any(|(v, _)| v == &object.get_untracked()) {
                object.set(list.first().map(|(v, _)| v.clone()).unwrap_or_default());
            }
        });
        let is_new = e.cue.is_none();
        let target = e.cue.clone();
        let at_ms = e.at_ms;
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
            let cue = TimelineCue::new(at_ms, &a);
            match &target {
                Some(held) => on_cue(held, &|t, n| {
                    t.cues[n] = cue.clone();
                    t.cues.sort_by_key(|c| c.at_ms);
                }),
                None => {
                    change(&|t| {
                        t.cues.push(cue.clone());
                        t.cues.sort_by_key(|c| c.at_ms);
                    });
                }
            }
        };
        let removed = e.cue.clone();
        let remove = move |_| {
            editing.set(None);
            if let Some(held) = &removed {
                on_cue(held, &|t, n| {
                    t.cues.remove(n);
                });
            }
        };
        let (left, top) = place(e.x, e.y, 190.0);
        Some(view! {
            <div class="ctx-catcher" on:pointerdown=move |_| editing.set(None)></div>
            <div class="context-menu seq-event-editor" role="dialog" aria-label="Event" node_ref=dialog on:keydown=escape style:left=left style:top=top>
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
                    {(!is_new).then(|| view! {
                        <button type="button" class="seq-delete" on:click=remove>"Delete"</button>
                    })}
                    <button type="button" on:click=move |_| editing.set(None)>"Cancel"</button>
                    <button type="button" class="primary" on:click=save>
                        {if is_new { "Add" } else { "Save" }}
                    </button>
                </div>
            </div>
        }
        .into_any())
    };
    view! {
        <div class="seq-lane seq-events">
            <div class="seq-lane-head">
                <div class="seq-lane-title">
                    <span class="seq-lane-name seq-events-name">"Events"</span>
                </div>
                <span class="hint">"Double-click to add one"</span>
            </div>
            <div class="seq-clip">
                <div
                    class="seq-area seq-events-area"
                    title="Double-click to add an event here"
                    style:width=move || frame().0
                    style:margin-left=move || frame().1
                    on:dblclick=add
                    on:pointermove=drag_to
                    on:pointerup=drop
                    on:pointercancel=move |_| moving.set(None)
                >
                    {chips}
                </div>
            </div>
        </div>
        {editor}
    }
}

/// Where an editor `height` px tall opens for a click at (`x`, `y`): below it, or above
/// it where there is no room below, kept inside the window. The Sequence panel's trigger
/// editor opens this way too.
pub(crate) fn place(x: f64, y: f64, height: f64) -> (String, String) {
    let (w, h) = match crate::ui_scale::window_size() {
        (0.0, _) | (_, 0.0) => (1600.0, 1000.0),
        size => size,
    };
    let left = (x - 20.0).min(w - 330.0).max(4.0);
    let top = if y + height > h {
        (y - height).max(4.0)
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

    /// The editor edits a cue only when it can build the same action again, and finds a
    /// cue by its time and action wherever it is now.
    #[test]
    fn a_cue_is_edited_as_it_is_and_found_where_it_is_now() {
        let main = SceneId::new("main");
        let show = |scene: &str| Action::ItemShow {
            item_id: "title_1".into(),
            scene_id: Some(SceneId::new(scene)),
        };
        let mine = TimelineCue::new(100, &show("main"));
        assert_eq!(
            editable(&main, &mine),
            Some((EventKind::Show, "title_1".into()))
        );
        let elsewhere = TimelineCue::new(100, &show("other"));
        assert_eq!(editable(&main, &elsewhere), None, "another scene's item");
        let delayed = TimelineCue::new(
            100,
            &Action::TimelinePlay {
                timeline_id: "outro".into(),
                delay_ms: Some(500),
            },
        );
        assert_eq!(editable(&main, &delayed), None, "a play with a delay");
        let mut t = crate::panels::sequence::empty_sequence("intro".into(), "Intro".into());
        t.cues = vec![elsewhere.clone(), mine.clone()];
        assert_eq!(position(&t, &mine), Some(1));
        t.cues.insert(0, delayed);
        assert_eq!(position(&t, &mine), Some(2), "found after an insert");
        t.cues.retain(|c| c != &mine);
        assert_eq!(position(&t, &mine), None, "gone");
    }
}
