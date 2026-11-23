//! The Mappings panel (UI-FEATURES MP-01 and MP-02): everything that is mapped, in one
//! dock panel with two tabs.
//!
//! - **Mappings**: every channel with what sets it (its inputs, the keys and MIDI that
//!   switch or set it, the sequences with a lane on it) and everything that follows it or
//!   that it scales, then every source read directly (a MIDI control or a field of a
//!   sender's live data) with all its readers. A source with several targets lists them
//!   under one another. Each target's range, smoothing and curve are edited in place, and
//!   × removes it. A search, a scene, a kind of source and "Find by moving a control"
//!   narrow the list. Clicking a channel opens it in the Channels panel.
//! - **Sources**: each JTunnel sender's Mods 1 to 32 with their live values and what each
//!   drives, then each MIDI input with its last moved control. A free Mod's Map… makes a
//!   channel that reads it and opens it; a mapped one shows its rows in Mappings.
//!
//! The rows come from [`crate::mapping_index`]; live values from `channel.values`,
//! `midi.last_controls`, `data.list` and `data.get`, read about ten times a second while
//! the panel is open ([`feed`]).
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#mappings

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::mapping_index::{self, Filter, Mode, Reader, Row, Setter, SourceKey, SourceKind};
use crate::panels::channels::live_values;
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio, query};
use crate::workspace::DockState;
use jstream_control::protocol::{
    ChannelValueInfo, Edit, Finite32, MidiControlInfo, MotionCurve, MotionKind, Query, QueryResult,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::HashMap;
use std::time::Duration;

/// How often an open panel reads the live sources.
const POLL: Duration = Duration::from_millis(100);

/// How many Mods a JTunnel sender has (ADR-0181).
const MOD_SLOTS: u8 = 32;

/// How far a Mod must move for "Find by moving a control" to take it.
const MOVED: f32 = 0.02;

/// One JTunnel sender's live data, as the panel shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sender {
    /// The sender's name, as `data.list` gives it.
    pub input: String,
    /// Its keys with a current message.
    pub keys: Vec<String>,
    /// Its Mods' values, 0 to 1, when it sends `params`.
    pub mods: Vec<f32>,
    /// Its Mods' names, as `params` carries them (empty for none).
    pub names: Vec<String>,
}

/// The live sources: each sender's data, and each MIDI input's last control.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feed {
    /// The senders with live data, by name.
    pub senders: Vec<Sender>,
    /// Each MIDI input's latest control change.
    pub controls: Vec<MidiControlInfo>,
}

impl Feed {
    /// A source's value now, 0 to 1, if it can be read: a MIDI control change, a Mod, or
    /// a channel (from `channels`).
    pub fn value(
        &self,
        key: &SourceKey,
        channels: &HashMap<String, ChannelValueInfo>,
    ) -> Option<f32> {
        match key {
            SourceKey::Midi {
                control: "cc",
                number: Some(n),
                channel,
                input,
            } => self
                .controls
                .iter()
                .filter(|c| {
                    c.controller == *n
                        && channel.is_none_or(|ch| ch == c.channel)
                        && input.as_ref().is_none_or(|i| *i == c.input)
                })
                .max_by_key(|c| c.count)
                .map(|c| f32::from(c.value) / 127.0),
            SourceKey::Data { input, .. } => {
                let slot = mapping_index::mod_slot(key)?;
                self.senders
                    .iter()
                    .filter(|s| input.as_ref().is_none_or(|i| *i == s.input))
                    .find_map(|s| s.mods.get(usize::from(slot - 1)).copied())
            }
            SourceKey::Channel(id) => channels.get(id).map(|c| c.value.get()),
            SourceKey::Midi { .. } => None,
        }
    }

    /// The first source that moved since `before`: a MIDI control that sent again, or a
    /// Mod that moved by more than [`MOVED`].
    pub fn moved(&self, before: &Feed) -> Option<SourceKey> {
        if let Some(c) = crate::panels::learn::moved(&before.controls, &self.controls) {
            return Some(SourceKey::Midi {
                control: "cc",
                number: Some(c.controller),
                channel: Some(c.channel),
                input: Some(c.input),
            });
        }
        for sender in &self.senders {
            let old = before.senders.iter().find(|s| s.input == sender.input);
            for (n, v) in sender.mods.iter().enumerate() {
                let was = old.and_then(|o| o.mods.get(n)).copied();
                if was.is_some_and(|was| (v - was).abs() > MOVED) {
                    return Some(mod_key(Some(&sender.input), n as u8 + 1));
                }
            }
        }
        None
    }
}

/// The source key of Mod `slot` (1 to 32) of `sender`, or of any sender.
pub fn mod_key(sender: Option<&str>, slot: u8) -> SourceKey {
    SourceKey::Data {
        input: sender.map(str::to_owned),
        key: "params".into(),
        field: format!("values.{}", slot.saturating_sub(1)),
    }
}

/// A reader of Mod `slot` of `sender` over a channel's whole 0 to 1.
pub fn mod_reader(sender: &str, slot: u8) -> MotionKind {
    MotionKind::Data {
        input: Some(sender.to_owned()),
        key: "params".into(),
        field: format!("values.{}", slot.saturating_sub(1)),
        range: [Finite32::ZERO, Finite32::ONE],
        states: Default::default(),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
        color_from: None,
        palette: Default::default(),
    }
}

