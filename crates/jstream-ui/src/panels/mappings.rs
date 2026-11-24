//! The Mappings panel (UI-FEATURES MP-01 and MP-02): everything that is mapped, in one
//! dock panel with two tabs.
//!
//! - **Mappings**: every mapping grouped by source: each channel (a named Mod shown as
//!   its name) with what sets it and everything that follows it or that it scales, then
//!   every source read directly, with all its readers. Each target's amount, smoothing,
//!   curve and bypass are edited in place, and × removes it; "+ Target…" arms the source.
//!   A search, "On program now", a kind of source and "Find by moving a control" narrow
//!   the list. Clicking a channel opens it in the Channels panel.
//! - **Sources**: each JTunnel sender's 32 Mods, four by eight, to name and arm; what the
//!   armed source drives; and the other sources (MIDI controls, channels, keys).
//!
//! The rows come from [`crate::mapping_index`]; live values from `channel.values` and
//! [`crate::live_sources`], read while the panel is open.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#mappings

use crate::app::SessionContext;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::live_sources::{self, LiveSources as Feed};
use crate::mapping_index::{
    self, Filter, Mode, Reader, Row, Setter, SourceKey, SourceKind, channel_reader,
};
use crate::panels::channels::live_values;
use crate::panels::mapping_mode::MappingMode;
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio};
use crate::workspace::DockState;
use jstream_control::protocol::{ChannelValueInfo, Edit, MotionCurve, MotionKind};
use leptos::prelude::*;
use std::collections::HashMap;

/// How many Mods a JTunnel sender has (ADR-0181).
const MOD_SLOTS: u8 = live_sources::MOD_COUNT as u8;

/// A source's value now, 0 to 1, if it can be read: a MIDI control change, a Mod, or a
/// channel (from `channels`).
pub fn source_value(
    live: &Feed,
    key: &SourceKey,
    channels: &HashMap<String, ChannelValueInfo>,
) -> Option<f32> {
    match key {
        SourceKey::Midi {
            control: "cc",
            number: Some(n),
            channel,
            input,
        } => live
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
            live.senders
                .iter()
                .filter(|s| input.as_ref().is_none_or(|i| *i == s.input))
                .find_map(|s| s.mods.get(usize::from(slot - 1)).copied())
        }
        SourceKey::Channel(id) => channels.get(id).map(|c| c.value.get()),
        SourceKey::Midi { .. } => None,
    }
}

/// The first source that moved since `before`, as `live_sources::moved` hears it.
pub fn moved_key(now: &Feed, before: &Feed) -> Option<SourceKey> {
    mapping_index::source_key(&live_sources::moved(before, now)?.reader())
}

/// The source key of Mod `slot` (1 to 32) of `sender`, or of any sender.
pub fn mod_key(sender: Option<&str>, slot: u8) -> SourceKey {
    SourceKey::Data {
        input: sender.map(str::to_owned),
        key: live_sources::MODS_KEY.into(),
        field: format!("values.{}", slot.saturating_sub(1)),
    }
}

/// A reader of Mod `slot` (1 to 32) of `sender` over 0 to 1.
pub fn mod_reader(sender: &str, slot: u8) -> MotionKind {
    live_sources::mod_reader(Some(sender), usize::from(slot.saturating_sub(1)))
}

/// A source the panel can arm: its key, a reader of it over 0 to 1, which a new mapping
/// starts from, and its name as the Sources tab shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// The source.
    pub key: SourceKey,
    /// A reader of it over 0 to 1.
    pub reader: MotionKind,
    /// Its name, as the Sources tab shows it.
    pub label: String,
}

/// The app-wide mapping mode (`mapping_mode::MappingMode`), as the panel arms and reads
/// it: by source, so two readers of one source are one armed source.
#[derive(Clone, Copy)]
pub struct Armed(pub MappingMode);

impl Armed {
    /// The armed source, tracked.
    fn key(self) -> Option<SourceKey> {
        self.0.armed.with(|m| {
            m.as_ref()
                .and_then(|a| mapping_index::source_key(&a.reader))
        })
    }

    /// The armed source, untracked.
    fn key_untracked(self) -> Option<SourceKey> {
        self.0.armed.with_untracked(|m| {
            m.as_ref()
                .and_then(|a| mapping_index::source_key(&a.reader))
        })
    }

    /// Whether `key` is armed, tracked.
    fn is(self, key: &SourceKey) -> bool {
        self.key().as_ref() == Some(key)
    }

