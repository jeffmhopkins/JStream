//! The Channels panel (ADR-0183, UI-FEATURES VC-02): a search over the collection's
//! channels, and the open channel's card. The card shows the live value with what drives
//! it now, the on switch, the inputs that drive it (learned from a MIDI control, or read
//! from another channel), its shaping (fades, averaging, curve, combine, the value it
//! takes when off), the settings that follow it and the sequences with a lane on it.
//!
//! The control button beside each live setting (`learn.rs`) makes a channel or follows
//! one through [`ChannelSearch`], the same search the Sequence panel's Add lane uses.
//! Live values come from `channel.values`, read about ten times a second while a panel
//! that shows them is open ([`live_values`]).
//!
//! Docs: docs/adr/0183-virtual-control-channels.md, docs/UI-FEATURES.md#automation

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::panels::learn::{controls, moved};
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio, query};
use crate::workspace::DockState;
use jstream_control::protocol::{
    Action, Call, ChannelCombine, ChannelDriver, ChannelInfo, ChannelSwitch, ChannelValueInfo,
    Edit, Finite32, MidiControlInfo, MotionCurve, MotionKind, MotionMidiControl, Query,
    QueryResult, SceneId, ValueTarget,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashMap;
use std::time::Duration;

/// How often an open panel reads the channels' values.
const POLL: Duration = Duration::from_millis(100);

/// Each channel's live value by ID, read with `channel.values` every 100 ms for as
/// long as the calling component lives.
pub fn live_values() -> RwSignal<HashMap<String, ChannelValueInfo>> {
    let values = RwSignal::new(HashMap::new());
    if let Some(session) = use_context::<SessionContext>() {
        poll(session, values);
    }
    values
}

/// Reads the values after [`POLL`], then again, until `values` is disposed.
fn poll(session: SessionContext, values: RwSignal<HashMap<String, ChannelValueInfo>>) {
    set_timeout(
        move || {
            spawn_local(async move {
                let Some(s) = session.0.try_get_value().flatten() else {
                    return;
                };
                if values.try_get_untracked().is_none() {
                    return;
                }
                if let Ok(QueryResult::ChannelValues { channels }) =
                    query(&s, Query::ChannelValues).await
                {
                    let map: HashMap<_, _> = channels
                        .into_iter()
                        .map(|c| (c.channel_id.clone(), c))
                        .collect();
                    if values.try_set(map).is_some() {
                        return;
                    }
                }
                if values.try_get_untracked().is_some() {
                    poll(session, values);
                }
            });
        },
        POLL,
    );
}

/// `name` as a channel ID: lower case ASCII letters and digits joined by `_`, at most
/// 48 bytes, then `_2`, `_3` and so on until no channel in `taken` has it.
pub fn channel_id_for(name: &str, taken: &[ChannelInfo]) -> String {
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('_') && !base.is_empty() {
            base.push('_');
        }
    }
    let mut base = base.trim_end_matches('_').to_owned();
    base.truncate(48);
    if base.is_empty() {
        base = "channel".into();
    }
    let free = |id: &str| taken.iter().all(|c| c.channel_id != id);
    if free(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| free(id))
        .unwrap_or(base)
}

/// A new channel named `name`, in `scene` or the project, resting at `default`.
pub fn new_channel(
    name: &str,
    scene: Option<SceneId>,
    default: f32,
    taken: &[ChannelInfo],
) -> ChannelInfo {
    let mut channel = ChannelInfo::new(channel_id_for(name, taken));
    channel.name = name.trim().to_owned();
    channel.scene_id = scene;
    channel.default = Finite32::new(default.clamp(0.0, 1.0)).unwrap_or(Finite32::ZERO);
    channel
}

