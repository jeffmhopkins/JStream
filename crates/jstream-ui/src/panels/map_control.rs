//! The Map control dialog (UI-FEATURES VC-03): one place to choose what drives a
//! setting. A right-click on a mappable setting, or its control button, offers "Map
//! control…", which opens it on that setting. The person picks a source from what is
//! arriving now, each with its live value and what it drives already: a JTunnel
//! sender's Mod or any field of its live data, a MIDI control, a channel, or a key.
//! Learn takes whichever MIDI control or Mod moves first. The foot sets the range the
//! source maps onto, its smoothing and its curve.
//!
//! Map writes one value binding named after the setting ([`learn_id`]), so mapping again
//! replaces it. A source is never refused for being in use: any number of settings read
//! one control, Mod or channel. A key runs one action, so a key maps through a channel
//! it switches ([`key_edits`]): a second setting mapped to that key follows the same
//! channel.
//!
//! Docs: docs/UI-FEATURES.md#automation, docs/adr/0178-value-bindings-drive-any-number.md

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::live_sources::{
    self, MOD_COUNT, MODS_KEY, Moved, live_sources, mod_drives, mod_of, mod_reader, reader_name,
};
use crate::panels::channels::{ChannelSearch, live_values, new_channel};
use crate::panels::learn::{KeyCapture, fraction, learn_id, set_smoothing, smoothing_of};
use crate::panels::map_target::MapTarget;
use crate::panels::number::NumberField;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{
    Action, ChannelInfo, ChannelSwitch, Chord, Edit, Finite32, MotionCurve, MotionKind,
    MotionMidiControl, SceneId, ValueBinding, ValueTarget,
};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::BTreeMap;
use std::time::Duration;

/// How long Learn listens before it gives up.
const LEARN_FOR: Duration = Duration::from_secs(15);
/// How many fields of one message the picker lists.
const FIELD_LIMIT: usize = 200;

/// What the dialog maps a control onto.
#[derive(Clone, Debug, PartialEq)]
pub struct MapControlRequest {
    /// The setting.
    pub target: MapTarget,
    /// What a source maps onto at first, in the setting's unit: a setting's lowest and
    /// highest, or the offset a property gets.
    pub range: (f64, f64),
    /// The lowest and highest either end of a mapping may be.
    pub limits: (f64, f64),
    /// The setting's name.
    pub label: String,
    /// The setting's value now.
    pub current: Option<f64>,
}

/// The open Map control dialog, if any. The shell provides it and draws
/// [`MapControlHost`].
#[derive(Clone, Copy)]
pub struct MapControlDialog(pub RwSignal<Option<MapControlRequest>>);

impl MapControlDialog {
    /// No dialog open.
    pub fn new() -> Self {
        MapControlDialog(RwSignal::new(None))
    }
}

impl Default for MapControlDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// The dialog's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Key,
    Jtunnel,
    Midi,
    Channel,
    Data,
}