    /// Arms `pick`.
    fn arm(self, pick: &Pick) {
        self.0.arm(pick.reader.clone());
    }

    /// Arms `pick`, or disarms when it is armed already.
    fn toggle(self, pick: Option<Pick>) {
        match pick {
            Some(p) if self.key_untracked().as_ref() != Some(&p.key) => self.arm(&p),
            _ => self.0.disarm(),
        }
    }
}

/// The shell's mapping mode, or one for this panel when the shell has none.
pub fn mapping_mode() -> Armed {
    Armed(use_context::<MappingMode>().unwrap_or_else(|| {
        let mode = MappingMode::new();
        provide_context(mode);
        mode
    }))
}

/// The panel's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Mappings,
    Sources,
    Triggers,
}

/// The Mappings panel.
#[component]
pub fn Mappings() -> impl IntoView {
    let tab = RwSignal::new(Tab::Mappings);
    let filter = RwSignal::new(Filter::default());
    let feed = live_sources::live_sources();
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
        <section class="panel mp-panel">
            <div class="segmented mp-tabs" role="tablist" aria-label="Mappings">
                {tab_button(Tab::Mappings, "Mappings")}
                {tab_button(Tab::Sources, "Sources")}
                {tab_button(Tab::Triggers, "Triggers")}
            </div>
            {move || match tab.get() {
                Tab::Mappings => view! { <MappingsTab filter=filter feed=feed values=values mode=mode /> }.into_any(),
                Tab::Sources => view! { <SourcesTab filter=filter tab=tab feed=feed values=values mode=mode /> }.into_any(),
                Tab::Triggers => view! { <TriggersTab /> }.into_any(),
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

/// An armed source's name: a named Mod's "Camera roll (Mod 20)", a channel's name, or
/// the source's.
fn key_label(collection: &Collection, key: &SourceKey) -> String {
    match key {
        SourceKey::Channel(id) => group_label(
            collection,
            &Row::Channel {
                channel_id: id.clone(),
                inputs: Vec::new(),
                setters: Vec::new(),
                followers: Vec::new(),
            },
        ),
        _ => mapping_index::source_name(collection, key),
    }
}

/// What arming a table group arms: its channel, or its direct source.
fn group_armed(collection: &Collection, row: &Row) -> Option<Pick> {
    let label = group_label(collection, row);
    match row {
        Row::Channel { channel_id, .. } => Some(Pick {
            key: SourceKey::Channel(channel_id.clone()),
            reader: channel_reader(channel_id),
            label,
        }),
        Row::Direct { source, readers } => Some(Pick {
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
    mode: Armed,
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
        if let Some(key) = moved_key(&now, &before) {
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
            <span class="mp-caught">
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
        <div class="mp-tools">
            <div class="tree-search mp-search">
                <Icon name="search" size=13 />
                <input
                    type="search"
                    placeholder="Search a source or target"
                    aria-label="Search mappings"
                    prop:value=move || filter.with(|f| f.text.clone())
                    on:input=move |e| filter.update(|f| f.text = event_target_value(&e))
                />
            </div>
            <button type="button" class="mp-toggle" aria-pressed=move || on_program.get().to_string()
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
            <button type="button" class="chan-add mp-find" class:learning=move || finding.with(Option::is_some) on:click=find>
                {move || if finding.with(Option::is_some) { "Move a control…" } else { "Find by moving a control" }}
            </button>
            {caught}
        </div>
        <div class="mp-table" role="table" aria-label="Mappings">
            <div class="mp-head" role="row">
                <span role="columnheader">"Source"</span>
                <span role="columnheader">"Live"</span>
                <span class="mp-head-targets">
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
        <span class="mp-live">
            <span class="meter-bar"><span class="mp-fill" style:width=format!("{width:.0}%")></span></span>
            <span class="mono hint" class:mp-unseen=level.is_none()>{text}</span>
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
    mode: Armed,
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
        view! { <span class=format!("mp-chip small {kind}")>{text}</span> }
    };
    let (main, subs, level, targets) = match row {
        Row::Channel {
            channel_id,
            inputs,
            setters,
            followers,
        } => {
            let named_key = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .and_then(|c| c.channel(&channel_id))
                    .and_then(mapping_index::named_mod)
            });
            let named = named_key.is_some();
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
            // A named Mod shows the Mod as it arrives; any other channel its value.
            let level = Signal::derive(move || match &named_key {
                Some(key) => feed.with(|f| source_value(f, key, &HashMap::new())),
                None => values.with(|v| v.get(&id).map(|c| c.value.get())),
            });
            let open = move |_| {
                studio.channel.set(Some(channel_id.clone()));
                if let Some(dock) = dock {
                    dock.0.update(|d| d.show(PanelId::Channels));
                }
            };
            let class = if named {
                "mp-chip data link"
            } else {
                "mp-chip channel link"
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
            let main = view! { <span class=format!("mp-chip {}", kind_class(&source))>{label.clone()}</span> }
                .into_any();
            let key = source.clone();
            let level = Signal::derive(move || {
                let channels = values.get();
                feed.with(|f| source_value(f, &key, &channels))
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
        Signal::derive(move || key.as_ref().is_some_and(|k| mode.is(k)))
    };
    let arm = move |_| mode.toggle(armed.clone());
    view! {
        <div class="mp-group" role="rowgroup" class:armed=is_armed>
            <div class="mp-source">
                {main}
                {(!subs.is_empty()).then(|| view! { <div class="mp-subs">{subs}</div> })}
            </div>
            <div class="mp-level">{move || live_bar(level.get())}</div>
            <div class="mp-targets">
                {target_rows}
                <button type="button" class="mp-add" aria-pressed=move || is_armed.get().to_string()
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
                <span class="mp-amount" role="cell" title=move || if centred() { "Centred: the stored value sits at the source's middle" } else { "How far it moves the target across the source's travel" }>
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
                <span class="mp-amount mp-range" role="cell" title="The value at the source's 0 and at its 1">
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
        <div class="mp-target" role="row" class:bypassed=move || enabled() == Some(false)>
            <span class="mp-target-name" role="cell" title=title>
                <span class="hint">"→ "</span>
                {name}
                {note.map(|n| view! { <span class="hint">{format!(" ({n})")}</span> })}
            </span>
            {if reads {
                view! {
                    {amount_field}
                    <span class="mp-mode hint" role="cell" title="Offset adds to the stored value; Override replaces it; Input sets a channel">{mode_text}</span>
                    <span class="mp-smooth" role="cell">
                        <NumberField named=Signal::derive(|| "Smoothing".to_owned()) unit="ms" value=smoothing step=10.0 decimals=0 min=f64::from(floor) max=10000.0 send=send_smoothing />
                    </span>
                    <span class="mp-curve" role="cell">
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
                view! { <span class="mp-span-rest hint" role="cell">{mode_text}</span> }.into_any()
            }}
            <span class="mp-end" role="cell">
                {move || enabled().map(|on| view! {
                    <button type="button" class="mp-bypass" aria-pressed=(!on).to_string()
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
fn mod_armed(collection: &Collection, sender: &str, slot: u8) -> Pick {
    match mapping_index::mod_channel(collection, sender, slot) {
        Some(c) => Pick {
            key: SourceKey::Channel(c.channel_id.clone()),
            reader: channel_reader(&c.channel_id),
            label: format!("{} (Mod {slot})", collection.channel_name(&c.channel_id)),
        },
        None => Pick {
            key: mod_key(Some(sender), slot),
            reader: mod_reader(sender, slot),
            label: format!("Mod {slot} · {sender}"),
        },
    }
}

/// The Sources tab: each sender's 32 Mods to name and arm, what the armed source drives,
/// and the other sources.
#[component]
fn SourcesTab(
    filter: RwSignal<Filter>,
    tab: RwSignal<Tab>,
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: Armed,
) -> impl IntoView {
    // The sender names, so the grids redraw only when the senders change.
    let senders = Memo::new(move |_| {
        feed.with(|f| {
            f.senders
                .iter()
                .map(|s| (s.input.clone(), !s.mods.is_empty()))
                .collect::<Vec<_>>()
        })
    });
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
        <div class="mp-sources-tab">
            <div class="mp-grids">{grids}</div>
            <div class="mp-side">
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
fn ModGrid(sender: String, has_mods: bool, feed: RwSignal<Feed>, mode: Armed) -> impl IntoView {
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
            mode.key().is_some_and(|k| {
                studio.collection.with(|c| {
                    c.as_ref()
                        .is_some_and(|c| sender.with_value(|s| mod_armed(c, s, slot).key == k))
                })
            })
        };
        let arm = move |_| {
            let next = studio.collection.with_untracked(|c| {
                c.as_ref()
                    .map(|c| sender.with_value(|s| mod_armed(c, s, slot)))
            });
            mode.toggle(next);
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
            <div class="mp-mod" class:armed=armed class:named=move || !given().is_empty() on:click=arm>
                <button type="button" class="mp-mod-n mono" tabindex="-1" aria-label=format!("Arm Mod {slot}")>{slot}</button>
                <input
                    class="mp-mod-name"
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
                <span class="mp-mod-count hint mono" class:over=move || bytes().is_some_and(|b| b > mapping_index::MAX_MOD_NAME)>
                    {move || match bytes() {
                        Some(b) => format!("{b}/{}", mapping_index::MAX_MOD_NAME),
                        None => match count() { 0 => String::new(), n => n.to_string() },
                    }}
                </span>
                <span class="meter-bar mp-mod-bar"><span class="mp-fill" style:width=move || format!("{:.0}%", level().unwrap_or(0.0) * 100.0)></span></span>
            </div>
        }
    };
    let title = sender.get_value();
    view! {
        <div class="mp-sender">
            <div class="section-head chan-head">
                <span class="eyebrow">{title}</span>
                <span class="hint">"JTunnel · state on"</span>
            </div>
            {if has_mods {
                view! { <div class="mp-mods" node_ref=grid>{(1..=MOD_SLOTS).map(cell).collect_view()}</div> }.into_any()
            } else {
                view! { <p class="hint chan-help">"This sender sends no Mods."</p> }.into_any()
            }}
            <p class="hint chan-help">
                "Type a name to label a Mod: the DAW shows it. The rest of a cell arms it. Enter or Tab goes to the next name, Escape restores; pasting several lines names this Mod and the ones after it."
            </p>
        </div>
    }
}

/// The Triggers tab: every key and MIDI trigger with each action it runs (the Settings
/// page's list, ADR-0208), then what starts and stops each sequence.
#[component]
fn TriggersTab() -> impl IntoView {
    view! {
        <div class="mp-triggers">
            <crate::panels::triggers::KeysAndMidi />
            <SequenceTriggers />
        </div>
    }
}

/// Each sequence's starts and stops as a trigger row: the sentence, what it does, Edit
/// (the Sequence panel on that sequence) and Remove.
#[component]
fn SequenceTriggers() -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let dock = use_context::<DockState>();
    let rows = move || {
        studio.collection.with(|c| {
            c.as_ref().map_or_else(Vec::new, |c| {
                mapping_index::sequence_triggers(c)
                    .into_iter()
                    .map(|row| {
                        let text = crate::panels::sequence::trigger_text(c, &row.trigger);
                        let scene = c
                            .timeline(&row.timeline_id)
                            .map(|(s, _)| s.scene_id.clone());
                        (row, text, scene)
                    })
                    .collect::<Vec<_>>()
            })
        })
    };
    let list = move || {
        let rows = rows();
        if rows.is_empty() {
            return view! {
                <p class="placeholder">"No sequence starts or stops by itself. A sequence's “+ Trigger” in the Sequence panel adds one."</p>
            }
            .into_any();
        }
        view! {
            <ul class="trigger-list">
                {rows.into_iter().map(|(row, text, scene)| {
                    let does = format!("{} {}", if row.stops { "Stops" } else { "Starts" }, row.name);
                    let aria = format!("Remove “{text}” from {}", row.name);
                    let timeline = row.timeline_id.clone();
                    let edit = move |_| {
                        if let Some(scene) = scene.clone() {
                            studio.select_scene(scene);
                        }
                        studio.sequence.set(Some(timeline.clone()));
                        if let Some(dock) = dock {
                            dock.0.update(|d| d.show(PanelId::Sequence));
                        }
                    };
                    let remove = move |_| {
                        let edit = studio.collection.with_untracked(|c| {
                            c.as_ref().and_then(|c| mapping_index::remove_sequence_trigger(c, &row))
                        });
                        if let (Some(edit), Some(s)) = (edit, session.get()) {
                            studio.apply(&s, vec![edit]);
                        }
                    };
                    view! {
                        <li class="trigger-row" data-kind="sequence">
                            <span class="trigger-when">{text}</span>
                            <ul class="trigger-does"><li class="trigger-action"><span>{does}</span></li></ul>
                            <span class="trigger-buttons">
                                <button type="button" on:click=edit>"Edit"</button>
                                <button type="button" aria-label=aria on:click=remove>"Remove"</button>
                            </span>
                        </li>
                    }
                }).collect_view()}
            </ul>
        }
        .into_any()
    };
    view! {
        <section class="prop-section">
            <div class="section-head"><span class="eyebrow">"Sequences"</span></div>
            {list}
        </section>
    }
}

/// What the armed source drives, beside the grid.
#[component]
fn ArmedTargets(mode: Armed, filter: RwSignal<Filter>, tab: RwSignal<Tab>) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let body = move || {
        let Some(key) = mode.key() else {
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
                                .is_some_and(|k| mapping_index::hears(&k, &key))
                    })
                    .collect()
            })
        });
        let label = studio
            .collection
            .with(|c| c.as_ref().map(|c| key_label(c, &key)))
            .unwrap_or_default();
        let show = move |_| {
            filter.set(Filter {
                source: Some(key.clone()),
                ..Filter::default()
            });
            tab.set(Tab::Mappings);
        };
        view! {
            <div class="section-head chan-head">
                <span class="eyebrow">{format!("Armed: {label}")}</span>
                <button type="button" class="chan-add" on:click=show>"Show in Mappings"</button>
            </div>
            <div class="mp-armed">
                {rows.into_iter().map(|r| view! { <TargetRow reader=r /> }).collect_view()}
            </div>
            <p class="hint chan-help">"Drag on any setting to map it. [ and ] arm the previous or next Mod; Escape or Done in the bar disarms."</p>
        }
        .into_any()
    };
    view! { <div class="mp-armed-box">{body}</div> }
}

/// The sources besides Mods: each MIDI control seen, each channel that isn't a named
/// Mod, and each key that switches a channel. A click arms one.
#[component]
fn OtherSources(
    feed: RwSignal<Feed>,
    values: RwSignal<HashMap<String, ChannelValueInfo>>,
    mode: Armed,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let row = move |armed: Pick, kind: &'static str, extra: Option<String>, level: Option<f32>| {
        let count = studio.collection.with(|c| {
            c.as_ref()
                .map_or(0, |c| mapping_index::drives(c, &armed.key).len())
        });
        let key = armed.key.clone();
        let on = move || mode.is(&key);
        let label = armed.label.clone();
        let pick = move |_| {
            mode.toggle(Some(armed.clone()));
        };
        view! {
            <li class="chan-row link" class:armed=on on:click=pick>
                <span class=format!("mp-dot {kind}")></span>
                <span class="chan-row-name">{label}{extra.map(|e| view! { <span class="hint">{format!(" {e}")}</span> })}</span>
                {level.map(|v| view! { <span class="meter-bar mp-other-bar"><span class="mp-fill" style:width=format!("{:.0}%", v * 100.0)></span></span> })}
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
                        Pick { key, reader, label },
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
                let armed = Pick {
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
                let armed = Pick {
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
    use crate::live_sources::Sender;

    #[test]
    fn a_source_is_read_live_and_found_by_moving() {
        let control = |n: u8, value: u8, count: u64| jstream_control::protocol::MidiControlInfo {
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
            mod_names: Vec::new(),
        };
        let before = Feed {
            senders: vec![sender(0.2)],
            controls: vec![control(21, 0, 1)],
            loaded: true,
        };
        let none = HashMap::new();
        let cc21 =
            mapping_index::source_key(&crate::panels::channels::midi_input(&control(21, 0, 1)))
                .expect("a source");
        assert_eq!(source_value(&before, &cc21, &none), Some(0.0));
        assert_eq!(source_value(&before, &mod_key(None, 2), &none), Some(0.2));
        assert_eq!(
            source_value(&before, &mod_key(Some("Other"), 2), &none),
            None
        );
        assert_eq!(moved_key(&before, &before), None);
        let turned = Feed {
            senders: vec![sender(0.6)],
            ..before.clone()
        };
        assert_eq!(moved_key(&turned, &before), Some(mod_key(Some("DAW"), 2)));
        let pressed = Feed {
            controls: vec![control(21, 127, 2)],
            ..before.clone()
        };
        assert_eq!(moved_key(&pressed, &before), Some(cc21));
    }

    #[test]
    fn a_mod_reader_is_the_mod_named_by_slot() {
        let reader = mod_reader("DAW", 20);
        let key = mapping_index::source_key(&reader).expect("a source");
        assert_eq!(key, mod_key(Some("DAW"), 20));
        assert_eq!(mapping_index::mod_slot(&key), Some(20));
    }
}