/// A channel input as the card lists it: "MIDI CC 21 · any channel", "Note 36",
/// "Data `x`", "Stage power".
pub fn input_name(collection: &Collection, input: &MotionKind) -> String {
    match input {
        MotionKind::Midi {
            control,
            controller,
            channel,
            ..
        } => {
            let on = channel.map_or("any channel".to_owned(), |c| format!("channel {c}"));
            let what = match (control, controller) {
                (MotionMidiControl::Cc, Some(n)) => format!("CC {n}"),
                (MotionMidiControl::Note, Some(n)) => format!("Note {n}"),
                (MotionMidiControl::PitchBend, _) => "Bend".into(),
                (MotionMidiControl::Pressure, _) => "Pressure".into(),
                _ => "Control".into(),
            };
            format!("MIDI {what} · {on}")
        }
        MotionKind::Data { key, field, .. } => format!("Data {key} · {field}"),
        MotionKind::Channel { channel_id, .. } => collection.channel_name(channel_id),
        _ => "Input".into(),
    }
}

/// What a value binding drives, as the card's Drives list says it.
pub fn target_name(collection: &Collection, target: &ValueTarget) -> String {
    match target {
        ValueTarget::SourceSetting { source_id, param } => {
            format!("{} · {param}", collection.source_name(source_id))
        }
        ValueTarget::FilterSetting {
            filter_id, param, ..
        } => format!("{filter_id} · {param}"),
        ValueTarget::SourceGain { source_id } => {
            format!("{} · volume", collection.source_name(source_id))
        }
        ValueTarget::BusGain { bus_id } => {
            let name = collection
                .buses
                .iter()
                .find(|b| &b.bus_id == bus_id)
                .map_or_else(|| bus_id.to_string(), |b| b.name.clone());
            format!("{name} · fader")
        }
        ValueTarget::SendGain { source_id, bus_id } => {
            format!("{} · send to {bus_id}", collection.source_name(source_id))
        }
        ValueTarget::SourceMute { source_id } => {
            format!("{} · mute", collection.source_name(source_id))
        }
        ValueTarget::BusMute { bus_id } => {
            let name = collection
                .buses
                .iter()
                .find(|b| &b.bus_id == bus_id)
                .map_or_else(|| bus_id.to_string(), |b| b.name.clone());
            format!("{name} · mute")
        }
        ValueTarget::ItemVisible { item_id, .. } => {
            let name = collection.item(item_id).map_or_else(
                || item_id.to_string(),
                |(_, item)| collection.source_name(&item.source_id),
            );
            format!("{name} · visible")
        }
        _ => "A setting".into(),
    }
}

/// The line under the meter: what drives the channel now.
pub fn driver_line(collection: &Collection, value: Option<&ChannelValueInfo>) -> String {
    let Some(value) = value else {
        return "Not read yet".into();
    };
    if !value.on {
        return "Switched off".into();
    }
    match &value.driver {
        Some(ChannelDriver::Input { index }) => collection
            .channel(&value.channel_id)
            .and_then(|c| c.inputs.get(usize::from(*index)))
            .map_or("Driven by an input".into(), |input| {
                format!("Driven by {}", input_name(collection, input))
            }),
        Some(ChannelDriver::Action) => "Set by an action".into(),
        Some(ChannelDriver::Sequence { timeline_id }) => {
            let name = collection
                .timeline(timeline_id)
                .map(|(_, t)| t.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| timeline_id.to_string());
            format!("Driven by the sequence “{name}”")
        }
        _ => "At its default".into(),
    }
}

/// The channels `query` finds, by name or ID, those of `scene` first, each group in
/// name order.
pub fn search<'a>(
    channels: &'a [ChannelInfo],
    query: &str,
    scene: Option<&SceneId>,
) -> (Vec<&'a ChannelInfo>, Vec<&'a ChannelInfo>) {
    let q = query.trim().to_lowercase();
    let mut found: Vec<&ChannelInfo> = channels
        .iter()
        .filter(|c| {
            q.is_empty()
                || c.name.to_lowercase().contains(&q)
                || c.channel_id.to_lowercase().contains(&q)
        })
        .collect();
    found.sort_by_key(|c| c.name.to_lowercase());
    found
        .into_iter()
        .partition(|c| scene.is_some() && c.scene_id.as_ref() == scene)
}