impl Tab {
    const ALL: [(Tab, &'static str); 5] = [
        (Tab::Key, "Hotkey"),
        (Tab::Jtunnel, "JTunnel"),
        (Tab::Midi, "MIDI"),
        (Tab::Channel, "Channel"),
        (Tab::Data, "Other data"),
    ];

    /// The tab that lists `reader`.
    fn of(reader: &MotionKind) -> Tab {
        match reader {
            MotionKind::Midi { .. } => Tab::Midi,
            MotionKind::Channel { .. } => Tab::Channel,
            MotionKind::Data { .. } if mod_of(reader).is_none() => Tab::Data,
            _ => Tab::Jtunnel,
        }
    }
}

/// The ends a reader maps its source onto.
pub fn ends(kind: &MotionKind) -> Option<(f32, f32)> {
    match kind {
        MotionKind::Midi { from, to, .. }
        | MotionKind::Data { from, to, .. }
        | MotionKind::Channel { from, to, .. } => Some((from.get(), to.get())),
        _ => None,
    }
}

/// `kind` mapping its source onto `from..to`.
pub fn with_ends(mut kind: MotionKind, low: f32, high: f32) -> MotionKind {
    let end = |v: f32| Finite32::new(v).unwrap_or(Finite32::ZERO);
    if let MotionKind::Midi { from, to, .. }
    | MotionKind::Data { from, to, .. }
    | MotionKind::Channel { from, to, .. } = &mut kind
    {
        *from = end(low);
        *to = end(high);
    }
    kind
}

/// How a reader bends its source.
pub fn curve_of(kind: &MotionKind) -> Option<MotionCurve> {
    match kind {
        MotionKind::Midi { curve, .. }
        | MotionKind::Data { curve, .. }
        | MotionKind::Channel { curve, .. } => Some(*curve),
        _ => None,
    }
}

/// `kind` bending its source by `next`.
pub fn with_curve(mut kind: MotionKind, next: MotionCurve) -> MotionKind {
    if let MotionKind::Midi { curve, .. }
    | MotionKind::Data { curve, .. }
    | MotionKind::Channel { curve, .. } = &mut kind
    {
        *curve = next;
    }
    kind
}

/// Whether two readers read the same source, whatever range, smoothing and curve each
/// has.
pub fn same_source(a: &MotionKind, b: &MotionKind) -> bool {
    match (a, b) {
        (
            MotionKind::Midi {
                control: c1,
                controller: n1,
                channel: ch1,
                input: i1,
                ..
            },
            MotionKind::Midi {
                control: c2,
                controller: n2,
                channel: ch2,
                input: i2,
                ..
            },
        ) => c1 == c2 && n1 == n2 && ch1 == ch2 && i1 == i2,
        (
            MotionKind::Data {
                input: i1,
                key: k1,
                field: f1,
                ..
            },
            MotionKind::Data {
                input: i2,
                key: k2,
                field: f2,
                ..
            },
        ) => i1 == i2 && k1 == k2 && f1 == f2,
        (MotionKind::Channel { channel_id: a, .. }, MotionKind::Channel { channel_id: b, .. }) => {
            a == b
        }
        _ => false,
    }
}

/// `reader` as the dialog's draft: with the range, smoothing and curve of the draft it
/// replaces, so choosing another source keeps what the foot shows, or over `range` when
/// there was none.
pub fn picked(reader: MotionKind, previous: Option<&MotionKind>, range: (f64, f64)) -> MotionKind {
    let (low, high) = previous
        .and_then(ends)
        .unwrap_or((range.0 as f32, range.1 as f32));
    let mut next = with_ends(reader, low, high);
    if let Some(previous) = previous {
        if let Some(ms) = smoothing_of(previous) {
            set_smoothing(&mut next, ms);
        }
        if let Some(curve) = curve_of(previous) {
            next = with_curve(next, curve);
        }
    }
    next
}

/// The binding that maps `input` onto `target`, named [`learn_id`], keeping the
/// `amount` of the binding it replaces.
pub fn binding_for(
    target: &ValueTarget,
    input: MotionKind,
    amount: Option<String>,
) -> ValueBinding {
    ValueBinding {
        binding_id: learn_id(target),
        input,
        target: target.clone(),
        amount,
    }
}

/// A MIDI reader typed by hand: `number` is the controller or the note, unused for a
/// bend or a pressure; `channel` 0 is any, and an empty `input` any.
pub fn midi_reader(control: MotionMidiControl, number: u8, channel: u8, input: &str) -> MotionKind {
    let numbered = matches!(control, MotionMidiControl::Cc | MotionMidiControl::Note);
    MotionKind::Midi {
        control,
        controller: numbered.then_some(number.min(127)),
        channel: (1..=16).contains(&channel).then_some(channel),
        input: Some(input.trim().to_owned()).filter(|i| !i.is_empty()),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
    }
}

/// A reader of `field` of `key` from `sender`, over 0 to 1.
pub fn data_reader(sender: Option<&str>, key: &str, field: &str) -> MotionKind {
    MotionKind::Data {
        input: sender.filter(|s| !s.is_empty()).map(str::to_owned),
        key: key.to_owned(),
        field: field.to_owned(),
        range: [Finite32::ZERO, Finite32::ONE],
        states: BTreeMap::new(),
        smoothing_ms: 50,
        curve: MotionCurve::Linear,
        from: Finite32::ZERO,
        to: Finite32::ONE,
        color_from: None,
        palette: BTreeMap::new(),
    }
}

/// The channel `chord` switches already, if that is what it is bound to.
pub fn key_channel(collection: &Collection, chord: &Chord) -> Option<String> {
    let bound = collection.hotkeys.iter().find(|b| &b.chord == chord)?;
    match bound.action()? {
        Action::ChannelSwitch { channel_id, .. } => Some(channel_id),
        _ => None,
    }
}

/// A reader of the channel `channel_id` over `range`, unsmoothed: the channel does its
/// own shaping.
pub fn channel_reader(channel_id: &str, range: (f64, f64)) -> MotionKind {
    let end = |v: f64| Finite32::new(v as f32).unwrap_or(Finite32::ZERO);
    MotionKind::Channel {
        channel_id: channel_id.to_owned(),
        smoothing_ms: 0,
        curve: MotionCurve::Linear,
        from: end(range.0),
        to: end(range.1),
    }
}

/// What `chord` does now, in words, when it is bound to anything but a channel's
/// switch: mapping it here takes it from that.
pub fn key_taken(collection: &Collection, chord: &Chord) -> bool {
    collection.hotkeys.iter().any(|b| &b.chord == chord) && key_channel(collection, chord).is_none()
}

/// What the Hotkey tab maps.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMapping<'a> {
    /// The key.
    pub chord: &'a Chord,
    /// The setting.
    pub target: &'a MapTarget,
    /// The setting's name, which a new channel takes.
    pub label: &'a str,
    /// The scene a new channel belongs to.
    pub scene: Option<SceneId>,
    /// The setting's value with the key off and on.
    pub ends: (f64, f64),
    /// How long the switch takes, in milliseconds.
    pub fade_ms: u32,
    /// Whether the setting is at its on value now, so the channel starts switched on.
    pub starts_on: bool,
}

/// The edits that map a key onto a setting (ADR-0183 decision 6). A key runs one
/// action, so it switches a channel and the setting follows that channel between
/// `ends`. A key that switches a channel already keeps it, and the setting joins what
/// follows it: one key, many settings. Otherwise a new channel is made, resting at 1
/// and fading over `fade_ms`, and the key is bound to its switch, in place of whatever
/// it ran before.
pub fn key_edits(collection: &Collection, map: &KeyMapping<'_>) -> Vec<Edit> {
    if let Some(channel_id) = key_channel(collection, map.chord) {
        return map.target.set(
            collection,
            channel_reader(&channel_id, map.ends),
            None,
            None,
        );
    }
    let mut channel = new_channel(map.label, map.scene.clone(), 1.0, &collection.channels);
    channel.rise_ms = map.fade_ms.min(60_000);
    channel.fall_ms = map.fade_ms.min(60_000);
    channel.on = map.starts_on;
    let toggle = Action::ChannelSwitch {
        channel_id: channel.channel_id.clone(),
        to: ChannelSwitch::Toggle,
    };
    let follow = map.target.set(
        collection,
        channel_reader(&channel.channel_id, map.ends),
        None,
        None,
    );
    let mut edits = vec![Edit::ChannelSet { channel }];
    edits.extend(follow);
    edits.extend(crate::hotkeys::bind_edits(collection, &toggle, map.chord));
    edits
}

/// The curves a mapping can take, by name.
const CURVES: [(&str, &str, MotionCurve); 4] = [
    ("linear", "Linear", MotionCurve::Linear),
    ("ease_in", "Ease in", MotionCurve::EaseIn),
    ("ease_out", "Ease out", MotionCurve::EaseOut),
    ("ease_in_out", "Ease in and out", MotionCurve::EaseInOut),
];