/// A sender's `params` message read into its Mods' values and names.
pub fn read_params(value: &serde_json::Value) -> (Vec<f32>, Vec<String>) {
    let mods = value
        .get("values")
        .and_then(serde_json::Value::as_array)
        .map(|v| {
            v.iter()
                .map(|n| n.as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32)
                .collect()
        })
        .unwrap_or_default();
    let names = value
        .get("names")
        .and_then(serde_json::Value::as_array)
        .map(|v| {
            v.iter()
                .map(|n| n.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();
    (mods, names)
}

/// Reads the live sources once.
async fn read(session: &crate::session::Session) -> Feed {
    let mut feed = Feed::default();
    if let Ok(QueryResult::MidiLastControls { controls }) =
        query(session, Query::MidiLastControls).await
    {
        feed.controls = controls;
    }
    if let Ok(QueryResult::DataList { inputs }) = query(session, Query::DataList).await {
        for info in inputs {
            let mut sender = Sender {
                input: info.input.clone(),
                keys: info.remote_keys.clone(),
                ..Sender::default()
            };
            if info.remote_keys.iter().any(|k| k == "params")
                && let Ok(QueryResult::DataGet {
                    remote_value: Some(value),
                }) = query(
                    session,
                    Query::DataGet {
                        input: info.input,
                        key: "params".into(),
                    },
                )
                .await
            {
                (sender.mods, sender.names) = read_params(&value);
            }
            feed.senders.push(sender);
        }
    }
    feed
}

/// The live sources, read every [`POLL`] for as long as the calling component lives.
pub fn feed() -> RwSignal<Feed> {
    let feed = RwSignal::new(Feed::default());
    if let Some(session) = use_context::<SessionContext>() {
        poll(session, feed);
    }
    feed
}

fn poll(session: SessionContext, feed: RwSignal<Feed>) {
    set_timeout(
        move || {
            spawn_local(async move {
                let Some(s) = session.0.try_get_value().flatten() else {
                    return;
                };
                if feed.try_get_untracked().is_none() {
                    return;
                }
                let now = read(&s).await;
                let changed = feed.try_with_untracked(|f| *f != now);
                match changed {
                    None => return,
                    Some(true) => {
                        if feed.try_set(now).is_some() {
                            return;
                        }
                    }
                    Some(false) => {}
                }
                poll(session, feed);
            });
        },
        POLL,
    );
}

/// The source armed for mapping mode: what a drag on a parameter would map.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmedSource {
    /// The source.
    pub key: SourceKey,
    /// A reader of it over 0 to 1, which a new mapping starts from.
    pub reader: MotionKind,
    /// Its name, as the bar and the Sources tab show it.
    pub label: String,
}

/// The armed source, shared by everything that arms one or maps to it.
#[derive(Clone, Copy)]
pub struct MappingMode(pub RwSignal<Option<ArmedSource>>);

/// The shell's mapping mode, or one for this panel when the shell has none.
pub fn mapping_mode() -> MappingMode {
    use_context::<MappingMode>().unwrap_or_else(|| {
        let mode = MappingMode(RwSignal::new(None));
        provide_context(mode);
        mode
    })
}

/// The panel's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Mappings,
    Sources,
}

/// The Mappings panel.
#[component]
pub fn Mappings() -> impl IntoView {
    let tab = RwSignal::new(Tab::Mappings);
    let filter = RwSignal::new(Filter::default());
    let feed = feed();
    let values = live_values();
    let mode = mapping_mode();
    let tab_button = move |t: Tab, label: &'static str| {
        let on = move || tab.get() == t;
        view! {
            <button
                type="button"
                role="tab"
                aria-selected=move || on().to_string()
                tabindex=move || if on() { "0" } else { "-1" }
                on:click=move |_| tab.set(t)
            >
                {label}
            </button>
        }
    };
    view! {
        <section class="panel map-panel">
            <div class="segmented map-tabs" role="tablist" aria-label="Mappings">
                {tab_button(Tab::Mappings, "Mappings")}
                {tab_button(Tab::Sources, "Sources")}
            </div>
            {move || match tab.get() {
                Tab::Mappings => view! { <MappingsTab filter=filter feed=feed values=values mode=mode /> }.into_any(),
                Tab::Sources => view! { <SourcesTab filter=filter tab=tab feed=feed values=values mode=mode /> }.into_any(),
            }}
        </section>
    }
}

/// What a table group's source is called: a named Mod as "Camera roll (Mod 20)", a
/// channel by its name, a direct source as [`mapping_index::source_name`] names it.
pub fn group_label(collection: &Collection, row: &Row) -> String {
    match row {
        Row::Channel { channel_id, .. } => {
            let name = collection.channel_name(channel_id);
            match collection
                .channel(channel_id)
                .and_then(mapping_index::named_mod)
                .and_then(|k| mapping_index::mod_slot(&k))
            {
                Some(n) => format!("{name} (Mod {n})"),
                None => name,
            }
        }
        Row::Direct { source, .. } => mapping_index::source_name(collection, source),
    }
}