/// Opens channel `channel_id` in the Channels panel, showing the panel.
pub fn open_channel(studio: Studio, channel_id: String) {
    studio.channel.set(Some(channel_id));
    if let Some(dock) = use_context::<DockState>() {
        dock.0.update(|d| d.show(PanelId::Channels));
    }
}

/// A search box over the channels, this scene's first, with "+ New channel" at its
/// foot. `pick` takes a channel's ID; `make` takes a new channel's name.
#[component]
pub fn ChannelSearch(
    /// What the box says before anything is typed.
    #[prop(into)]
    placeholder: String,
    /// Takes the picked channel's ID.
    pick: impl Fn(String) + Copy + Send + Sync + 'static,
    /// Takes the name typed for a new channel.
    make: impl Fn(String) + Copy + Send + Sync + 'static,
    /// The live values, for the level beside each.
    #[prop(optional)]
    values: Option<RwSignal<HashMap<String, ChannelValueInfo>>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let text = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(el) = input.get() {
            _ = el.focus();
        }
    });
    let level = move |id: &str| {
        values
            .and_then(|v| v.with(|v| v.get(id).map(|c| c.value.get())))
            .map(|v| format!("{:.0} %", v * 100.0))
            .unwrap_or_default()
    };
    let row = move |c: &ChannelInfo| {
        let id = c.channel_id.clone();
        let name = if c.name.is_empty() {
            c.channel_id.clone()
        } else {
            c.name.clone()
        };
        let shown = id.clone();
        view! {
            <button type="button" class="menuitem chan-pick" on:click=move |_| pick(id.clone())>
                <span class="ctx-label">{name}</span>
                <span class="hint">{move || level(&shown)}</span>
            </button>
        }
    };
    let groups = move || {
        let scene = studio.selection.with(|s| s.scene.clone());
        let q = text.get();
        studio.collection.with(|c| {
            let Some(c) = c else {
                return ().into_any();
            };
            let (here, rest) = search(&c.channels, &q, scene.as_ref());
            let here_view = (!here.is_empty()).then(|| {
                view! {
                    <div class="ctx-head">"This scene"</div>
                    {here.into_iter().map(row).collect_view()}
                }
            });
            let rest_view = (!rest.is_empty()).then(|| {
                view! {
                    <div class="ctx-head">"Project"</div>
                    {rest.into_iter().map(row).collect_view()}
                }
            });
            view! { {here_view} {rest_view} }.into_any()
        })
    };
    let new_label = move || {
        let q = text.get();
        if q.trim().is_empty() {
            "+ New channel".to_owned()
        } else {
            format!("+ New channel “{}”", q.trim())
        }
    };
    let create = move || {
        let q = text.get_untracked();
        make(if q.trim().is_empty() {
            "Channel".into()
        } else {
            q.trim().to_owned()
        });
    };
    view! {
        <div class="chan-search">
            <div class="tree-search">
                <Icon name="search" size=13 />
                <input
                    node_ref=input
                    type="search"
                    placeholder=placeholder
                    aria-label="Search channels"
                    prop:value=move || text.get()
                    on:input=move |e| text.set(event_target_value(&e))
                    on:keydown=move |e: leptos::ev::KeyboardEvent| {
                        if e.key() == "Enter" {
                            let q = text.get_untracked();
                            let first = studio.collection.with_untracked(|c| {
                                c.as_ref().and_then(|c| {
                                    let scene = studio.selection.with_untracked(|s| s.scene.clone());
                                    let (here, rest) = search(&c.channels, &q, scene.as_ref());
                                    here.into_iter().chain(rest).next().map(|c| c.channel_id.clone())
                                })
                            });
                            match first {
                                Some(id) => pick(id),
                                None => create(),
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

/// The Channels panel: a search with the channels, and the open channel's card.
#[component]
pub fn Channels() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let values = live_values();
    let make = move |name: String| {
        let Some(s) = session.0.get_value() else {
            return;
        };
        let scene = studio.selection.with_untracked(|sel| sel.scene.clone());
        let channel = studio.collection.with_untracked(|c| {
            new_channel(
                &name,
                scene,
                0.0,
                c.as_ref().map_or(&[][..], |c| &c.channels[..]),
            )
        });
        studio.channel.set(Some(channel.channel_id.clone()));
        studio.apply(&s, vec![Edit::ChannelSet { channel }]);
    };
    let pick = move |id: String| studio.channel.set(Some(id));
    // A memo, so an edit elsewhere doesn't rebuild the card (and cancel a learn in it).
    let open = Memo::new(move |_| {
        let id = studio.channel.get()?;
        let exists = studio
            .collection
            .with(|c| c.as_ref().is_some_and(|c| c.channel(&id).is_some()));
        exists.then_some(id)
    });
    view! {
        <section class="panel chan-panel">
            {move || match open.get() {
                Some(id) => view! {
                    <button type="button" class="chan-back" on:click=move |_| studio.channel.set(None)>
                        "‹ All channels"
                    </button>
                    <ChannelCard channel_id=id values=values />
                }
                .into_any(),
                None => view! {
                    <ChannelSearch placeholder="Find or make a channel" pick=pick make=make values=values />
                    <p class="hint chan-help">
                        "A channel is a named live value, 0 to 100 %. Controls, keys and sequences set it; any setting can follow it."
                    </p>
                }
                .into_any(),
            }}
        </section>
    }
}

/// One channel's card.
#[component]
fn ChannelCard(
    channel_id: String,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let id = StoredValue::new(channel_id);
    let info = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| id.with_value(|id| c.channel(id).cloned()))
        })
    };
    let set = move |change: &dyn Fn(&mut ChannelInfo)| {
        let Some(s) = session.0.get_value() else {
            return false;
        };
        let Some(mut channel) = untrack(info) else {
            return false;
        };
        change(&mut channel);
        studio.apply(&s, vec![Edit::ChannelSet { channel }]);
        true
    };
    let act = move |action: Action| {
        if let Some(s) = session.0.get_value() {
            studio.act(&s, action);
        }
    };
    let live = move || values.with(|v| id.with_value(|id| v.get(id).cloned()));
    let level = move || live().map_or(0.0, |v| v.value.get());
    let on = move || live().is_none_or(|v| v.on);
    let name = move || {
        info().map_or_else(String::new, |c| {
            if c.name.is_empty() {
                c.channel_id
            } else {
                c.name
            }
        })
    };
    let place = move || {
        let scene = info().and_then(|c| c.scene_id);
        studio.collection.with(|c| {
            scene
                .and_then(|s| c.as_ref().and_then(|c| c.scene(&s)).map(|s| s.name.clone()))
                .unwrap_or_else(|| "Project".into())
        })
    };
    let driver = move || {
        let live = live();
        studio.collection.with(|c| {
            c.as_ref()
                .map_or_else(String::new, |c| driver_line(c, live.as_ref()))
        })
    };
    let toggle = move |_| {
        act(Action::ChannelSwitch {
            channel_id: id.get_value(),
            to: if on() {
                ChannelSwitch::Off
            } else {
                ChannelSwitch::On
            },
        });
    };
    // The value under the hand while the slider is held: the poll doesn't move the
    // thumb then, and at most one action is in flight, with the latest one queued.
    let hand = RwSignal::new(None::<f32>);
    let queued = StoredValue::new(None::<f32>);
    let sending = StoredValue::new(false);
    let set_level = move |e: leptos::ev::Event| {
        let Ok(v) = event_target_value(&e).parse::<f32>() else {
            return;
        };
        let v = (v / 100.0).clamp(0.0, 1.0);
        hand.set(Some(v));
        queued.set_value(Some(v));
        if sending.get_value() {
            return;
        }
        let Some(s) = session.0.get_value() else {
            return;
        };
        sending.set_value(true);
        let channel_id = id.get_value();
        spawn_local(async move {
            while let Some(Some(v)) = queued.try_update_value(Option::take) {
                let action = Action::ChannelSetValue {
                    channel_id: channel_id.clone(),
                    value: Finite32::new(v).unwrap_or(Finite32::ZERO),
                    fade_ms: None,
                };
                if let Err(error) = s.control.call(Call::Action { action }).await {
                    studio.report(&error);
                }
            }
            sending.try_set_value(false);
        });
    };
    let let_go = move |_| hand.set(None);
    let rename = move |e: leptos::ev::Event| {
        let text = event_target_value(&e);
        set(&|c| c.name = text.trim().to_owned());
    };

    // Inputs.
    let learning = RwSignal::new(false);
    let reading = RwSignal::new(false);
    let learn = move |_| {
        if learning.get_untracked() {
            learning.set(false);
            return;
        }
        let Some(s) = session.0.get_value() else {
            return;
        };
        learning.set(true);
        spawn_local(async move {
            let Some(baseline) = controls(&s).await else {
                learning.try_set(false);
                studio
                    .problem
                    .set(Some("This engine reads no MIDI controls.".into()));
                return;
            };
            listen(s, studio, learning, id, baseline, 100);
        });
    };
    let inputs = move || {
        let channel = info()?;
        let rows = studio.collection.with(|c| {
            let c = c.as_ref()?;
            Some(
                channel
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(n, input)| {
                        let text = input_name(c, input);
                        let remove = move |_| {
                            set(&|ch| {
                                if n < ch.inputs.len() {
                                    ch.inputs.remove(n);
                                }
                            });
                        };
                        view! {
                            <li class="chan-row">
                                <span class="chan-row-name">{text}</span>
                                <button type="button" class="chan-x" title="Remove this input" aria-label="Remove this input" on:click=remove>
                                    "×"
                                </button>
                            </li>
                        }
                    })
                    .collect_view(),
            )
        })?;
        Some(view! { <ul class="chan-rows">{rows}</ul> })
    };
    let read_channel = move |other: String| {
        reading.set(false);
        if other == id.get_value() {
            return;
        }
        set(&|c| {
            c.inputs.push(MotionKind::Channel {
                channel_id: other.clone(),
                smoothing_ms: 0,
                curve: MotionCurve::Linear,
                from: Finite32::ZERO,
                to: Finite32::ONE,
            })
        });
    };
    let make_read = move |name: String| {
        let Some(s) = session.0.get_value() else {
            return;
        };
        reading.set(false);
        let Some(mut channel) = untrack(info) else {
            return;
        };
        let other = studio.collection.with_untracked(|c| {
            new_channel(
                &name,
                channel.scene_id.clone(),
                0.0,
                c.as_ref().map_or(&[][..], |c| &c.channels[..]),
            )
        });
        channel.inputs.push(MotionKind::Channel {
            channel_id: other.channel_id.clone(),
            smoothing_ms: 0,
            curve: MotionCurve::Linear,
            from: Finite32::ZERO,
            to: Finite32::ONE,
        });
        studio.apply(
            &s,
            vec![
                Edit::ChannelSet { channel: other },
                Edit::ChannelSet { channel },
            ],
        );
    };

    // Shaping.
    let ms = move |get: fn(&ChannelInfo) -> u32| {
        Signal::derive(move || info().map(|c| f64::from(get(&c))))
    };
    let pct = move |get: fn(&ChannelInfo) -> f32| {
        Signal::derive(move || info().map(|c| f64::from(get(&c)) * 100.0))
    };
    let curve = move || info().map_or(MotionCurve::Linear, |c| c.curve);
    let combine = move || info().map_or(ChannelCombine::Latest, |c| c.combine);
    let set_curve = move |e: leptos::ev::Event| {
        let v = match event_target_value(&e).as_str() {
            "ease_in" => MotionCurve::EaseIn,
            "ease_out" => MotionCurve::EaseOut,
            "ease_in_out" => MotionCurve::EaseInOut,
            _ => MotionCurve::Linear,
        };
        set(&|c| c.curve = v);
    };
    let set_combine = move |e: leptos::ev::Event| {
        let v = match event_target_value(&e).as_str() {
            "max" => ChannelCombine::Max,
            "min" => ChannelCombine::Min,
            "sum" => ChannelCombine::Sum,
            "product" => ChannelCombine::Product,
            "average" => ChannelCombine::Average,
            _ => ChannelCombine::Latest,
        };
        set(&|c| c.combine = v);
    };
    let frac = |v: f64| Finite32::new((v / 100.0) as f32).unwrap_or(Finite32::ZERO);

    // What follows it, and where it has lanes.
    let drives = move || {
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            let me = id.get_value();
            let rows: Vec<_> = c
                .value_bindings
                .iter()
                .filter(|b| matches!(&b.input, MotionKind::Channel { channel_id, .. } if *channel_id == me))
                .map(|b| {
                    let text = target_name(c, &b.target);
                    let range = match &b.input {
                        MotionKind::Channel { from, to, .. } => {
                            format!("{} → {}", trim(from.get()), trim(to.get()))
                        }
                        _ => String::new(),
                    };
                    let binding_id = b.binding_id.clone();
                    let remove = move |_| {
                        if let Some(s) = session.0.get_value() {
                            studio.apply(
                                &s,
                                vec![Edit::BindingClearValue {
                                    binding_id: binding_id.clone(),
                                }],
                            );
                        }
                    };
                    view! {
                        <li class="chan-row">
                            <span class="chan-row-name">{text}</span>
                            <span class="hint mono">{range}</span>
                            <button type="button" class="chan-x" title="Stop following this channel" aria-label="Stop following this channel" on:click=remove>
                                "×"
                            </button>
                        </li>
                    }
                })
                .collect();
            let count = rows.len();
            Some(view! {
                <div class="section-head chan-head">
                    <span class="eyebrow">"Drives"</span>
                    <span class="hint">{format!("{count} setting{}", if count == 1 { "" } else { "s" })}</span>
                </div>
                {(count == 0).then(|| view! {
                    <p class="hint chan-help">"Nothing follows it yet. Use the control button beside any live setting: Follow channel."</p>
                })}
                <ul class="chan-rows">{rows}</ul>
            })
        })
    };
    let lanes = move || {
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            let me = id.get_value();
            let rows: Vec<_> = c
                .scenes
                .iter()
                .flat_map(|s| &s.timelines)
                .filter(|t| t.lanes.iter().any(|l| l.target.channel == me))
                .map(|t| {
                    let tid = t.timeline_id.clone();
                    let name = if t.name.is_empty() {
                        t.timeline_id.to_string()
                    } else {
                        t.name.clone()
                    };
                    let points: usize = t
                        .lanes
                        .iter()
                        .filter(|l| l.target.channel == me)
                        .map(|l| l.points.len())
                        .sum();
                    let open = move |_| {
                        studio.sequence.set(Some(tid.clone()));
                        if let Some(dock) = use_context::<DockState>() {
                            dock.0.update(|d| d.show(PanelId::Sequence));
                        }
                    };
                    view! {
                        <li class="chan-row link" on:click=open>
                            <Icon name="sequence" size=13 />
                            <span class="chan-row-name">{name}</span>
                            <span class="hint">{format!("{points} point{}", if points == 1 { "" } else { "s" })}</span>
                        </li>
                    }
                })
                .collect();
            (!rows.is_empty()).then(|| {
                view! {
                    <div class="section-head chan-head"><span class="eyebrow">"In sequences"</span></div>
                    <ul class="chan-rows">{rows}</ul>
                }
            })
        })
    };
    let delete = move |_| {
        if let Some(s) = session.0.get_value() {
            studio.channel.set(None);
            studio.apply(
                &s,
                vec![Edit::ChannelDelete {
                    channel_id: id.get_value(),
                }],
            );
        }
    };

    view! {
        <div class="chan-card">
            <div class="chan-title">
                <span class="chan-dot" class:off=move || !on()></span>
                <input
                    class="chan-name"
                    aria-label="Channel name"
                    prop:value=name
                    on:change=rename
                />
                <span class="hint">{move || format!("channel · {}", place())}</span>
                <button
                    type="button"
                    role="switch"
                    class="switch"
                    class:on=on
                    aria-label="Channel on"
                    aria-checked=move || on().to_string()
                    title="Switch the channel on or off: off, it fades to its When off value"
                    on:click=toggle
                >
                    <span class="switch-knob"></span>
                </button>
            </div>
            <div class="chan-meter">
                <input
                    type="range"
                    min="0"
                    max="100"
                    step="1"
                    aria-label="Set the value"
                    title="Set the value by hand"
                    prop:value=move || format!("{:.0}", hand.get().unwrap_or_else(level) * 100.0)
                    on:input=set_level
                    on:change=let_go
                />
                <span class="mono chan-level">{move || format!("{:.0} %", level() * 100.0)}</span>
            </div>
            <p class="hint chan-driver">{driver}</p>

            <div class="section-head chan-head">
                <span class="eyebrow">"Driven by"</span>
                <span class="chan-adds">
                    <button type="button" class="chan-add" class:learning=learning on:click=learn>
                        {move || if learning.get() { "Move a control…" } else { "+ Learn MIDI" }}
                    </button>
                    <button type="button" class="chan-add" on:click=move |_| reading.update(|r| *r = !*r)>
                        "+ Channel"
                    </button>
                </span>
            </div>
            {move || reading.get().then(|| view! {
                <div class="chan-popover">
                    <ChannelSearch placeholder="Read another channel" pick=read_channel make=make_read values=values />
                </div>
            })}
            {inputs}
            <div class="chan-key">
                <crate::panels::inspector::HotkeyRow
                    label="Key"
                    action=Action::ChannelSwitch { channel_id: id.get_value(), to: ChannelSwitch::Toggle }
                />
            </div>

            <div class="section-head chan-head"><span class="eyebrow">"Shaping"</span></div>
            <div class="chan-grid">
                <span>"Fade up"</span>
                <NumberField named=Signal::derive(|| "Fade up".to_owned()) unit="ms" value=ms(|c| c.rise_ms) step=50.0 decimals=0 min=0.0 max=60000.0
                    send=move |v| set(&|c| c.rise_ms = v as u32) />
                <span>"Fade down"</span>
                <NumberField named=Signal::derive(|| "Fade down".to_owned()) unit="ms" value=ms(|c| c.fall_ms) step=50.0 decimals=0 min=0.0 max=60000.0
                    send=move |v| set(&|c| c.fall_ms = v as u32) />
                <span>"Smooth"</span>
                <NumberField named=Signal::derive(|| "Smooth".to_owned()) unit="ms" value=ms(|c| c.average_ms) step=10.0 decimals=0 min=0.0 max=10000.0
                    send=move |v| set(&|c| c.average_ms = v as u32) />
                <span>"Curve"</span>
                <select class="chan-select" aria-label="Curve" on:change=set_curve>
                    {[("linear", "Linear", MotionCurve::Linear), ("ease_in", "Ease in", MotionCurve::EaseIn), ("ease_out", "Ease out", MotionCurve::EaseOut), ("ease_in_out", "Ease in and out", MotionCurve::EaseInOut)]
                        .into_iter()
                        .map(|(v, label, c)| view! { <option value=v selected=move || curve() == c>{label}</option> })
                        .collect_view()}
                </select>
                <span>"Combine"</span>
                <select class="chan-select" aria-label="Combine" on:change=set_combine>
                    {[("latest", "Latest moved", ChannelCombine::Latest), ("max", "Highest", ChannelCombine::Max), ("min", "Lowest", ChannelCombine::Min), ("sum", "Sum", ChannelCombine::Sum), ("product", "Multiply", ChannelCombine::Product), ("average", "Average", ChannelCombine::Average)]
                        .into_iter()
                        .map(|(v, label, c)| view! { <option value=v selected=move || combine() == c>{label}</option> })
                        .collect_view()}
                </select>
                <span>"Default"</span>
                <NumberField named=Signal::derive(|| "Default".to_owned()) unit="%" value=pct(|c| c.default.get()) step=1.0 decimals=0 min=0.0 max=100.0
                    send=move |v| set(&|c| c.default = frac(v)) />
                <span>"When off"</span>
                <NumberField named=Signal::derive(|| "When off".to_owned()) unit="%" value=pct(|c| c.off_value.get()) step=1.0 decimals=0 min=0.0 max=100.0
                    send=move |v| set(&|c| c.off_value = frac(v)) />
            </div>

            {drives}
            {lanes}

            <button type="button" class="menuitem danger chan-delete" on:click=delete>
                "Delete channel"
            </button>
        </div>
    }
}

