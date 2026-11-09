//! The Inspector's Motion section (ADR-0099): the selected item's motion modifiers,
//! each a row with what it moves, how (wiggle, oscillate, drift or shake, with that
//! kind's parameters, or audio: what it hears and measures, ADR-0100), its clock and
//! whether it is on, and Add and Remove. A row on the trigger clock has a Fire button,
//! which sends `item.trigger_motion` for it (#1107). Every change sends
//! `item.set_motion` with the whole list, addressed to the shown scene, so in a keyframe
//! scene it writes the keyframe's own list. The engine draws the motion; the canvas
//! shows the item where its values put it.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/UI-FEATURES.md#inspector

use super::inspector::Fold;
use super::number::NumberField;
use crate::app::SessionContext;
use crate::store::Studio;
use jstream_control::protocol::{
    Action, BusId, Edit, Finite32, ItemId, MAX_MOTION_MODIFIERS, MotionClock, MotionCurve,
    MotionKind, MotionMeasure, MotionModifier, MotionProperty, MotionShape, SceneId, SourceId,
};
use leptos::prelude::*;

/// What each property is called in the section.
pub fn property_name(property: MotionProperty) -> &'static str {
    match property {
        MotionProperty::X => "X",
        MotionProperty::Y => "Y",
        MotionProperty::Scale => "Scale",
        MotionProperty::Rotation => "Rotation",
        MotionProperty::Tilt => "Tilt",
        MotionProperty::Turn => "Turn",
        MotionProperty::Depth => "Depth",
        MotionProperty::Opacity => "Opacity",
        MotionProperty::CropLeft => "Crop left",
        MotionProperty::CropTop => "Crop top",
        MotionProperty::CropRight => "Crop right",
        MotionProperty::CropBottom => "Crop bottom",
        // An effect's setting (ADR-0138): shown, but not offered here yet.
        MotionProperty::Effect => "Effect setting",
        // One from a newer engine: shown, but not offered.
        _ => "Other",
    }
}

/// The unit a property's amplitude, rate or wrap is in.
pub fn property_unit(property: MotionProperty) -> &'static str {
    match property {
        MotionProperty::Rotation | MotionProperty::Tilt | MotionProperty::Turn => "°",
        // A factor: 0.1 is ±10%.
        MotionProperty::Scale | MotionProperty::Opacity => "×",
        _ => "px",
    }
}

/// The kinds a row can switch between, by name.
pub const KINDS: [&str; 5] = ["Wiggle", "Oscillate", "Drift", "Shake", "Audio"];

/// What an audio modifier can measure, by name.
pub const MEASURES: [(MotionMeasure, &str); 4] = [
    (MotionMeasure::Level, "Level"),
    (MotionMeasure::Peak, "Peak"),
    (MotionMeasure::Onset, "Beat"),
    (MotionMeasure::Band, "Band"),
];

/// How an audio modifier can bend its envelope, by name.
pub const CURVES: [(MotionCurve, &str); 4] = [
    (MotionCurve::Linear, "Linear"),
    (MotionCurve::EaseIn, "Ease in"),
    (MotionCurve::EaseOut, "Ease out"),
    (MotionCurve::EaseInOut, "Ease in-out"),
];

/// The shapes an oscillator can have, by name.
pub const SHAPES: [(MotionShape, &str); 4] = [
    (MotionShape::Sine, "Sine"),
    (MotionShape::Triangle, "Triangle"),
    (MotionShape::Square, "Square"),
    (MotionShape::Saw, "Saw"),
];

/// `kind`'s name in [`KINDS`].
pub fn kind_name(kind: &MotionKind) -> &'static str {
    match kind {
        MotionKind::Wiggle { .. } => "Wiggle",
        MotionKind::Oscillate { .. } => "Oscillate",
        MotionKind::Drift { .. } => "Drift",
        MotionKind::Impulse { .. } => "Shake",
        MotionKind::Audio { .. } => "Audio",
        _ => "Other",
    }
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).unwrap_or(Finite32::ZERO)
}

/// A new modifier of the kind named `name`, with a gentle default for `property`: a few
/// pixels or a fraction of a degree, as an ambient sway.
pub fn default_kind(name: &str, property: MotionProperty) -> Option<MotionKind> {
    let size = match property_unit(property) {
        "°" => 0.5,
        "×" => 0.05,
        _ => 3.0,
    };
    if name == "Audio" {
        // Follows the program mix's level: a light dims between beats, anything else
        // pushes by a little more than a sway would.
        let (from, to) = match property {
            MotionProperty::Opacity => (-0.6, 0.0),
            _ => (0.0, size * 2.0),
        };
        return Some(MotionKind::Audio {
            audio: None,
            audio_source: None,
            measure: MotionMeasure::Level,
            low_hz: None,
            high_hz: None,
            floor_db: finite(-60.0),
            ceiling_db: Finite32::ZERO,
            attack_ms: 10,
            release_ms: 250,
            curve: MotionCurve::Linear,
            from: finite(from),
            to: finite(to),
        });
    }
    sized_kind(name, size)
}