/// A reader of a channel over 0 to 1, as a new mapping from it starts.
fn channel_reader(channel_id: &str) -> MotionKind {
    MotionKind::Channel {
        channel_id: channel_id.to_owned(),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

/// What arming a table group arms: its channel, or its direct source.
fn group_armed(collection: &Collection, row: &Row) -> Option<ArmedSource> {
    let label = group_label(collection, row);
    match row {
        Row::Channel { channel_id, .. } => Some(ArmedSource {
            key: SourceKey::Channel(channel_id.clone()),
            reader: channel_reader(channel_id),
            label,
        }),
        Row::Direct { source, readers } => Some(ArmedSource {
            key: source.clone(),
            reader: mapping_index::with_range(&readers.first()?.input, 0.0, 1.0)?,
            label,
        }),
    }
}

/// The Mappings tab: the filters, then the table.
#[component]
fn MappingsTab(
    filter: RwSignal<Filter>,
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: MappingMode,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let finding = RwSignal::new(None::<Feed>);
    let on_program = RwSignal::new(false);
    // "Find by moving a control": the first source that moves after the click.
    Effect::new(move |_| {
        let now = feed.get();
        let Some(before) = finding.get_untracked() else {
            return;
        };
        if let Some(key) = now.moved(&before) {
            finding.set(None);
            filter.update(|f| f.source = Some(key));
        }
    });
    // "On program now" follows the program scene.
    Effect::new(move |_| {
        let scene = on_program.get().then(|| studio.program.get()).flatten();
        filter.update(|f| f.scene = scene);
    });
    let find = move |_| {
        if finding.get_untracked().is_some() {
            finding.set(None);
        } else {
            finding.set(Some(feed.get_untracked()));
        }
    };
    let set_kind = move |e: leptos::ev::Event| {
        let kind = match event_target_value(&e).as_str() {
            "midi" => SourceKind::Midi,
            "data" => SourceKind::Data,
            "channel" => SourceKind::Channel,
            "key" => SourceKind::Key,
            _ => SourceKind::All,
        };
        filter.update(|f| f.kind = kind);
    };
    let rows = Memo::new(move |_| {
        studio
            .collection
            .with(|c| c.as_ref().map(mapping_index::rows).unwrap_or_default())
    });
    let shown = move || {
        let f = filter.get();
        studio.collection.with(|c| {
            let Some(c) = c else {
                return Vec::new();
            };
            rows.get()
                .into_iter()
                .filter(|r| mapping_index::shows(c, r, &f))
                .collect::<Vec<_>>()
        })
    };
    let caught = move || {
        let key = filter.with(|f| f.source.clone())?;
        let name = studio.collection.with(|c| {
            c.as_ref()
                .map(|c| mapping_index::source_name(c, &key))
                .unwrap_or_default()
        });
        Some(view! {
            <span class="map-caught">
                {format!("Moved: {name}")}
                <button type="button" class="chan-x" aria-label="Show every source" title="Show every source"
                    on:click=move |_| filter.update(|f| f.source = None)>"×"</button>
            </span>
        })
    };
    let body = move || {
        let list = shown();
        if list.is_empty() {
            let empty = rows.with(Vec::is_empty);
            return view! {
                <p class="hint chan-help">
                    {if empty {
                        "Nothing is mapped yet. Name or arm a Mod in Sources, or use the control button beside any live setting."
                    } else {
                        "No mapping matches."
                    }}
                </p>
            }
            .into_any();
        }
        list.into_iter()
            .map(|row| view! { <MapGroup row=row feed=feed values=values mode=mode /> })
            .collect_view()
            .into_any()
    };
    view! {
        <div class="map-tools">
            <div class="tree-search map-search">
                <Icon name="search" size=13 />
                <input
                    type="search"
                    placeholder="Search a source or target"
                    aria-label="Search mappings"
                    prop:value=move || filter.with(|f| f.text.clone())
                    on:input=move |e| filter.update(|f| f.text = event_target_value(&e))
                />
            </div>
            <button type="button" class="map-toggle" aria-pressed=move || on_program.get().to_string()
                title="Only what drives something in the program scene"
                on:click=move |_| on_program.update(|on| *on = !*on)>
                "On program now"
            </button>
            <select class="chan-select" aria-label="Source" on:change=set_kind>
                {[("all", "Source: all", SourceKind::All), ("midi", "MIDI", SourceKind::Midi), ("data", "JTunnel and data", SourceKind::Data), ("channel", "Channels", SourceKind::Channel), ("key", "Keys", SourceKind::Key)]
                    .into_iter()
                    .map(|(v, label, k)| view! { <option value=v selected=move || filter.with(|f| f.kind == k)>{label}</option> })
                    .collect_view()}
            </select>
            <button type="button" class="chan-add map-find" class:learning=move || finding.with(Option::is_some) on:click=find>
                {move || if finding.with(Option::is_some) { "Move a control…" } else { "Find by moving a control" }}
            </button>
            {caught}
        </div>
        <div class="map-table" role="table" aria-label="Mappings">
            <div class="map-head" role="row">
                <span role="columnheader">"Source"</span>
                <span role="columnheader">"Live"</span>
                <span class="map-head-targets">
                    <span role="columnheader">"Target"</span>
                    <span role="columnheader">"Amount"</span>
                    <span role="columnheader">"Mode"</span>
                    <span role="columnheader">"Smooth"</span>
                    <span role="columnheader">"Curve"</span>
                    <span role="columnheader" aria-label="Bypass and remove"></span>
                </span>
            </div>
            {body}
        </div>
    }
}

/// A live level as a bar and its value.
fn live_bar(level: Option<f32>) -> impl IntoView {
    let width = level.map_or(0.0, |v| v.clamp(0.0, 1.0) * 100.0);
    let text = level.map_or_else(|| "not seen".to_owned(), |v| format!("{v:.2}"));
    view! {
        <span class="map-live">
            <span class="meter-bar"><span class="map-fill" style:width=format!("{width:.0}%")></span></span>
            <span class="mono hint" class:map-unseen=level.is_none()>{text}</span>
        </span>
    }
}

/// The CSS class of a source's chip, by its kind.
fn kind_class(key: &SourceKey) -> &'static str {
    match key.kind() {
        SourceKind::Midi => "midi",
        SourceKind::Data => "data",
        SourceKind::Channel => "channel",
        _ => "",
    }
}

/// What else sets a channel, as its chip says it.
fn setter_text(collection: &Collection, setter: &Setter) -> String {
    match setter {
        Setter::Key(chord) => format!("Key {}", crate::hotkeys::label(chord)),
        Setter::Midi(trigger) => mapping_index::midi_text(trigger),
        Setter::Sequence(id) => {
            let name = collection.timeline(id).map_or_else(
                || id.to_string(),
                |(_, t)| crate::panels::sequence::timeline_name(t),
            );
            format!("Sequence “{name}”")
        }
    }
}

/// One group of the table: a source with every target it drives, and "+ Target…".
#[component]
fn MapGroup(
    row: Row,
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: MappingMode,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let dock = use_context::<DockState>();
    let (label, armed) = studio.collection.with_untracked(|c| {
        c.as_ref().map_or_else(
            || (String::new(), None),
            |c| (group_label(c, &row), group_armed(c, &row)),
        )
    });
    let chip = |text: String, kind: &'static str| {
        view! { <span class=format!("map-chip small {kind}")>{text}</span> }
    };
    let (main, subs, level, targets) = match row {
        Row::Channel {
            channel_id,
            inputs,
            setters,
            followers,
        } => {
            let named = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .and_then(|c| c.channel(&channel_id))
                    .and_then(mapping_index::named_mod)
                    .is_some()
            });
            // A named Mod is shown once, as its name: its one input is the Mod itself.
            let mut subs: Vec<AnyView> = Vec::new();
            if !named {
                studio.collection.with_untracked(|c| {
                    let Some(c) = c else {
                        return;
                    };
                    subs.extend(
                        inputs
                            .iter()
                            .filter_map(|r| mapping_index::source_key(&r.input))
                            .map(|k| {
                                chip(mapping_index::source_name(c, &k), kind_class(&k)).into_any()
                            }),
                    );
                    subs.extend(setters.iter().map(|s| {
                        let kind = if matches!(s, Setter::Key(_)) {
                            "key"
                        } else {
                            "midi"
                        };
                        chip(setter_text(c, s), kind).into_any()
                    }));
                });
            }
            let id = channel_id.clone();
            let level = Signal::derive(move || values.with(|v| v.get(&id).map(|c| c.value.get())));
            let open = move |_| {
                studio.channel.set(Some(channel_id.clone()));
                if let Some(dock) = dock {
                    dock.0.update(|d| d.show(PanelId::Channels));
                }
            };
            let class = if named {
                "map-chip data link"
            } else {
                "map-chip channel link"
            };
            let main = view! {
                <button type="button" class=class title="Open this channel" on:click=open>
                    {label.clone()}
                </button>
            }
            .into_any();
            (main, subs, level, followers)
        }
        Row::Direct { source, readers } => {
            let main = view! { <span class=format!("map-chip {}", kind_class(&source))>{label.clone()}</span> }
                .into_any();
            let key = source.clone();
            let level = Signal::derive(move || {
                let channels = values.get();
                feed.with(|f| f.value(&key, &channels))
            });
            (main, Vec::new(), level, readers)
        }
    };
    let target_rows = targets
        .into_iter()
        .map(|r| view! { <TargetRow reader=r /> })
        .collect_view();
    let is_armed = {
        let key = armed.as_ref().map(|a| a.key.clone());
        Signal::derive(move || mode.0.with(|m| m.as_ref().map(|a| &a.key) == key.as_ref()))
    };
    let arm = move |_| {
        let next = armed.clone();
        mode.0.update(|m| {
            *m = if m.as_ref().map(|a| &a.key) == next.as_ref().map(|a| &a.key) {
                None
            } else {
                next
            }
        });
    };
    view! {
        <div class="map-group" role="rowgroup" class:armed=is_armed>
            <div class="map-source">
                {main}
                {(!subs.is_empty()).then(|| view! { <div class="map-subs">{subs}</div> })}
            </div>
            <div class="map-level">{move || live_bar(level.get())}</div>
            <div class="map-targets">
                {target_rows}
                <button type="button" class="map-add" aria-pressed=move || is_armed.get().to_string()
                    title="Arm this source, then drag on any parameter to map it"
                    on:click=arm>
                    {move || if is_armed.get() { "Armed: drag on a parameter…" } else { "+ Target…" }}
                </button>
            </div>
        </div>
    }
}