/// A number with no trailing zeros, as a range end is shown.
fn trim(v: f32) -> String {
    crate::panels::number::format_number(f64::from(v), 2, false)
}

/// Reads the controls after a poll, and adds the first that moved since `baseline` to
/// channel `id` as an input, or reads again, `tries` more times, while `learning` holds.
fn listen(
    session: crate::session::Session,
    studio: Studio,
    learning: RwSignal<bool>,
    id: StoredValue<String>,
    baseline: Vec<MidiControlInfo>,
    tries: u32,
) {
    set_timeout(
        move || {
            spawn_local(async move {
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                let now = controls(&session).await.unwrap_or_default();
                if learning.try_get_untracked() != Some(true) {
                    return;
                }
                if let Some(control) = moved(&baseline, &now) {
                    learning.try_set(false);
                    let Some(channel_id) = id.try_get_value() else {
                        return;
                    };
                    let channel = studio.collection.with_untracked(|c| {
                        c.as_ref().and_then(|c| c.channel(&channel_id).cloned())
                    });
                    if let Some(mut channel) = channel {
                        channel.inputs.push(midi_input(&control));
                        studio.apply(&session, vec![Edit::ChannelSet { channel }]);
                    }
                } else if tries == 0 {
                    learning.try_set(false);
                } else {
                    listen(session, studio, learning, id, baseline, tries - 1);
                }
            });
        },
        Duration::from_millis(150),
    );
}