/// A new modifier of the kind named `name` whose amount is `size` (a drift's rate is
/// ten times it a second).
pub fn sized_kind(name: &str, size: f32) -> Option<MotionKind> {
    Some(match name {
        "Wiggle" => MotionKind::Wiggle {
            amplitude: finite(size),
            frequency_hz: finite(0.5),
            seed: 0,
            octaves: 1,
        },
        "Oscillate" => MotionKind::Oscillate {
            shape: MotionShape::Sine,
            amplitude: finite(size),
            period_ms: 4000,
            phase: Finite32::ZERO,
        },
        "Drift" => MotionKind::Drift {
            rate_per_s: finite(size * 10.0),
            wrap: None,
        },
        // A short jolt that dies away: bigger than a sway, since it is brief.
        "Shake" => MotionKind::Impulse {
            amplitude: finite(size * 4.0),
            frequency_hz: finite(6.0),
            decay_per_s: finite(4.0),
            duration_ms: 700,
        },
        _ => return None,
    })
}

/// `list` with a new wiggle on x at its end, under an ID it doesn't use yet; `None` when
/// it is full.
pub fn with_added(list: &[MotionModifier]) -> Option<Vec<MotionModifier>> {
    if list.len() >= MAX_MOTION_MODIFIERS {
        return None;
    }
    let id = (1..)
        .map(|n| format!("motion-{n}"))
        .find(|id| list.iter().all(|m| m.modifier_id != *id))?;
    let mut next = list.to_vec();
    next.push(MotionModifier {
        modifier_id: id,
        property: MotionProperty::X,
        effect: None,
        kind: default_kind("Wiggle", MotionProperty::X)?,
        clock: MotionClock::Engine,
        enabled: true,
    });
    Some(next)
}

/// One of a row's number fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    /// A wiggle's or oscillator's size.
    Amplitude,
    /// A wiggle's turns of direction a second.
    Frequency,
    /// A wiggle's noise.
    Seed,
    /// A wiggle's detail.
    Octaves,
    /// An oscillator's period.
    Period,
    /// An oscillator's start within its period, as a fraction.
    Phase,
    /// A drift's rate a second.
    Rate,
    /// Where a drift wraps back to 0; 0 for never.
    Wrap,
    /// How fast a shake dies away, a second.
    Decay,
    /// How long a shake lasts.
    Duration,
    /// An audio modifier's value at silence.
    From,
    /// Its value at the ceiling.
    To,
    /// How fast it rises.
    Attack,
    /// How fast it falls.
    Release,
    /// The level that maps to its `from`.
    Floor,
    /// The level that maps to its `to`.
    Ceiling,
    /// A band's lowest frequency.
    LowHz,
    /// A band's highest frequency.
    HighHz,
}

impl Param {
    /// The fields a kind has, in the order the row shows them.
    pub fn of(kind: &MotionKind) -> &'static [Param] {
        match kind {
            MotionKind::Wiggle { .. } => &[
                Param::Amplitude,
                Param::Frequency,
                Param::Octaves,
                Param::Seed,
            ],
            MotionKind::Oscillate { .. } => &[Param::Amplitude, Param::Period, Param::Phase],
            MotionKind::Drift { .. } => &[Param::Rate, Param::Wrap],
            MotionKind::Audio {
                measure: MotionMeasure::Band,
                ..
            } => &[
                Param::From,
                Param::To,
                Param::LowHz,
                Param::HighHz,
                Param::Floor,
                Param::Ceiling,
                Param::Attack,
                Param::Release,
            ],
            MotionKind::Audio {
                measure: MotionMeasure::Onset,
                ..
            } => &[Param::From, Param::To, Param::Attack, Param::Release],
            MotionKind::Audio { .. } => &[
                Param::From,
                Param::To,
                Param::Floor,
                Param::Ceiling,
                Param::Attack,
                Param::Release,
            ],
            MotionKind::Impulse { .. } => &[
                Param::Amplitude,
                Param::Frequency,
                Param::Decay,
                Param::Duration,
            ],
            _ => &[],
        }
    }

    /// Its label, step, decimals and range.
    pub fn field(self) -> (&'static str, f64, usize, f64, f64) {
        match self {
            Param::Amplitude => ("Amount", 0.1, 2, 0.0, 100_000.0),
            Param::Frequency => ("Speed", 0.05, 2, 0.01, 60.0),
            Param::Seed => ("Seed", 1.0, 0, 0.0, f64::from(u32::MAX)),
            Param::Octaves => ("Detail", 1.0, 0, 1.0, 4.0),
            Param::Period => ("Period", 100.0, 0, 1.0, 3_600_000.0),
            Param::Phase => ("Phase", 0.05, 2, 0.0, 0.99),
            Param::Rate => ("Rate", 1.0, 2, -100_000.0, 100_000.0),
            Param::Wrap => ("Wrap", 10.0, 1, 0.0, 100_000.0),
            Param::Decay => ("Decay", 0.5, 1, 0.0, 1000.0),
            Param::Duration => ("Length", 50.0, 0, 1.0, 3_600_000.0),
            Param::From => ("At silence", 0.05, 2, -100_000.0, 100_000.0),
            Param::To => ("At loudest", 0.05, 2, -100_000.0, 100_000.0),
            Param::Attack => ("Attack", 5.0, 0, 0.0, 10_000.0),
            Param::Release => ("Release", 10.0, 0, 0.0, 10_000.0),
            Param::Floor => ("Floor", 1.0, 0, -90.0, -1.0),
            Param::Ceiling => ("Ceiling", 1.0, 0, -89.0, 0.0),
            Param::LowHz => ("Low", 10.0, 0, 20.0, 19_999.0),
            Param::HighHz => ("High", 10.0, 0, 21.0, 20_000.0),
        }
    }
}