/// One target of a group: its name, amount, mode, smoothing, curve and bypass, edited in
/// place, and ×.
#[component]
fn TargetRow(reader: Reader) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let holder = StoredValue::new(reader.holder.clone());
    let mode = mapping_index::mode_of(&reader.holder);
    let unit = mapping_index::unit(&reader.target);
    let (name, note) = studio.collection.with_untracked(|c| {
        c.as_ref().map_or_else(
            || (String::new(), None),
            |c| mapping_index::target_name(c, &reader.target),
        )
    });
    let scaled = reader.amount.clone().filter(|_| !reader.reads_source());
    let note = match (&reader.target, scaled) {
        (_, Some(amount)) => {
            let by = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .map(|c| c.channel_name(&amount))
                    .unwrap_or_default()
            });
            Some(format!("scaled by {by}"))
        }
        _ => note.map(str::to_owned),
    };
    // The reader as the collection holds it now.
    let kind = Signal::derive(move || {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| holder.with_value(|h| mapping_index::reader_at(c, h)))
                .map(|r| r.input)
        })
    });
    let send = move |edit: Option<Edit>| {
        let (Some(edit), Some(s)) = (edit, session.0.get_value()) else {
            return false;
        };
        studio.apply(&s, vec![edit]);
        true
    };
    let write = move |next: Option<MotionKind>| {
        let edit = next.and_then(|next| {
            studio.collection.with_untracked(|c| {
                c.as_ref()
                    .and_then(|c| holder.with_value(|h| mapping_index::write(c, h, next)))
            })
        });
        send(edit)
    };
    let range = move || kind.get().as_ref().and_then(mapping_index::range_of);
    let amount_field = match mode {
        Mode::Offset => {
            let value = Signal::derive(move || {
                range().map(|(from, to)| {
                    if mapping_index::centred(from, to) {
                        f64::from(to.abs())
                    } else {
                        f64::from(to - from)
                    }
                })
            });
            let centred = move || range().is_some_and(|(f, t)| mapping_index::centred(f, t));
            let set = move |v: f64| {
                let k = kind.get_untracked();
                write(
                    k.as_ref()
                        .and_then(|k| mapping_index::with_amount(k, v as f32)),
                )
            };
            view! {
                <span class="map-amount" role="cell" title=move || if centred() { "Centred: the stored value sits at the source's middle" } else { "How far it moves the target across the source's travel" }>
                    {move || centred().then_some("±")}
                    <NumberField named=Signal::derive(|| "Amount".to_owned()) unit=unit value=value step=0.1 decimals=2 send=set />
                </span>
            }
            .into_any()
        }
        Mode::Override | Mode::Input => {
            let end = move |pick: fn((f32, f32)) -> f32| {
                Signal::derive(move || range().map(|r| f64::from(pick(r))))
            };
            let send_from = move |v: f64| {
                let k = kind.get_untracked();
                write(k.as_ref().and_then(|k| {
                    let (_, to) = mapping_index::range_of(k)?;
                    mapping_index::with_range(k, v as f32, to)
                }))
            };
            let send_to = move |v: f64| {
                let k = kind.get_untracked();
                write(k.as_ref().and_then(|k| {
                    let (from, _) = mapping_index::range_of(k)?;
                    mapping_index::with_range(k, from, v as f32)
                }))
            };
            view! {
                <span class="map-amount map-range" role="cell" title="The value at the source's 0 and at its 1">
                    <NumberField named=Signal::derive(|| "From".to_owned()) value=end(|r| r.0) step=0.01 decimals=2 send=send_from />
                    <span class="hint">"→"</span>
                    <NumberField named=Signal::derive(|| "To".to_owned()) value=end(|r| r.1) step=0.01 decimals=2 send=send_to />
                </span>
            }
            .into_any()
        }
    };
    let floor = mapping_index::min_smoothing(&reader.holder, &reader.input);
    let smoothing = Signal::derive(move || {
        kind.get()
            .as_ref()
            .and_then(crate::panels::learn::smoothing_of)
            .map(f64::from)
    });
    let send_smoothing = move |v: f64| {
        let k = kind.get_untracked();
        write(k.map(|mut k| {
            let ms = (v.round().clamp(0.0, 10_000.0) as u32).max(floor);
            crate::panels::learn::set_smoothing(&mut k, ms);
            k
        }))
    };
    let curve = move || kind.get().as_ref().and_then(mapping_index::curve_of);
    let set_curve = move |e: leptos::ev::Event| {
        let v = event_target_value(&e);
        let next = crate::panels::motion::CURVES
            .iter()
            .find(|(_, label)| *label == v)
            .map_or(MotionCurve::Linear, |(c, _)| *c);
        let k = kind.get_untracked();
        write(k.as_ref().and_then(|k| mapping_index::with_curve(k, next)));
    };
    let enabled = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| holder.with_value(|h| mapping_index::enabled_of(c, h)))
        })
    };
    let bypass = move |_| {
        let Some(on) = untrack(enabled) else {
            return;
        };
        let edit = studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| holder.with_value(|h| mapping_index::set_enabled(c, h, !on)))
        });
        send(edit);
    };
    let remove = move |_| {
        let edit = studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| holder.with_value(|h| mapping_index::remove(c, h)))
        });
        send(edit);
    };
    let reads = reader.reads_source();
    let mode_text = match mode {
        Mode::Offset => "Offset",
        Mode::Override => "Override",
        Mode::Input => "Input",
    };
    let title = name.clone();
    view! {
        <div class="map-target" role="row" class:bypassed=move || enabled() == Some(false)>
            <span class="map-target-name" role="cell" title=title>
                <span class="hint">"→ "</span>
                {name}
                {note.map(|n| view! { <span class="hint">{format!(" ({n})")}</span> })}
            </span>
            {if reads {
                view! {
                    {amount_field}
                    <span class="map-mode hint" role="cell" title="Offset adds to the stored value; Override replaces it; Input sets a channel">{mode_text}</span>
                    <span class="map-smooth" role="cell">
                        <NumberField named=Signal::derive(|| "Smoothing".to_owned()) unit="ms" value=smoothing step=10.0 decimals=0 min=f64::from(floor) max=10000.0 send=send_smoothing />
                    </span>
                    <span class="map-curve" role="cell">
                        <select class="chan-select" aria-label="Curve" on:change=set_curve>
                            {crate::panels::motion::CURVES
                                .into_iter()
                                .map(|(c, label)| view! { <option value=label selected=move || curve() == Some(c)>{label}</option> })
                                .collect_view()}
                        </select>
                    </span>
                }
                .into_any()
            } else {
                view! { <span class="map-span-rest hint" role="cell">{mode_text}</span> }.into_any()
            }}
            <span class="map-end" role="cell">
                {move || enabled().map(|on| view! {
                    <button type="button" class="map-bypass" aria-pressed=(!on).to_string()
                        title="Bypass: keep the mapping but stop it applying" on:click=bypass>
                        "Bypass"
                    </button>
                })}
                <button type="button" class="chan-x" title="Remove this mapping" aria-label="Remove this mapping" on:click=remove>
                    "×"
                </button>
            </span>
        </div>
    }
}