/// A channel input reading `control` over the channel's whole 0 to 1.
pub fn midi_input(control: &MidiControlInfo) -> MotionKind {
    MotionKind::Midi {
        control: MotionMidiControl::Cc,
        controller: Some(control.controller),
        channel: Some(control.channel),
        input: Some(control.input.clone()),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_channel_id_is_the_name_in_lower_case_and_free() {
        let taken = vec![ChannelInfo::new("key_light")];
        assert_eq!(channel_id_for("Stage power", &[]), "stage_power");
        assert_eq!(channel_id_for("Key light", &taken), "key_light_2");
        assert_eq!(channel_id_for("  ✨ ", &[]), "channel");
        let made = new_channel("Key light", None, 1.4, &taken);
        assert_eq!(made.channel_id, "key_light_2");
        assert_eq!(made.name, "Key light");
        assert_eq!(made.default.get(), 1.0);
    }

    #[test]
    fn a_search_finds_by_name_and_puts_the_scene_first() {
        let mut a = ChannelInfo::new("a");
        a.name = "Key lights".into();
        a.scene_id = Some(SceneId::new("s"));
        let mut b = ChannelInfo::new("b");
        b.name = "Keyboard cam".into();
        let mut c = ChannelInfo::new("c");
        c.name = "Stage power".into();
        let all = vec![b, c, a];
        let (here, rest) = search(&all, "key", Some(&SceneId::new("s")));
        assert_eq!(
            here.iter()
                .map(|c| c.channel_id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert_eq!(
            rest.iter()
                .map(|c| c.channel_id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        let (here, rest) = search(&all, "", None);
        assert!(here.is_empty());
        assert_eq!(rest.len(), 3);
    }
}