/// A mapping's range, smoothing and curve, as fields over one reader: `send` takes the
/// reader with a field changed. `limits` keeps the range within the setting's.
#[component]
pub fn ReaderFields(
    /// The reader; `None` disables the fields.
    #[prop(into)]
    kind: Signal<Option<MotionKind>>,
    /// The lowest and highest the range may reach.
    limits: (f64, f64),
    /// What the range's ends are called.
    #[prop(default = ("From", "To"))]
    names: (&'static str, &'static str),
    /// Takes the changed reader.
    send: impl Fn(MotionKind) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let (min, max) = (limits.0.min(limits.1), limits.0.max(limits.1));
    let low = Signal::derive(move || kind.get().as_ref().and_then(ends).map(|e| f64::from(e.0)));
    let high = Signal::derive(move || kind.get().as_ref().and_then(ends).map(|e| f64::from(e.1)));
    let smoothing =
        Signal::derive(move || kind.get().as_ref().and_then(smoothing_of).map(f64::from));
    let curve = move || {
        kind.get()
            .as_ref()
            .and_then(curve_of)
            .unwrap_or(MotionCurve::Linear)
    };
    let change = move |with: &dyn Fn(MotionKind) -> MotionKind| {
        let Some(now) = kind.get_untracked() else {
            return false;
        };
        send(with(now));
        true
    };
    let set_curve = move |e: leptos::ev::Event| {
        let value = event_target_value(&e);
        let next = CURVES
            .iter()
            .find(|(id, _, _)| *id == value)
            .map_or(MotionCurve::Linear, |(_, _, c)| *c);
        change(&|k| with_curve(k, next));
    };
    view! {
        <div class="map-fields">
            <NumberField label=names.0 value=low step=0.1 decimals=2 min=min max=max
                send=move |v| change(&|k| {
                    let high = ends(&k).map_or(0.0, |e| e.1);
                    with_ends(k, v as f32, high)
                }) />
            <NumberField label=names.1 value=high step=0.1 decimals=2 min=min max=max
                send=move |v| change(&|k| {
                    let low = ends(&k).map_or(0.0, |e| e.0);
                    with_ends(k, low, v as f32)
                }) />
            <NumberField label="Smoothing" unit="ms" value=smoothing step=10.0 decimals=0 min=0.0 max=10000.0
                send=move |v| change(&|mut k| {
                    set_smoothing(&mut k, v.round().clamp(0.0, 10_000.0) as u32);
                    k
                }) />
            <label class="map-curve">
                <span class="field-label">"Curve"</span>
                <select aria-label="Curve" disabled=move || kind.get().is_none() on:change=set_curve>
                    {CURVES
                        .into_iter()
                        .map(|(id, label, c)| view! { <option value=id selected=move || curve() == c>{label}</option> })
                        .collect_view()}
                </select>
            </label>
        </div>
    }
}

/// How long Learn waits on the only source that moved before it takes it.
const ACCEPT_AFTER: Duration = Duration::from_millis(900);

/// Learn, from a setting's menu: listens for what moves and lists it, farthest first.
/// When only one source has moved it is taken after a short pause; when several have,
/// the person picks. What was moving already when it opened is left out, so it works
/// while a DAW plays.
#[component]
pub fn LearnPopover(
    /// Takes the reader of the source chosen, over 0 to 1.
    accept: impl Fn(MotionKind) + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let live = live_sources();
    let listening = StoredValue::new(None::<live_sources::Listening>);
    let heard = RwSignal::new(Vec::<(Moved, f32)>::new());
    let settling = RwSignal::new(true);
    // Bumped each time the set of sources heard changes, so a pause is only over when
    // nothing new joined during it.
    let round = StoredValue::new(0_u32);
    Effect::new(move |_| {
        let now = live.get();
        // Until the first read, everything would look as if it had just arrived.
        if !now.loaded {
            return;
        }
        let mut state = listening.get_value();
        let next = match state.as_mut() {
            None => {
                state = Some(live_sources::Listening::start(now));
                Vec::new()
            }
            Some(l) => l.hear(&now),
        };
        settling.set(
            state
                .as_ref()
                .is_some_and(live_sources::Listening::settling),
        );
        listening.set_value(state);
        let changed = heard.with_untracked(|was| {
            was.len() != next.len() || was.iter().zip(&next).any(|(a, b)| !a.0.same(&b.0))
        });
        if changed {
            heard.set(next);
            round.update_value(|r| *r += 1);
            let mine = round.get_value();
            set_timeout(
                move || {
                    if round.try_get_value() != Some(mine) {
                        return;
                    }
                    let only = heard.try_with_untracked(|h| match &h[..] {
                        [(source, _)] => Some(source.reader()),
                        _ => None,
                    });
                    if let Some(Some(reader)) = only {
                        accept(reader);
                    }
                },
                ACCEPT_AFTER,
            );
        }
    });
    let rows = move || {
        heard
            .get()
            .into_iter()
            .map(|(source, travel)| {
                let reader = source.reader();
                let name = reader_name(None, &reader);
                view! {
                    <button type="button" class="menuitem map-field" on:click=move |_| accept(reader.clone())>
                        <span class="ctx-label">{name}</span>
                        {bar(move || travel)}
                    </button>
                }
            })
            .collect_view()
    };
    view! {
        <div class="ctx-head">"Learn"</div>
        <p class="hint control-menu-note" role="status">
            {move || match (settling.get(), heard.with(Vec::len)) {
                (true, _) => "Listening…",
                (false, 0) => "Move a knob, fader or button, or a Mod in the DAW.",
                (false, 1) => "Heard one. Taking it…",
                (false, _) => "Several moved. Pick one:",
            }}
        </p>
        <div class="map-list">{rows}</div>
    }
}

/// Draws the Map control dialog while one is open.
#[component]
pub fn MapControlHost() -> impl IntoView {
    let dialog = expect_context::<MapControlDialog>();
    move || {
        dialog
            .0
            .get()
            .map(|request| view! { <MapControl request=request /> })
    }
}

/// A 0 to 1 level as a small bar.
fn bar(level: impl Fn() -> f32 + Send + Sync + 'static) -> impl IntoView {
    view! {
        <span class="map-bar" aria-hidden="true">
            <span class="map-bar-fill" style:width=move || format!("{:.0}%", level().clamp(0.0, 1.0) * 100.0)></span>
        </span>
    }
}