/// What arming a Mod arms: the channel that names it, or the Mod itself.
fn mod_armed(collection: &Collection, sender: &str, slot: u8) -> ArmedSource {
    match mapping_index::mod_channel(collection, sender, slot) {
        Some(c) => ArmedSource {
            key: SourceKey::Channel(c.channel_id.clone()),
            reader: channel_reader(&c.channel_id),
            label: format!("{} (Mod {slot})", collection.channel_name(&c.channel_id)),
        },
        None => ArmedSource {
            key: mod_key(Some(sender), slot),
            reader: mod_reader(sender, slot),
            label: format!("Mod {slot} · {sender}"),
        },
    }
}

/// The Mod slot an armed source is, for a sender, if it is one of its Mods.
fn armed_slot(collection: &Collection, sender: &str, armed: &ArmedSource) -> Option<u8> {
    (1..=MOD_SLOTS).find(|n| mod_armed(collection, sender, *n).key == armed.key)
}

/// The Sources tab: each sender's 32 Mods to name and arm, what the armed source drives,
/// and the other sources.
#[component]
fn SourcesTab(
    filter: RwSignal<Filter>,
    tab: RwSignal<Tab>,
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: MappingMode,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    // The sender names, so the grids redraw only when the senders change.
    let senders = Memo::new(move |_| {
        feed.with(|f| {
            f.senders
                .iter()
                .map(|s| (s.input.clone(), !s.mods.is_empty()))
                .collect::<Vec<_>>()
        })
    });
    // [ and ] arm the previous or next Mod, and so do the arrow keys while no field or
    // slider has the focus; Escape disarms.
    let keys = window_event_listener(leptos::ev::keydown, move |event| {
        if event.default_prevented() || crate::hotkeys::overlay_open() {
            return;
        }
        let key = event.key();
        if key == "Escape" && mode.0.with_untracked(Option::is_some) {
            mode.0.set(None);
            return;
        }
        let typing = crate::hotkeys::typed_in_field(&event);
        let step: i16 = match key.as_str() {
            "[" if !typing => -1,
            "]" if !typing => 1,
            "ArrowLeft" | "ArrowUp" if !typing => -1,
            "ArrowRight" | "ArrowDown" if !typing => 1,
            _ => return,
        };
        let Some(armed) = mode.0.get_untracked() else {
            return;
        };
        let next = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            senders.with_untracked(|list| {
                list.iter().find_map(|(sender, _)| {
                    let slot = armed_slot(c, sender, &armed)?;
                    let n = (i16::from(slot) - 1 + step).rem_euclid(i16::from(MOD_SLOTS)) as u8 + 1;
                    Some(mod_armed(c, sender, n))
                })
            })
        });
        if let Some(next) = next {
            event.prevent_default();
            mode.0.set(Some(next));
        }
    });
    on_cleanup(move || keys.remove());
    let grids = move || {
        let list = senders.get();
        if list.is_empty() {
            return view! {
                <p class="hint chan-help">
                    "No JTunnel sender is sending its state. Turn on “Use this sender's state” for a sender in Settings › Devices, and its Mods show here."
                </p>
            }
            .into_any();
        }
        list.into_iter()
            .map(|(sender, has_mods)| {
                view! { <ModGrid sender=sender has_mods=has_mods feed=feed mode=mode /> }
            })
            .collect_view()
            .into_any()
    };
    view! {
        <div class="map-sources-tab">
            <div class="map-grids">{grids}</div>
            <div class="map-side">
                <ArmedTargets mode=mode filter=filter tab=tab />
                <OtherSources feed=feed values=values mode=mode />
            </div>
        </div>
    }
}