/// `kind`'s value of `param`, or `None` when it has no such field.
pub fn param_value(kind: &MotionKind, param: Param) -> Option<f64> {
    let f = |v: Finite32| Some(f64::from(v.get()));
    match (kind.clone(), param) {
        (MotionKind::Wiggle { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Oscillate { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Impulse { amplitude, .. }, Param::Amplitude) => f(amplitude),
        (MotionKind::Wiggle { frequency_hz, .. }, Param::Frequency)
        | (MotionKind::Impulse { frequency_hz, .. }, Param::Frequency) => f(frequency_hz),
        (MotionKind::Impulse { decay_per_s, .. }, Param::Decay) => f(decay_per_s),
        (MotionKind::Impulse { duration_ms, .. }, Param::Duration) => Some(f64::from(duration_ms)),
        (MotionKind::Wiggle { seed, .. }, Param::Seed) => Some(f64::from(seed)),
        (MotionKind::Wiggle { octaves, .. }, Param::Octaves) => Some(f64::from(octaves)),
        (MotionKind::Oscillate { period_ms, .. }, Param::Period) => Some(f64::from(period_ms)),
        (MotionKind::Oscillate { phase, .. }, Param::Phase) => f(phase),
        (MotionKind::Drift { rate_per_s, .. }, Param::Rate) => f(rate_per_s),
        (MotionKind::Drift { wrap, .. }, Param::Wrap) => {
            Some(wrap.map_or(0.0, |w| f64::from(w.get())))
        }
        (MotionKind::Audio { from, .. }, Param::From) => f(from),
        (MotionKind::Audio { to, .. }, Param::To) => f(to),
        (MotionKind::Audio { attack_ms, .. }, Param::Attack) => Some(f64::from(attack_ms)),
        (MotionKind::Audio { release_ms, .. }, Param::Release) => Some(f64::from(release_ms)),
        (MotionKind::Audio { floor_db, .. }, Param::Floor) => f(floor_db),
        (MotionKind::Audio { ceiling_db, .. }, Param::Ceiling) => f(ceiling_db),
        (MotionKind::Audio { low_hz, .. }, Param::LowHz) => low_hz.and_then(f),
        (MotionKind::Audio { high_hz, .. }, Param::HighHz) => high_hz.and_then(f),
        _ => None,
    }
}

/// `kind` with `param` set to `value`, or `None` when it has no such field. A wrap of 0
/// is no wrap.
pub fn with_param(kind: &MotionKind, param: Param, value: f64) -> Option<MotionKind> {
    let v = finite(value as f32);
    let mut next = kind.clone();
    match (&mut next, param) {
        (MotionKind::Wiggle { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Oscillate { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Impulse { amplitude, .. }, Param::Amplitude) => *amplitude = v,
        (MotionKind::Wiggle { frequency_hz, .. }, Param::Frequency)
        | (MotionKind::Impulse { frequency_hz, .. }, Param::Frequency) => *frequency_hz = v,
        (MotionKind::Impulse { decay_per_s, .. }, Param::Decay) => *decay_per_s = v,
        (MotionKind::Impulse { duration_ms, .. }, Param::Duration) => {
            *duration_ms = value.max(1.0) as u32
        }
        (MotionKind::Wiggle { seed, .. }, Param::Seed) => *seed = value.max(0.0) as u32,
        (MotionKind::Wiggle { octaves, .. }, Param::Octaves) => {
            *octaves = value.clamp(1.0, 4.0) as u8
        }
        (MotionKind::Oscillate { period_ms, .. }, Param::Period) => {
            *period_ms = value.max(1.0) as u32
        }
        (MotionKind::Oscillate { phase, .. }, Param::Phase) => *phase = v,
        (MotionKind::Drift { rate_per_s, .. }, Param::Rate) => *rate_per_s = v,
        (MotionKind::Drift { wrap, .. }, Param::Wrap) => {
            *wrap = (value > 0.0).then_some(v);
        }
        (MotionKind::Audio { from, .. }, Param::From) => *from = v,
        (MotionKind::Audio { to, .. }, Param::To) => *to = v,
        (MotionKind::Audio { attack_ms, .. }, Param::Attack) => {
            *attack_ms = value.clamp(0.0, 10_000.0) as u32
        }
        (MotionKind::Audio { release_ms, .. }, Param::Release) => {
            *release_ms = value.clamp(0.0, 10_000.0) as u32
        }
        (MotionKind::Audio { floor_db, .. }, Param::Floor) => *floor_db = v,
        (MotionKind::Audio { ceiling_db, .. }, Param::Ceiling) => *ceiling_db = v,
        (MotionKind::Audio { low_hz, .. }, Param::LowHz) => *low_hz = Some(v),
        (MotionKind::Audio { high_hz, .. }, Param::HighHz) => *high_hz = Some(v),
        _ => return None,
    }
    Some(next)
}

/// What the Hears select shows for an audio `kind`: empty for the program mix, a bus's
/// ID, or `source:` and the ID of a source it hears instead.
pub fn hears_value(kind: &MotionKind) -> String {
    match kind {
        MotionKind::Audio {
            audio_source: Some(source),
            ..
        } => format!("source:{}", source.as_str()),
        MotionKind::Audio {
            audio: Some(bus), ..
        } => bus.as_str().to_owned(),
        _ => String::new(),
    }
}

/// An audio `kind` hearing what the Hears select's `value` names ([`hears_value`]):
/// picking the program mix or a bus stops it hearing a source. `None` when it isn't
/// audio or already hears that.
pub fn with_hears(kind: &MotionKind, value: &str) -> Option<MotionKind> {
    if hears_value(kind) == value {
        return None;
    }
    let mut kind = kind.clone();
    let MotionKind::Audio {
        audio,
        audio_source,
        ..
    } = &mut kind
    else {
        return None;
    };
    match value.strip_prefix("source:") {
        Some(source) => *audio_source = Some(SourceId::new(source)),
        None => {
            *audio_source = None;
            *audio = (!value.is_empty()).then(|| BusId::new(value));
        }
    }
    Some(kind)
}

/// An audio `kind` measuring `measure` instead: a band gets a bass band to start from,
/// and any other measure drops the band's edges. `None` when it isn't audio or already
/// measures that.
pub fn with_measure(kind: &MotionKind, next: MotionMeasure) -> Option<MotionKind> {
    let mut kind = kind.clone();
    let MotionKind::Audio {
        measure,
        low_hz,
        high_hz,
        ..
    } = &mut kind
    else {
        return None;
    };
    if *measure == next {
        return None;
    }
    *measure = next;
    if next == MotionMeasure::Band {
        *low_hz = Some(low_hz.unwrap_or(finite(40.0)));
        *high_hz = Some(high_hz.unwrap_or(finite(120.0)));
    } else {
        *low_hz = None;
        *high_hz = None;
    }
    Some(kind)
}

/// `kind` switched to the kind named `name`, keeping its size where both have one.
pub fn with_kind(kind: &MotionKind, name: &str, property: MotionProperty) -> Option<MotionKind> {
    if kind_name(kind) == name {
        return None;
    }
    let next = default_kind(name, property)?;
    Some(
        param_value(kind, Param::Amplitude)
            .and_then(|size| with_param(&next, Param::Amplitude, size))
            .unwrap_or(next),
    )
}

/// The section's hint while folded: how many modifiers there are.
pub fn hint(list: &[MotionModifier]) -> String {
    match list.len() {
        0 => String::new(),
        1 => "1 modifier".to_owned(),
        n => format!("{n} modifiers"),
    }
}

/// The Motion section of the item `item` as scene `scene` shows it.
#[component]
pub fn MotionSection(
    /// The scene shown: its own, or a keyframe of it.
    scene: SceneId,
    /// The selected item.
    item: ItemId,
) -> impl IntoView {
    let studio = expect_context::<Studio>();
    let session = expect_context::<SessionContext>();
    let ids = StoredValue::new((item, scene));
    // The list as the scene shows it: in a keyframe scene, its override if it has one.
    let list = Memo::new(move |_| {
        let (item, scene) = ids.get_value();
        studio.collection.with(|c| {
            c.as_ref()
                .and_then(|c| c.item_in(Some(&scene), &item))
                .map(|(_, i)| i.motion.clone())
                .unwrap_or_default()
        })
    });
    let send = move |next: Vec<MotionModifier>| {
        if list.with_untracked(|l| *l == next) {
            return false;
        }
        let Some(s) = session.get() else {
            return false;
        };
        let (item_id, scene) = ids.get_value();
        studio.apply(
            &s,
            vec![Edit::ItemSetMotion {
                item_id,
                scene_id: Some(scene),
                motion: next,
            }],
        );
        true
    };
    // `list` with row `n` changed by `change`.
    let change = move |n: usize, change: &dyn Fn(&mut MotionModifier) -> bool| {
        let mut next = list.get_untracked();
        let Some(m) = next.get_mut(n) else {
            return false;
        };
        change(m) && send(next)
    };
    let hint_text = Signal::derive(move || list.with(|l| hint(l)));
    // Fires row `n`'s modifier on program (#1107).
    let fire = move |n: usize| {
        let Some(modifier_id) = list.with_untracked(|l| l.get(n).map(|m| m.modifier_id.clone()))
        else {
            return;
        };
        let Some(s) = session.get() else {
            return;
        };
        let (item_id, _) = ids.get_value();
        leptos::task::spawn_local(async move {
            let fired = Action::ItemTriggerMotion {
                item_id,
                modifier_id: Some(modifier_id),
            };
            if let Err(error) = super::outputs::action(&*s.control, fired).await {
                leptos::logging::warn!("couldn't fire the motion: {error}");
            }
        });
    };

    let row = move |n: usize| {
        let at = move || list.with(|l| l.get(n).cloned());
        let fields = move || {
            let kind = at()?.kind;
            Some(
                Param::of(&kind)
                    .iter()
                    .map(|param| {
                        let param = *param;
                        let (label, step, decimals, min, max) = param.field();
                        let unit = match param {
                            Param::Amplitude | Param::Rate | Param::Wrap => {
                                at().map_or("", |m| property_unit(m.property))
                            }
                            Param::From | Param::To => {
                                at().map_or("", |m| property_unit(m.property))
                            }
                            Param::Frequency | Param::LowHz | Param::HighHz => "Hz",
                            Param::Period | Param::Attack | Param::Release => "ms",
                            Param::Floor | Param::Ceiling => "dB",
                            _ => "",
                        };
                        let value =
                            Signal::derive(move || at().and_then(|m| param_value(&m.kind, param)));
                        let commit = move |v: f64| {
                            change(n, &|m| match with_param(&m.kind, param, v) {
                                Some(kind) if kind != m.kind => {
                                    m.kind = kind;
                                    true
                                }
                                _ => false,
                            })
                        };
                        view! {
                            <NumberField label unit value step decimals min max send=commit />
                        }
                    })
                    .collect_view(),
            )
        };
        view! {
            <div class="motion-row">
                <div class="field-grid">
                    <label class="select-field">
                        <span class="field-label">"Moves"</span>
                        <select
                            aria-label="What it moves"
                            prop:value=move || at().map_or("", |m| property_name(m.property))
                            on:change=move |event| {
                                let name = event_target_value(&event);
                                change(n, &|m| {
                                    let Some(p) = MotionProperty::ALL
                                        .into_iter()
                                        .find(|p| property_name(*p) == name)
                                        .filter(|p| *p != m.property)
                                    else {
                                        return false;
                                    };
                                    m.property = p;
                                    // A placement property names no effect setting.
                                    m.effect = None;
                                    true
                                });
                            }
                        >
                            {MotionProperty::ALL
                                .into_iter()
                                .map(|p| view! { <option value=property_name(p)>{property_name(p)}</option> })
                                .collect_view()}
                            // An effect setting it drives (ADR-0138) shows as itself.
                            {move || {
                                at().filter(|m| m.property == MotionProperty::Effect).map(|_| {
                                    let name = property_name(MotionProperty::Effect);
                                    view! { <option value=name disabled=true>{name}</option> }
                                })
                            }}
                        </select>
                    </label>
                    <label class="select-field">
                        <span class="field-label">"How"</span>
                        <select
                            aria-label="How it moves"
                            prop:value=move || at().map_or("", |m| kind_name(&m.kind))
                            on:change=move |event| {
                                let name = event_target_value(&event);
                                change(n, &|m| match with_kind(&m.kind, &name, m.property) {
                                    Some(kind) => {
                                        m.kind = kind;
                                        true
                                    }
                                    None => false,
                                });
                            }
                        >
                            {KINDS
                                .into_iter()
                                .map(|k| view! { <option value=k>{k}</option> })
                                .collect_view()}
                        </select>
                    </label>
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Oscillate { .. }))>
                        <label class="select-field">
                            <span class="field-label">"Shape"</span>
                            <select
                                aria-label="Shape"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Oscillate { shape, .. } => SHAPES
                                        .iter()
                                        .find(|(s, _)| *s == shape)
                                        .map(|(_, name)| *name),
                                    _ => None,
                                }).unwrap_or("")
                                on:change=move |event| {
                                    let name = event_target_value(&event);
                                    let Some(next) = SHAPES
                                        .iter()
                                        .find(|(_, n)| *n == name)
                                        .map(|(s, _)| *s)
                                    else {
                                        return;
                                    };
                                    change(n, &|m| match &mut m.kind {
                                        MotionKind::Oscillate { shape, .. } if *shape != next => {
                                            *shape = next;
                                            true
                                        }
                                        _ => false,
                                    });
                                }
                            >
                                {SHAPES
                                    .iter()
                                    .map(|(_, name)| view! { <option value=*name>{*name}</option> })
                                    .collect_view()}
                            </select>
                        </label>
                    </Show>
                    <Show when=move || at().is_some_and(|m| matches!(m.kind, MotionKind::Audio { .. }))>
                        <label class="select-field">
                            <span class="field-label">"Hears"</span>
                            <select
                                aria-label="What it hears"
                                title="The program mix as the outputs hear it, or one bus after its fader"
                                prop:value=move || at().map(|m| hears_value(&m.kind)).unwrap_or_default()
                                on:change=move |event| {
                                    let value = event_target_value(&event);
                                    change(n, &|m| match with_hears(&m.kind, &value) {
                                        Some(kind) => {
                                            m.kind = kind;
                                            true
                                        }
                                        None => false,
                                    });
                                }
                            >
                                <option value="">"Program mix"</option>
                                // A source set over MCP or the API shows as itself, so
                                // the select never claims a bus the engine isn't hearing.
                                {move || at().and_then(|m| match m.kind {
                                    MotionKind::Audio { audio_source: Some(source), .. } => {
                                        let name = studio.collection.with(|c| {
                                            c.as_ref()
                                                .and_then(|c| c.sources.iter().find(|s| s.source_id == source))
                                                .map(|s| s.name.clone())
                                        });
                                        let label = name.unwrap_or_else(|| source.as_str().to_owned());
                                        Some(view! {
                                            <option value=format!("source:{}", source.as_str())>{format!("Source: {label}")}</option>
                                        })
                                    }
                                    _ => None,
                                })}
                                {move || studio.collection.with(|c| {
                                    c.as_ref()
                                        .map(|c| c.buses.clone())
                                        .unwrap_or_default()
                                        .into_iter()
                                        .map(|bus| {
                                            let id = bus.bus_id.as_str().to_owned();
                                            view! { <option value=id>{bus.name}</option> }
                                        })
                                        .collect_view()
                                })}
                            </select>
                        </label>
                        <label class="select-field">
                            <span class="field-label">"Follows"</span>
                            <select
                                aria-label="What it follows"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Audio { measure, .. } => MEASURES
                                        .iter()
                                        .find(|(x, _)| *x == measure)
                                        .map(|(_, name)| *name),
                                    _ => None,
                                }).unwrap_or("")
                                on:change=move |event| {
                                    let name = event_target_value(&event);
                                    let Some(next) = MEASURES
                                        .iter()
                                        .find(|(_, n)| *n == name)
                                        .map(|(x, _)| *x)
                                    else {
                                        return;
                                    };
                                    change(n, &|m| match with_measure(&m.kind, next) {
                                        Some(kind) => {
                                            m.kind = kind;
                                            true
                                        }
                                        None => false,
                                    });
                                }
                            >
                                {MEASURES
                                    .iter()
                                    .map(|(_, name)| view! { <option value=*name>{*name}</option> })
                                    .collect_view()}
                            </select>
                        </label>
                        <label class="select-field">
                            <span class="field-label">"Curve"</span>
                            <select
                                aria-label="Curve"
                                prop:value=move || at().and_then(|m| match m.kind {
                                    MotionKind::Audio { curve, .. } => CURVES
                                        .iter()
                                        .find(|(c, _)| *c == curve)
                                        .map(|(_, name)| *name),
                                    _ => None,
                                }).unwrap_or("")
                                on:change=move |event| {
                                    let name = event_target_value(&event);
                                    let Some(next) = CURVES
                                        .iter()
                                        .find(|(_, n)| *n == name)
                                        .map(|(c, _)| *c)
                                    else {
                                        return;
                                    };
                                    change(n, &|m| match &mut m.kind {
                                        MotionKind::Audio { curve, .. } if *curve != next => {
                                            *curve = next;
                                            true
                                        }
                                        _ => false,
                                    });
                                }
                            >
                                {CURVES
                                    .iter()
                                    .map(|(_, name)| view! { <option value=*name>{*name}</option> })
                                    .collect_view()}
                            </select>
                        </label>
                    </Show>
                    {fields}
                    <Show when=move || !at().is_some_and(|m| matches!(m.kind, MotionKind::Audio { .. }))>
                    <label class="select-field">
                        <span class="field-label">"Starts"</span>
                        <select
                            aria-label="When its time starts"
                            title="With the studio clock, so every item with the same settings moves in step, each time the item is shown, or each time it is fired (a hotkey or the Fire button)"
                            prop:value=move || at().map_or("", |m| match m.clock {
                                MotionClock::Shown => "shown",
                                MotionClock::Triggered => "triggered",
                                _ => "engine",
                            })
                            on:change=move |event| {
                                let clock = match event_target_value(&event).as_str() {
                                    "shown" => MotionClock::Shown,
                                    "triggered" => MotionClock::Triggered,
                                    _ => MotionClock::Engine,
                                };
                                change(n, &|m| {
                                    let changed = m.clock != clock;
                                    m.clock = clock;
                                    changed
                                });
                            }
                        >
                            <option value="engine">"In step"</option>
                            <option value="shown">"When shown"</option>
                            <option value="triggered">"When fired"</option>
                        </select>
                    </label>
                    </Show>
                </div>
                <div class="motion-row-actions">
                    <label class="check">
                        <input
                            type="checkbox"
                            prop:checked=move || at().is_some_and(|m| m.enabled)
                            on:change=move |event| {
                                let on = event_target_checked(&event);
                                change(n, &|m| {
                                    let changed = m.enabled != on;
                                    m.enabled = on;
                                    changed
                                });
                            }
                        />
                        "On"
                    </label>
                    <Show when=move || at().is_some_and(|m| m.clock == MotionClock::Triggered)>
                        <button
                            class="motion-button"
                            title="Fire this modifier now, as a hotkey bound to it would"
                            on:click=move |_| fire(n)
                        >
                            "Fire"
                        </button>
                    </Show>
                    <button
                        class="motion-button"
                        title="Remove this modifier"
                        on:click=move |_| {
                            let mut next = list.get_untracked();
                            if n < next.len() {
                                next.remove(n);
                                send(next);
                            }
                        }
                    >
                        "Remove"
                    </button>
                </div>
            </div>
        }
    };

    view! {
        <Fold title="Motion" open=true hint=hint_text>
            <p class="note">
                "Motion the engine draws each frame, on top of the item's values: continuous, or a shake each time it is fired."
            </p>
            {move || (0..list.with(Vec::len)).map(row).collect_view()}
            <button
                class="motion-button"
                disabled=move || list.with(|l| l.len() >= MAX_MOTION_MODIFIERS)
                on:click=move |_| {
                    if let Some(next) = list.with_untracked(|l| with_added(l)) {
                        send(next);
                    }
                }
            >
                "+ Add motion"
            </button>
        </Fold>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adding_picks_a_free_id_and_stops_when_full() {
        let one = with_added(&[]).expect("room");
        assert_eq!(one[0].modifier_id, "motion-1");
        let mut two = with_added(&one).expect("room");
        assert_eq!(two[1].modifier_id, "motion-2");
        two.remove(0);
        let again = with_added(&two).expect("room");
        assert_eq!(again[1].modifier_id, "motion-1");
        let full: Vec<_> =
            (0..MAX_MOTION_MODIFIERS).fold(Vec::new(), |list, _| with_added(&list).expect("room"));
        assert_eq!(with_added(&full), None);
        assert_eq!(jstream_control::protocol::check_motion(&full), Ok(()));
    }

    #[test]
    fn each_field_reads_and_writes_its_parameter() {
        let wiggle = default_kind("Wiggle", MotionProperty::X).expect("a kind");
        for param in Param::of(&wiggle) {
            let next = with_param(&wiggle, *param, 2.0).expect("its field");
            assert_eq!(param_value(&next, *param), Some(2.0), "{param:?}");
        }
        assert_eq!(with_param(&wiggle, Param::Period, 2.0), None);
        let drift = default_kind("Drift", MotionProperty::Rotation).expect("a kind");
        let wrapped = with_param(&drift, Param::Wrap, 360.0).expect("its field");
        assert_eq!(param_value(&wrapped, Param::Wrap), Some(360.0));
        let unwrapped = with_param(&wrapped, Param::Wrap, 0.0).expect("its field");
        assert!(matches!(unwrapped, MotionKind::Drift { wrap: None, .. }));
    }

    /// ADR-0100: an audio modifier's fields, its measure switch and its defaults, all of
    /// which the wire accepts.
    #[test]
    fn an_audio_row_reads_and_writes_its_fields() {
        let light = default_kind("Audio", MotionProperty::Opacity).expect("a kind");
        assert_eq!(param_value(&light, Param::From), Some(-0.6_f32 as f64));
        assert_eq!(param_value(&light, Param::To), Some(0.0));
        for param in Param::of(&light) {
            let next = with_param(&light, *param, -12.0).expect("its field");
            let expected = match param {
                Param::Attack | Param::Release => 0.0,
                _ => -12.0,
            };
            assert_eq!(param_value(&next, *param), Some(expected), "{param:?}");
        }
        assert!(!Param::of(&light).contains(&Param::LowHz));
        let band = with_measure(&light, MotionMeasure::Band).expect("switched");
        assert!(Param::of(&band).contains(&Param::LowHz));
        assert_eq!(param_value(&band, Param::LowHz), Some(40.0));
        assert_eq!(with_measure(&band, MotionMeasure::Band), None);
        let beat = with_measure(&band, MotionMeasure::Onset).expect("switched");
        assert_eq!(
            param_value(&beat, Param::LowHz),
            None,
            "the band's edges go"
        );
        assert!(!Param::of(&beat).contains(&Param::Floor));
        let row = |kind| MotionModifier {
            kind,
            ..with_added(&[]).expect("room")[0].clone()
        };
        for kind in [light.clone(), band, beat] {
            assert_eq!(
                jstream_control::protocol::check_motion(&[row(kind.clone())]),
                Ok(()),
                "{kind:?}"
            );
        }
        // What it hears: a source set elsewhere shows as itself, and picking a bus or
        // the program mix stops it hearing the source (#1149 review N2).
        assert_eq!(hears_value(&light), "");
        let bus = with_hears(&light, "bus_music").expect("switched");
        assert_eq!(hears_value(&bus), "bus_music");
        let mut heard = bus.clone();
        if let MotionKind::Audio { audio_source, .. } = &mut heard {
            *audio_source = Some(SourceId::new("src_mic"));
        }
        assert_eq!(hears_value(&heard), "source:src_mic");
        assert_eq!(with_hears(&heard, "source:src_mic"), None);
        let back = with_hears(&heard, "bus_music").expect("switched");
        assert_eq!(back, bus, "the source goes, the bus stays");
        let program = with_hears(&heard, "").expect("switched");
        assert_eq!(hears_value(&program), "");
        let wiggle = default_kind("Wiggle", MotionProperty::X).expect("a kind");
        assert_eq!(with_measure(&wiggle, MotionMeasure::Band), None);
        assert_eq!(with_hears(&wiggle, "bus_music"), None);
        let pushed = with_kind(&wiggle, "Audio", MotionProperty::X).expect("switched");
        assert_eq!(kind_name(&pushed), "Audio");
    }

    #[test]
    fn switching_kind_keeps_the_size_and_defaults_are_gentle() {
        let wiggle = with_param(
            &default_kind("Wiggle", MotionProperty::X).expect("a kind"),
            Param::Amplitude,
            7.0,
        )
        .expect("its field");
        let oscillate = with_kind(&wiggle, "Oscillate", MotionProperty::X).expect("switched");
        assert_eq!(param_value(&oscillate, Param::Amplitude), Some(7.0));
        assert_eq!(with_kind(&oscillate, "Oscillate", MotionProperty::X), None);
        let turn = default_kind("Oscillate", MotionProperty::Turn).expect("a kind");
        assert_eq!(param_value(&turn, Param::Amplitude), Some(0.5));
        assert_eq!(hint(&[]), "");
        assert_eq!(hint(&with_added(&[]).expect("room")), "1 modifier");
    }

    #[test]
    fn a_shake_reads_and_writes_its_envelope() {
        let shake = default_kind("Shake", MotionProperty::Y).expect("a kind");
        assert_eq!(kind_name(&shake), "Shake");
        assert_eq!(param_value(&shake, Param::Duration), Some(700.0));
        let longer = with_param(&shake, Param::Duration, 1200.0).expect("has a length");
        assert_eq!(param_value(&longer, Param::Duration), Some(1200.0));
        let calmer = with_param(&longer, Param::Decay, 2.0).expect("has a decay");
        assert_eq!(param_value(&calmer, Param::Decay), Some(2.0));
        assert_eq!(with_param(&calmer, Param::Wrap, 1.0), None, "no wrap");
        // Switching to a shake keeps the size.
        let wiggle = default_kind("Wiggle", MotionProperty::Y).expect("a kind");
        let switched = with_kind(&wiggle, "Shake", MotionProperty::Y).expect("switches");
        assert_eq!(
            param_value(&switched, Param::Amplitude),
            param_value(&wiggle, Param::Amplitude)
        );
    }
}