/// The dialog, on one setting.
#[component]
fn MapControl(request: MapControlRequest) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let mapping_mode = use_context::<crate::panels::mapping_mode::MappingMode>();
    let session = expect_context::<SessionContext>();
    let dialog = expect_context::<MapControlDialog>();
    let request = StoredValue::new(request);
    let range = request.with_value(|r| r.range);
    let limits = request.with_value(|r| r.limits);
    let adds = request.with_value(|r| r.target.adds());
    let close = move || dialog.0.set(None);

    // Every mapping onto the setting: one for a setting a binding takes over, any
    // number for a property mappings add to.
    let mapped = move || {
        studio.collection.with(|c| {
            c.as_ref()
                .map(|c| request.with_value(|r| r.target.mappings(c)))
                .unwrap_or_default()
        })
    };
    let bound = move || mapped().into_iter().next();
    let existing = untrack(bound);
    // The mapping as it would be written: the source with its range, smoothing and curve.
    let draft = RwSignal::new(existing.as_ref().map(|b| b.input.clone()));
    let tab = RwSignal::new(
        existing
            .as_ref()
            .map_or(Tab::Jtunnel, |b| Tab::of(&b.input)),
    );
    // A channel made here, written with the mapping.
    let pending = RwSignal::new(None::<ChannelInfo>);
    let live = live_sources();
    let values = live_values();
    let pick = move |reader: MotionKind| {
        draft.update(|d| *d = Some(picked(reader, d.as_ref(), range)));
    };
    let chosen = move |reader: &MotionKind| {
        draft.with(|d| d.as_ref().is_some_and(|d| same_source(d, reader)))
    };

    // Learn: lists what moves once it has settled, and pre-selects what moved farthest.
    // Nothing is mapped until Use.
    let listener = StoredValue::new(None::<live_sources::Listening>);
    let learning = RwSignal::new(false);
    let heard = RwSignal::new(Vec::<(Moved, f32)>::new());
    Effect::new(move |_| {
        let now = live.get();
        if !learning.get_untracked() {
            return;
        }
        let mut state = listener.get_value();
        let Some(next) = state.as_mut().map(|l| l.hear(&now)) else {
            return;
        };
        listener.set_value(state);
        // The first thing heard is chosen, and stays so while more join the list.
        if heard.with_untracked(Vec::is_empty)
            && let Some((first, _)) = next.first()
        {
            let reader = first.reader();
            tab.set(Tab::of(&reader));
            pick(reader);
        }
        let changed = heard.with_untracked(|was| {
            was.len() != next.len() || was.iter().zip(&next).any(|(a, b)| !a.0.same(&b.0))
        });
        if changed {
            heard.set(next);
        }
    });
    let learn = move |_| {
        if learning.get_untracked() {
            learning.set(false);
            return;
        }
        listener.set_value(Some(live_sources::Listening::start(live.get_untracked())));
        heard.set(Vec::new());
        learning.set(true);
        set_timeout(
            move || {
                learning.try_set(false);
            },
            LEARN_FOR,
        );
    };
    let heard_strip = move || {
        let list = heard.get();
        (!list.is_empty()).then(|| {
            let chips = list
                .into_iter()
                .map(|(source, _)| {
                    let reader = source.reader();
                    let name = reader_name(None, &reader);
                    let is_chosen = {
                        let reader = reader.clone();
                        move || chosen(&reader)
                    };
                    view! {
                        <button type="button" class="map-key" class:on=is_chosen
                            on:click=move |_| { tab.set(Tab::of(&reader)); pick(reader.clone()); }>
                            {name}
                        </button>
                    }
                })
                .collect_view();
            view! { <div class="map-heard"><span class="hint">"Moved:"</span>{chips}</div> }
        })
    };

    // JTunnel: Mods, and any other field of a sender's data.
    let any_sender = RwSignal::new(false);
    let opened = RwSignal::new(None::<(String, String, Vec<(String, String)>)>);
    let open_key = move |sender: String, key: String| {
        let Some(s) = session.0.get_value() else {
            return;
        };
        spawn_local(async move {
            let fields = live_sources::message(&s, &sender, &key)
                .await
                .map(|m| live_sources::fields_of(&m, FIELD_LIMIT))
                .unwrap_or_default();
            opened.try_set(Some((sender, key, fields)));
        });
    };
    let mods = move |sender: Option<String>, names: Vec<String>| {
        (0..MOD_COUNT)
            .map(|slot| {
                let from = sender.clone();
                let reader = move || {
                    let named = from.clone().filter(|_| !any_sender.get_untracked());
                    mod_reader(named.as_deref(), slot)
                };
                let is_chosen = {
                    let from = sender.clone();
                    move || {
                        draft.with(|d| {
                            d.as_ref().and_then(mod_of).is_some_and(|(input, n)| {
                                n == slot && (input.is_none() || input == from.as_deref())
                            })
                        })
                    }
                };
                let drives = {
                    let from = sender.clone().unwrap_or_default();
                    let own = names.get(slot).cloned().unwrap_or_default();
                    move || {
                        let line = studio.collection.with(|c| {
                            c.as_ref()
                                .map(|c| live_sources::drives_line(&mod_drives(c, &from, slot)))
                                .unwrap_or_default()
                        });
                        if line.is_empty() { own.clone() } else { line }
                    }
                };
                // Read as it arrives, so the cell itself is never drawn again under a click.
                let level = sender.clone().map(|from| {
                    move || {
                        live.with(|l| {
                            l.senders
                                .iter()
                                .find(|s| s.input == from)
                                .and_then(|s| s.mods.get(slot).copied())
                                .unwrap_or(0.0)
                        })
                    }
                });
                view! {
                    <button type="button" class="map-mod" class:on=is_chosen.clone() aria-pressed=move || is_chosen().to_string()
                        on:click=move |_| pick(reader())>
                        <span class="map-mod-name">{format!("Mod {}", slot + 1)}</span>
                        <span class="map-mod-drives">{drives}</span>
                        {level.map(bar)}
                    </button>
                }
            })
            .collect_view()
    };
    // Who is sending what, without the values: the lists are drawn again only when a
    // sender, a key or a name comes or goes.
    let shape = Memo::new(move |_| {
        live.with(|l| {
            l.senders
                .iter()
                .map(|s| {
                    (
                        s.input.clone(),
                        s.keys.clone(),
                        !s.mods.is_empty(),
                        s.mod_names.clone(),
                    )
                })
                .collect::<Vec<_>>()
        })
    });
    let jtunnel = move || {
        let senders = shape.get();
        if senders.is_empty() {
            return view! {
                <p class="hint map-help">
                    "No JTunnel sender is sending data. Accept one in Settings › JTunnel, with “Use this sender's state” on. A Mod can still be mapped now and will follow once a sender sends it."
                </p>
                <div class="map-mods">{mods(None, Vec::new())}</div>
            }
            .into_any();
        }
        senders
            .into_iter()
            .map(|(name, _, has_mods, names)| {
                view! {
                    <div class="map-sender">
                        <div class="map-sender-name">{name.clone()}</div>
                        {if has_mods {
                            view! { <div class="map-mods">{mods(Some(name.clone()), names)}</div> }.into_any()
                        } else {
                            view! { <p class="hint map-help">"This sender sends no Mods."</p> }.into_any()
                        }}
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };
    let other_data = move || {
        let senders = shape.get();
        if senders.is_empty() {
            return view! {
                <p class="hint map-help">"No JTunnel sender is sending data."</p>
            }
            .into_any();
        }
        senders
            .into_iter()
            .map(|(name, keys, _, _)| {
                let keys = keys
                    .into_iter()
                    .filter(|k| k != MODS_KEY)
                    .map(|key| {
                        let from = name.clone();
                        let shown = key.clone();
                        let is_open = {
                            let (from, key) = (name.clone(), key.clone());
                            move || opened.with(|o| o.as_ref().is_some_and(|(s, k, _)| *s == from && *k == key))
                        };
                        view! {
                            <button type="button" class="map-key" class:on=is_open on:click=move |_| open_key(from.clone(), key.clone())>
                                {shown}
                            </button>
                        }
                    })
                    .collect_view();
                let fields = {
                    let name = name.clone();
                    move || {
                        let (from, key, fields) = opened.get().filter(|(s, _, _)| *s == name)?;
                        let rows = fields
                            .into_iter()
                            .map(|(field, value)| {
                                let reader = data_reader(Some(&from), &key, &field);
                                let is_chosen = {
                                    let reader = reader.clone();
                                    move || chosen(&reader)
                                };
                                let shown = if field.is_empty() { "(the value)".to_owned() } else { field };
                                view! {
                                    <button type="button" class="menuitem map-field" class:on=is_chosen on:click=move |_| pick(reader.clone())>
                                        <span class="ctx-label mono">{shown}</span>
                                        <span class="hint mono">{value}</span>
                                    </button>
                                }
                            })
                            .collect_view();
                        Some(view! {
                            <div class="ctx-head">{format!("Fields of {key}, as they were when opened")}</div>
                            <div class="map-list">{rows}</div>
                        })
                    }
                };
                view! {
                    <div class="map-sender">
                        <div class="map-sender-name">{name.clone()}</div>
                        <div class="map-keys">{keys}</div>
                        {fields}
                    </div>
                }
            })
            .collect_view()
            .into_any()
    };

    // MIDI: the controls seen, and one typed by hand.
    let typed_control = RwSignal::new(MotionMidiControl::Cc);
    let typed_number = RwSignal::new(1.0_f64);
    let typed_channel = RwSignal::new(0.0_f64);
    let typed_input = RwSignal::new(String::new());
    let use_typed = move |_| {
        pick(midi_reader(
            typed_control.get_untracked(),
            typed_number.get_untracked() as u8,
            typed_channel.get_untracked() as u8,
            &typed_input.get_untracked(),
        ));
    };
    // The controls seen, without their values, so a row isn't drawn again while its
    // control moves.
    let seen_controls = Memo::new(move |_| {
        live.with(|l| {
            l.controls
                .iter()
                .map(|c| (c.input.clone(), c.channel, c.controller))
                .collect::<Vec<_>>()
        })
    });
    let seen = move || {
        let controls = seen_controls.get();
        if controls.is_empty() {
            return view! { <p class="hint map-help">"No MIDI control has moved yet. Move one, or press Learn."</p> }.into_any();
        }
        controls
            .into_iter()
            .map(|(input, channel, controller)| {
                let reader = midi_reader(MotionMidiControl::Cc, controller, channel, &input);
                let is_chosen = {
                    let reader = reader.clone();
                    move || chosen(&reader)
                };
                let value = {
                    let input = input.clone();
                    move || {
                        live.with(|l| {
                            l.controls
                                .iter()
                                .find(|c| c.input == input && c.channel == channel && c.controller == controller)
                                .map_or(0, |c| c.value)
                        })
                    }
                };
                let level = {
                    let value = value.clone();
                    move || f32::from(value()) / 127.0
                };
                view! {
                    <button type="button" class="menuitem map-field" class:on=is_chosen on:click=move |_| pick(reader.clone())>
                        <span class="ctx-label">{format!("CC {controller} · channel {channel} · {input}")}</span>
                        {bar(level)}
                        <span class="hint mono">{value}</span>
                    </button>
                }
            })
            .collect_view()
            .into_any()
    };
    let set_typed_control = move |e: leptos::ev::Event| {
        typed_control.set(match event_target_value(&e).as_str() {
            "note" => MotionMidiControl::Note,
            "bend" => MotionMidiControl::PitchBend,
            "pressure" => MotionMidiControl::Pressure,
            _ => MotionMidiControl::Cc,
        });
    };
    let numbered = Signal::derive(move || {
        matches!(
            typed_control.get(),
            MotionMidiControl::Cc | MotionMidiControl::Note
        )
        .then(|| typed_number.get())
    });

    // Channel.
    let pick_channel = move |channel_id: String| {
        pick(channel_reader(&channel_id, range));
    };
    let make_channel = move |name: String| {
        let scene = studio.selection.with_untracked(|s| s.scene.clone());
        let at = request
            .with_value(|r| r.current)
            .map_or(0.0, |v| fraction(v, range));
        let channel = studio.collection.with_untracked(|c| {
            new_channel(
                &name,
                scene,
                at,
                c.as_ref().map_or(&[][..], |c| &c.channels[..]),
            )
        });
        let id = channel.channel_id.clone();
        pending.set(Some(channel));
        pick_channel(id);
    };

    // Hotkey.
    let chord = RwSignal::new(None::<Chord>);
    let capturing = RwSignal::new(false);
    let fade = RwSignal::new(300.0_f64);
    let key_ends = RwSignal::new({
        let current = request.with_value(|r| r.current);
        let on = current
            .filter(|v| (v - range.0).abs() > f64::EPSILON)
            .unwrap_or(range.1);
        (range.0, on)
    });
    let captured = move |c: Option<Chord>| {
        capturing.set(false);
        if c.is_some() {
            chord.set(c);
        }
    };
    let key_note = move || {
        let chord = chord.get()?;
        studio.collection.with(|c| {
            let c = c.as_ref()?;
            let key = crate::hotkeys::label(&chord);
            Some(match key_channel(c, &chord) {
                Some(id) => format!(
                    "{key} already switches the channel “{}”. This setting will follow it too, so the key moves both.",
                    c.channel_name(&id)
                ),
                None if key_taken(c, &chord) => {
                    format!("{key} is bound to something else, which this mapping replaces.")
                }
                None => format!(
                    "{key} will switch this setting between its off and on values, through a channel other settings can follow too."
                ),
            })
        })
    };
    // The Hotkey tab's range, as a reader the shared fields edit.
    let key_reader = Signal::derive(move || {
        let (off, on) = key_ends.get();
        Some(with_ends(
            data_reader(None, "key", ""),
            off as f32,
            on as f32,
        ))
    });

    let ready = move || match tab.get() {
        Tab::Key => chord.with(Option::is_some),
        _ => draft.with(Option::is_some),
    };
    let map = move |_| {
        let Some(s) = session.0.get_value() else {
            return;
        };
        let edits = if tab.get_untracked() == Tab::Key {
            let Some(chord) = chord.get_untracked() else {
                return;
            };
            studio.collection.with_untracked(|c| {
                let c = c.as_ref()?;
                Some(request.with_value(|r| {
                    let ends = key_ends.get_untracked();
                    key_edits(
                        c,
                        &KeyMapping {
                            chord: &chord,
                            target: &r.target,
                            label: &r.label,
                            scene: studio.selection.with_untracked(|s| s.scene.clone()),
                            ends,
                            fade_ms: fade.get_untracked() as u32,
                            starts_on: r
                                .current
                                .is_some_and(|v| (v - ends.1).abs() < (v - ends.0).abs()),
                        },
                    )
                }))
            })
        } else {
            draft.get_untracked().map(|input| {
                let mut edits = Vec::new();
                if let (Some(channel), MotionKind::Channel { channel_id, .. }) =
                    (pending.get_untracked(), &input)
                    && channel.channel_id == *channel_id
                {
                    edits.push(Edit::ChannelSet { channel });
                }
                let set = studio.collection.with_untracked(|c| {
                    let c = c.as_ref()?;
                    Some(request.with_value(|r| {
                        // A mapping from the same source keeps the channel scaling it.
                        let amount = r.target.mapping_from(c, &input).and_then(|m| m.amount);
                        r.target.set(c, input, amount, None)
                    }))
                });
                edits.extend(set.unwrap_or_default());
                edits
            })
        };
        if let Some(edits) = edits {
            studio.apply(&s, edits);
            close();
        }
    };
    let remove = move |key: String| {
        let edits = studio.collection.with_untracked(|c| {
            c.as_ref()
                .map(|c| request.with_value(|r| r.target.clear(c, &key)))
                .unwrap_or_default()
        });
        if let (false, Some(s)) = (edits.is_empty(), session.0.get_value()) {
            studio.apply(&s, edits);
        }
        if !adds {
            draft.set(None);
        }
    };
    let value_line = request.with_value(|r| match r.current {
        Some(v) => format!(
            "Value {}",
            crate::panels::number::format_number(v, 2, false)
        ),
        None => String::new(),
    });
    // The setting's mapping as a row: its source, the range it maps onto, and Remove.
    let mapped_rows = move || {
        let all = mapped();
        if all.is_empty() {
            return None;
        }
        let rows = all
            .into_iter()
            .map(|mapping| {
                let name = studio
                    .collection
                    .with(|c| reader_name(c.as_ref(), &mapping.input));
                let onto = ends(&mapping.input).map_or_else(String::new, |(low, high)| {
                    let n = |v: f32| crate::panels::number::format_number(f64::from(v), 2, false);
                    format!("{} → {}", n(low), n(high))
                });
                let key = mapping.key;
                view! {
                    <li class="chan-row">
                        <span class="chan-row-name">{name}</span>
                        <span class="hint mono">{onto}</span>
                        <span class="hint">{if adds { "Offset" } else { "Override" }}</span>
                        <button type="button" class="chan-x" title="Remove this mapping" aria-label="Remove this mapping"
                            on:click=move |_| remove(key.clone())>
                            "×"
                        </button>
                    </li>
                }
            })
            .collect_view();
        Some(view! {
            <div class="section-head"><span class="eyebrow">"Mapped to"</span></div>
            <ul class="chan-rows">{rows}</ul>
        })
    };
    // What Use does, in words, when the draft isn't the mapping the setting has.
    let replaces = move || {
        if adds {
            return None;
        }
        let binding = bound()?;
        let same = match tab.get() {
            Tab::Key => chord.with(Option::is_none),
            _ => draft.with(|d| d.as_ref().is_none_or(|d| same_source(d, &binding.input))),
        };
        (!same).then(|| {
            let name = studio
                .collection
                .with(|c| reader_name(c.as_ref(), &binding.input));
            format!("A setting takes one source that overrides it: Use replaces {name}.")
        })
    };
    let draft_line = move || {
        draft.with(|d| {
            d.as_ref().map(|d| {
                let pending_name = pending.with(|p| {
                    p.as_ref().and_then(|p| match d {
                        MotionKind::Channel { channel_id, .. } if *channel_id == p.channel_id => {
                            Some(format!("new channel “{}”", p.name))
                        }
                        _ => None,
                    })
                });
                pending_name
                    .unwrap_or_else(|| studio.collection.with(|c| reader_name(c.as_ref(), d)))
            })
        })
    };
    let tabs = Tab::ALL
        .into_iter()
        .map(|(t, label)| {
            view! {
                <button type="button" role="tab" class:on=move || tab.get() == t aria-selected=move || (tab.get() == t).to_string()
                    on:click=move |_| tab.set(t)>
                    {label}
                </button>
            }
        })
        .collect_view();
    let title = request.with_value(|r| format!("Map control: {}", r.label));

    view! {
        <Modal label="Map control" class="map-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">{title}</h2>
                    <p class="modal-subtitle">{value_line}</p>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>"×"</button>
            </div>
            <div class="map-body">
                {mapped_rows}
                <div class="section-head"><span class="eyebrow">{move || if bound().is_some() { "Change the source" } else { "Choose a source" }}</span></div>
                <div class="map-top">
                    <div class="segmented map-tabs" role="tablist" aria-label="Source">{tabs}</div>
                    <button type="button" class="secondary map-learn" class:learning=learning on:click=learn
                        title="Move a knob, fader or button on a MIDI controller, or a Mod in the DAW: the first to move is chosen">
                        {move || if learning.get() { "Move a control…" } else { "Learn" }}
                    </button>
                </div>
                {heard_strip}
                <div class="map-pane">
                    {move || match tab.get() {
                        Tab::Jtunnel => view! {
                            <label class="check map-any">
                                <input type="checkbox" class="inline-check" prop:checked=any_sender
                                    on:change=move |e| any_sender.set(event_target_checked(&e)) />
                                "Follow this Mod from any sender"
                            </label>
                            {jtunnel}
                        }.into_any(),
                        Tab::Data => view! {
                            <p class="hint map-help">"Any number, switch or text a sender is sending. Open a key to see its fields."</p>
                            {other_data}
                        }.into_any(),
                        Tab::Midi => view! {
                            <div class="ctx-head">"Moved last, by input"</div>
                            <div class="map-list">{seen}</div>
                            <div class="ctx-head">"Or name one"</div>
                            <div class="map-typed">
                                <select aria-label="Kind of control" on:change=set_typed_control>
                                    <option value="cc">"Control change"</option>
                                    <option value="note">"Note (held)"</option>
                                    <option value="bend">"Pitch bend"</option>
                                    <option value="pressure">"Pressure"</option>
                                </select>
                                <NumberField named=Signal::derive(|| "Number".to_owned()) value=numbered step=1.0 decimals=0 min=0.0 max=127.0
                                    send=move |v| { typed_number.set(v); true } />
                                <NumberField named=Signal::derive(|| "MIDI channel, 0 for any".to_owned()) unit="ch" value=Signal::derive(move || Some(typed_channel.get())) step=1.0 decimals=0 min=0.0 max=16.0
                                    send=move |v| { typed_channel.set(v); true } />
                                <input type="text" class="map-input" placeholder="Any input" aria-label="Which MIDI input"
                                    prop:value=move || typed_input.get()
                                    on:change=move |e| typed_input.set(event_target_value(&e)) />
                                <button type="button" class="secondary" on:click=use_typed>"Use"</button>
                            </div>
                        }.into_any(),
                        Tab::Channel => view! {
                            <ChannelSearch placeholder="Follow a channel" pick=pick_channel make=make_channel values=values />
                        }.into_any(),
                        Tab::Key => view! {
                            <div class="map-key-row">
                                {move || if capturing.get() {
                                    view! { <KeyCapture done=captured /> }.into_any()
                                } else {
                                    view! {
                                        <button type="button" class="secondary" on:click=move |_| capturing.set(true)>
                                            {move || chord.get().map_or("Press to set a key…".to_owned(), |c| crate::hotkeys::label(&c))}
                                        </button>
                                    }.into_any()
                                }}
                                <NumberField label="Fade" unit="ms" value=Signal::derive(move || Some(fade.get())) step=50.0 decimals=0 min=0.0 max=60000.0
                                    send=move |v| { fade.set(v); true } />
                            </div>
                            <p class="hint map-help">{key_note}</p>
                        }.into_any(),
                    }}
                </div>
                <div class="map-foot">
                    <p class="map-chosen">
                        {move || match tab.get() {
                            Tab::Key => chord.get().map_or("No key yet".to_owned(), |c| format!("Key {}", crate::hotkeys::label(&c))),
                            _ => draft_line().unwrap_or_else(|| "No source chosen".to_owned()),
                        }}
                    </p>
                    {move || if tab.get() == Tab::Key {
                        view! {
                            <ReaderFields kind=key_reader limits=limits names=("Off", "On")
                                send=move |k| { if let Some((off, on)) = ends(&k) { key_ends.set((f64::from(off), f64::from(on))); } } />
                        }.into_any()
                    } else {
                        view! { <ReaderFields kind=draft limits=limits send=move |k| draft.set(Some(k)) /> }.into_any()
                    }}
                </div>
            </div>
            <div class="modal-footer">
                <span class="hint footer-left">{replaces}</span>
                <button type="button" class="secondary" on:click=move |_| close()>"Cancel"</button>
                <button type="button" class="secondary"
                    title="Close this and arm the source: then drag up or down on any setting to map the source onto it by an amount"
                    disabled=move || tab.get() == Tab::Key || draft.with(Option::is_none)
                    on:click=move |_| {
                        if let (Some(mode), Some(reader)) = (mapping_mode, draft.get_untracked()) {
                            mode.arm(reader);
                            close();
                        }
                    }>
                    "Map by dragging"
                </button>
                <button type="button" class="primary" data-primary disabled=move || !ready() on:click=map>"Use"</button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{BusId, HotkeyBinding, SourceId};

    fn target() -> ValueTarget {
        ValueTarget::BusGain {
            bus_id: BusId::master(),
        }
    }

    fn collection() -> Collection {
        Collection::default()
    }

    #[test]
    fn a_new_source_keeps_the_range_smoothing_and_curve_already_set() {
        let first = picked(mod_reader(Some("Bitwig"), 19), None, (-60.0, 6.0));
        assert_eq!(ends(&first), Some((-60.0, 6.0)));
        let mut shaped = with_curve(with_ends(first, -20.0, 0.0), MotionCurve::EaseOut);
        set_smoothing(&mut shaped, 400);
        let next = picked(
            midi_reader(MotionMidiControl::Cc, 21, 0, ""),
            Some(&shaped),
            (-60.0, 6.0),
        );
        assert!(matches!(
            next,
            MotionKind::Midi {
                controller: Some(21),
                ..
            }
        ));
        assert_eq!(ends(&next), Some((-20.0, 0.0)));
        assert_eq!(smoothing_of(&next), Some(400));
        assert_eq!(curve_of(&next), Some(MotionCurve::EaseOut));
    }

    #[test]
    fn readers_of_one_source_match_whatever_their_shape() {
        let a = mod_reader(Some("Bitwig"), 3);
        let b = with_ends(mod_reader(Some("Bitwig"), 3), 5.0, 9.0);
        assert!(same_source(&a, &b));
        assert!(!same_source(&a, &mod_reader(Some("Bitwig"), 4)));
        assert!(!same_source(&a, &mod_reader(None, 3)));
        let cc = midi_reader(MotionMidiControl::Cc, 7, 1, "Pads");
        assert!(same_source(
            &cc,
            &with_curve(cc.clone(), MotionCurve::EaseIn)
        ));
        assert!(!same_source(
            &cc,
            &midi_reader(MotionMidiControl::Note, 7, 1, "Pads")
        ));
        assert!(!same_source(&cc, &a));
    }

    #[test]
    fn a_typed_midi_control_is_a_valid_reader() {
        let note = midi_reader(MotionMidiControl::Note, 36, 10, " Pads ");
        assert!(matches!(
            &note,
            MotionKind::Midi { control: MotionMidiControl::Note, controller: Some(36), channel: Some(10), input: Some(i), .. } if i == "Pads"
        ));
        let bend = midi_reader(MotionMidiControl::PitchBend, 5, 0, "");
        assert!(matches!(
            bend,
            MotionKind::Midi {
                controller: None,
                channel: None,
                input: None,
                ..
            }
        ));
        for reader in [
            note,
            bend,
            data_reader(Some("Bitwig"), "transport", "playing"),
        ] {
            assert_eq!(binding_for(&target(), reader, None).problem(), None);
        }
    }

    #[test]
    fn a_mapping_is_named_after_its_setting_and_keeps_the_amount() {
        let binding = binding_for(&target(), mod_reader(None, 0), Some("depth".into()));
        assert_eq!(binding.binding_id, learn_id(&target()));
        assert_eq!(binding.amount.as_deref(), Some("depth"));
        assert_eq!(binding.problem(), None);
    }

    #[test]
    fn a_free_key_makes_a_channel_it_switches_and_the_setting_follows() {
        let c = collection();
        let chord = Chord::new("F5");
        let target = target();
        let edits = key_edits(
            &c,
            &KeyMapping {
                chord: &chord,
                target: &MapTarget::Value(target.clone()),
                label: "Master fader",
                scene: None,
                ends: (-60.0, 0.0),
                fade_ms: 300,
                starts_on: true,
            },
        );
        let [
            Edit::ChannelSet { channel },
            Edit::BindingSetValue { binding },
            Edit::HotkeySet {
                chord: bound,
                action,
            },
        ] = &edits[..]
        else {
            panic!("a channel, a binding and a hotkey: {edits:?}");
        };
        assert_eq!(channel.name, "Master fader");
        assert_eq!(
            (channel.rise_ms, channel.fall_ms, channel.on),
            (300, 300, true)
        );
        assert_eq!(channel.default, Finite32::ONE);
        assert_eq!(channel.problem(), None);
        assert!(
            matches!(&binding.input, MotionKind::Channel { channel_id, .. } if *channel_id == channel.channel_id)
        );
        assert_eq!(ends(&binding.input), Some((-60.0, 0.0)));
        assert_eq!(bound, &chord);
        assert!(
            matches!(action, Action::ChannelSwitch { channel_id, to: ChannelSwitch::Toggle } if *channel_id == channel.channel_id)
        );
    }

    #[test]
    fn a_key_that_switches_a_channel_takes_a_second_setting_onto_it() {
        let mut c = collection();
        let chord = Chord::new("F5");
        c.hotkeys.push(HotkeyBinding::new(
            chord.clone(),
            &Action::ChannelSwitch {
                channel_id: "stage_power".into(),
                to: ChannelSwitch::Toggle,
            },
        ));
        assert_eq!(key_channel(&c, &chord).as_deref(), Some("stage_power"));
        assert!(!key_taken(&c, &chord));
        let second = ValueTarget::SourceGain {
            source_id: SourceId::new("mic"),
        };
        let edits = key_edits(
            &c,
            &KeyMapping {
                chord: &chord,
                target: &MapTarget::Value(second.clone()),
                label: "Mic volume",
                scene: None,
                ends: (-60.0, 6.0),
                fade_ms: 0,
                starts_on: false,
            },
        );
        let [Edit::BindingSetValue { binding }] = &edits[..] else {
            panic!("only the setting's binding: {edits:?}");
        };
        assert!(
            matches!(&binding.input, MotionKind::Channel { channel_id, .. } if channel_id == "stage_power")
        );
        assert_eq!(binding.target, second);
    }

    #[test]
    fn a_key_bound_to_something_else_is_taken_and_rebound() {
        let mut c = collection();
        let chord = Chord::new("F6");
        let scene = SceneId::new("scn_1");
        c.hotkeys.push(HotkeyBinding::new(
            chord.clone(),
            &crate::hotkeys::switch_to(&scene),
        ));
        assert!(key_taken(&c, &chord));
        let target = target();
        let edits = key_edits(
            &c,
            &KeyMapping {
                chord: &chord,
                target: &MapTarget::Value(target.clone()),
                label: "Fader",
                scene: None,
                ends: (0.0, 1.0),
                fade_ms: 0,
                starts_on: false,
            },
        );
        assert!(matches!(
            &edits[..],
            [
                Edit::ChannelSet { .. },
                Edit::BindingSetValue { .. },
                Edit::HotkeyClear { .. },
                Edit::HotkeySet { .. }
            ]
        ));
    }
}