/// One sender's 32 Mods, four columns by eight rows: each with its number, a name box,
/// how many targets it drives and a live bar. The name box names it; the rest of the
/// cell arms it.
#[component]
fn ModGrid(
    sender: String,
    has_mods: bool,
    feed: RwSignal<Feed>,
    mode: MappingMode,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let grid = NodeRef::<leptos::html::Div>::new();
    let sender = StoredValue::new(sender);
    let focus = move |slot: u8| {
        let Some(grid) = grid.get_untracked() else {
            return;
        };
        if let Ok(Some(el)) = grid.query_selector(&format!("input[data-slot='{slot}']")) {
            use leptos::wasm_bindgen::JsCast;
            if let Ok(input) = el.dyn_into::<leptos::web_sys::HtmlInputElement>() {
                _ = input.focus();
                input.select();
            }
        }
    };
    // Focus starts in the first unnamed Mod.
    Effect::new(move |done: Option<bool>| {
        if done == Some(true) || grid.get().is_none() {
            return done.unwrap_or(false);
        }
        let first = studio.collection.with_untracked(|c| {
            let c = c.as_ref()?;
            sender.with_value(|s| {
                (1..=MOD_SLOTS).find(|n| mapping_index::mod_channel(c, s, *n).is_none())
            })
        });
        if let Some(n) = first {
            focus(n);
        }
        true
    });
    let name = move |names: Vec<(u8, String)>| {
        let Some(s) = session.0.get_value() else {
            return;
        };
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref().map_or_else(Vec::new, |c| {
                sender.with_value(|sender| {
                    let list: Vec<_> = names
                        .into_iter()
                        .map(|(slot, name)| (slot, name, mod_reader(sender, slot)))
                        .collect();
                    mapping_index::name_mods_edits(c, sender, &list)
                })
            })
        });
        if !edits.is_empty() {
            studio.apply(&s, edits);
        }
    };
    let cell = move |slot: u8| {
        let given = move || {
            studio.collection.with(|c| {
                c.as_ref()
                    .and_then(|c| sender.with_value(|s| mapping_index::mod_channel(c, s, slot)))
                    .map(|ch| ch.name.clone())
                    .unwrap_or_default()
            })
        };
        let count = move || {
            studio.collection.with(|c| {
                c.as_ref().map_or(0, |c| {
                    let armed = sender.with_value(|s| mod_armed(c, s, slot));
                    mapping_index::drives(c, &armed.key).len()
                })
            })
        };
        let level = move || {
            feed.with(|f| {
                sender.with_value(|s| {
                    f.senders
                        .iter()
                        .find(|x| &x.input == s)
                        .and_then(|x| x.mods.get(usize::from(slot - 1)).copied())
                })
            })
        };
        let typed = RwSignal::new(None::<String>);
        let bytes = move || typed.get().map(|t| t.trim().len());
        let armed = move || {
            mode.0.with(|m| {
                m.as_ref().is_some_and(|a| {
                    studio.collection.with(|c| {
                        c.as_ref().is_some_and(|c| {
                            sender.with_value(|s| mod_armed(c, s, slot).key == a.key)
                        })
                    })
                })
            })
        };
        let arm = move |_| {
            let next = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .map(|c| sender.with_value(|s| mod_armed(c, s, slot)))
            });
            mode.0.update(|m| {
                *m = if m.as_ref().map(|a| &a.key) == next.as_ref().map(|a| &a.key) {
                    None
                } else {
                    next
                };
            });
        };
        let commit = move |e: leptos::ev::Event| {
            let text = event_target_value(&e);
            typed.set(None);
            if text.trim().is_empty() {
                // An emptied box keeps the name it had: a named Mod stays a channel.
                return;
            }
            name(vec![(slot, text)]);
        };
        let keydown = move |e: leptos::ev::KeyboardEvent| match e.key().as_str() {
            "Enter" => {
                e.prevent_default();
                if slot < MOD_SLOTS {
                    focus(slot + 1);
                } else if let Some(t) = e.target() {
                    use leptos::wasm_bindgen::JsCast;
                    if let Ok(el) = t.dyn_into::<leptos::web_sys::HtmlElement>() {
                        _ = el.blur();
                    }
                }
            }
            "Escape" => {
                e.prevent_default();
                e.stop_propagation();
                typed.set(None);
                use leptos::wasm_bindgen::JsCast;
                if let Some(input) = e
                    .target()
                    .and_then(|t| t.dyn_into::<leptos::web_sys::HtmlInputElement>().ok())
                {
                    input.set_value(&untrack(given));
                    _ = input.blur();
                }
            }
            _ => {}
        };
        // Several pasted lines fill this Mod and the ones after it.
        let paste = move |e: leptos::ev::ClipboardEvent| {
            let Some(text) = e.clipboard_data().and_then(|d| d.get_data("text").ok()) else {
                return;
            };
            if !text.contains('\n') {
                return;
            }
            e.prevent_default();
            let names: Vec<(u8, String)> = text
                .lines()
                .map(str::trim)
                .zip(slot..=MOD_SLOTS)
                .filter(|(line, _)| !line.is_empty())
                .map(|(line, n)| (n, line.to_owned()))
                .collect();
            name(names);
        };
        view! {
            <div class="map-mod" class:armed=armed class:named=move || !given().is_empty() on:click=arm>
                <button type="button" class="map-mod-n mono" tabindex="-1" aria-label=format!("Arm Mod {slot}")>{slot}</button>
                <input
                    class="map-mod-name"
                    data-slot=slot
                    placeholder="Name"
                    aria-label=format!("Name of Mod {slot}")
                    prop:value=given
                    on:click=|e| e.stop_propagation()
                    on:input=move |e| typed.set(Some(event_target_value(&e)))
                    on:change=commit
                    on:keydown=keydown
                    on:paste=paste
                />
                <span class="map-mod-count hint mono" class:over=move || bytes().is_some_and(|b| b > mapping_index::MAX_MOD_NAME)>
                    {move || match bytes() {
                        Some(b) => format!("{b}/{}", mapping_index::MAX_MOD_NAME),
                        None => match count() { 0 => String::new(), n => n.to_string() },
                    }}
                </span>
                <span class="meter-bar map-mod-bar"><span class="map-fill" style:width=move || format!("{:.0}%", level().unwrap_or(0.0) * 100.0)></span></span>
            </div>
        }
    };
    let title = sender.get_value();
    view! {
        <div class="map-sender">
            <div class="section-head chan-head">
                <span class="eyebrow">{title}</span>
                <span class="hint">"JTunnel · state on"</span>
            </div>
            {if has_mods {
                view! { <div class="map-mods" node_ref=grid>{(1..=MOD_SLOTS).map(cell).collect_view()}</div> }.into_any()
            } else {
                view! { <p class="hint chan-help">"This sender sends no Mods."</p> }.into_any()
            }}
            <p class="hint chan-help">
                "Type a name to label a Mod: the DAW shows it. The rest of a cell arms it. Enter or Tab goes to the next name, Escape restores; pasting several lines names this Mod and the ones after it."
            </p>
        </div>
    }
}

/// What the armed source drives, beside the grid.
#[component]
fn ArmedTargets(mode: MappingMode, filter: RwSignal<Filter>, tab: RwSignal<Tab>) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let body = move || {
        let Some(armed) = mode.0.get() else {
            return view! {
                <p class="hint chan-help">"Click a Mod or another source to arm it, then drag on any parameter to map it."</p>
            }
            .into_any();
        };
        let rows: Vec<Reader> = studio.collection.with(|c| {
            c.as_ref().map_or_else(Vec::new, |c| {
                mapping_index::readers(c)
                    .into_iter()
                    .filter(|r| {
                        !matches!(r.holder, mapping_index::Holder::ChannelInput { .. })
                            && mapping_index::source_key(&r.input)
                                .is_some_and(|k| mapping_index::hears(&k, &armed.key))
                    })
                    .collect()
            })
        });
        let key = armed.key.clone();
        let show = move |_| {
            filter.set(Filter {
                source: Some(key.clone()),
                ..Filter::default()
            });
            tab.set(Tab::Mappings);
        };
        view! {
            <div class="section-head chan-head">
                <span class="eyebrow">{format!("Armed: {}", armed.label)}</span>
                <button type="button" class="chan-add" on:click=show>"Show in Mappings"</button>
            </div>
            <div class="map-armed">
                {rows.into_iter().map(|r| view! { <TargetRow reader=r /> }).collect_view()}
            </div>
            <p class="hint chan-help">"Drag on any tinted parameter to add one. [ and ] arm the previous or next Mod, as the arrow keys do while no field has the focus. Escape disarms."</p>
        }
        .into_any()
    };
    view! { <div class="map-armed-box">{body}</div> }
}

/// The sources besides Mods: each MIDI control seen, each channel that isn't a named
/// Mod, and each key that switches a channel. A click arms one.
#[component]
fn OtherSources(
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: MappingMode,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let row = move |armed: ArmedSource,
                    kind: &'static str,
                    extra: Option<String>,
                    level: Option<f32>| {
        let count = studio.collection.with(|c| {
            c.as_ref()
                .map_or(0, |c| mapping_index::drives(c, &armed.key).len())
        });
        let key = armed.key.clone();
        let on = move || mode.0.with(|m| m.as_ref().map(|a| &a.key) == Some(&key));
        let label = armed.label.clone();
        let pick = move |_| {
            let next = armed.clone();
            mode.0.update(|m| {
                *m = if m.as_ref().map(|a| &a.key) == Some(&next.key) {
                    None
                } else {
                    Some(next)
                }
            });
        };
        view! {
            <li class="chan-row link" class:armed=on on:click=pick>
                <span class=format!("map-dot {kind}")></span>
                <span class="chan-row-name">{label}{extra.map(|e| view! { <span class="hint">{format!(" {e}")}</span> })}</span>
                {level.map(|v| view! { <span class="meter-bar map-other-bar"><span class="map-fill" style:width=format!("{:.0}%", v * 100.0)></span></span> })}
                <span class="hint mono">{if count == 0 { String::new() } else { count.to_string() }}</span>
            </li>
        }
    };
    let list = move || {
        let controls = feed.with(|f| f.controls.clone());
        let channel_values = values.get();
        studio.collection.with(|c| {
            let Some(c) = c else {
                return Vec::new();
            };
            let mut out: Vec<AnyView> = controls
                .iter()
                .map(|control| {
                    let reader = crate::panels::channels::midi_input(control);
                    let key = mapping_index::source_key(&reader)
                        .expect("invariant: a MIDI reader has a source");
                    let label = format!("{} · CC {}", control.input, control.controller);
                    row(
                        ArmedSource { key, reader, label },
                        "midi",
                        None,
                        Some(f32::from(control.value) / 127.0),
                    )
                    .into_any()
                })
                .collect();
            let mut channels: Vec<_> = c
                .channels
                .iter()
                .filter(|ch| mapping_index::named_mod(ch).is_none())
                .collect();
            channels.sort_by_key(|ch| c.channel_name(&ch.channel_id).to_lowercase());
            out.extend(channels.into_iter().map(|ch| {
                let armed = ArmedSource {
                    key: SourceKey::Channel(ch.channel_id.clone()),
                    reader: channel_reader(&ch.channel_id),
                    label: c.channel_name(&ch.channel_id),
                };
                let level = channel_values.get(&ch.channel_id).map(|v| v.value.get());
                row(armed, "channel", Some("channel".into()), level).into_any()
            }));
            out.extend(c.hotkeys.iter().filter_map(|h| {
                let action = h.action()?;
                let channel = mapping_index::action_channel(&action)?;
                let armed = ArmedSource {
                    key: SourceKey::Channel(channel.to_owned()),
                    reader: channel_reader(channel),
                    label: format!("Key {}", crate::hotkeys::label(&h.chord)),
                };
                Some(
                    row(
                        armed,
                        "key",
                        Some(format!("· {}", c.channel_name(channel))),
                        None,
                    )
                    .into_any(),
                )
            }));
            out
        })
    };
    view! {
        <div class="section-head chan-head"><span class="eyebrow">"Other sources"</span></div>
        <ul class="chan-rows">
            {move || {
                let items = list();
                if items.is_empty() {
                    view! { <li class="hint chan-help">"No MIDI control has moved yet, and there are no channels or keys."</li> }.into_any()
                } else {
                    items.into_iter().collect_view().into_any()
                }
            }}
        </ul>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn params_read_into_values_and_names() {
        let (mods, names) =
            read_params(&json!({"values": [0.5, 2.0, -1], "names": ["Camera roll", ""]}));
        assert_eq!(mods, vec![0.5, 1.0, 0.0]);
        assert_eq!(names, vec!["Camera roll".to_owned(), String::new()]);
        assert_eq!(read_params(&json!(null)), (Vec::new(), Vec::new()));
    }

    #[test]
    fn a_feed_reads_a_source_and_finds_what_moved() {
        let control = |n: u8, value: u8, count: u64| MidiControlInfo {
            input: "Pads".into(),
            channel: 1,
            controller: n,
            value,
            count,
        };
        let sender = |v: f32| Sender {
            input: "DAW".into(),
            keys: vec!["params".into()],
            mods: vec![0.0, v],
            names: Vec::new(),
        };
        let before = Feed {
            senders: vec![sender(0.2)],
            controls: vec![control(21, 0, 1)],
        };
        let none = HashMap::new();
        let cc21 =
            mapping_index::source_key(&crate::panels::channels::midi_input(&control(21, 0, 1)))
                .expect("a source");
        assert_eq!(before.value(&cc21, &none), Some(0.0));
        assert_eq!(before.value(&mod_key(None, 2), &none), Some(0.2));
        assert_eq!(before.value(&mod_key(Some("Other"), 2), &none), None);
        assert_eq!(before.moved(&before), None);
        let turned = Feed {
            senders: vec![sender(0.6)],
            ..before.clone()
        };
        assert_eq!(turned.moved(&before), Some(mod_key(Some("DAW"), 2)));
        let pressed = Feed {
            controls: vec![control(21, 127, 2)],
            ..before.clone()
        };
        assert_eq!(pressed.moved(&before), Some(cc21));
    }

    #[test]
    fn a_mod_reader_is_the_mod_named_by_slot() {
        let reader = mod_reader("DAW", 20);
        let key = mapping_index::source_key(&reader).expect("a source");
        assert_eq!(key, mod_key(Some("DAW"), 20));
        assert_eq!(mapping_index::mod_slot(&key), Some(20));
    }
}
