//! The Inspector's Motion section (ADR-0099): the selected item's motion modifiers,
//! each a row with what it moves, how (wiggle, oscillate or drift, with that kind's
//! parameters), its clock and whether it is on, and Add and Remove. Every change sends
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
    Edit, Finite32, ItemId, MAX_MOTION_MODIFIERS, MotionClock, MotionKind, MotionModifier,
    MotionProperty, MotionShape, SceneId,
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
pub const KINDS: [&str; 3] = ["Wiggle", "Oscillate", "Drift"];

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
            _ => &[],
        }
    }

    /// Its label, step, decimals and range.
    fn field(self) -> (&'static str, f64, usize, f64, f64) {
        match self {
            Param::Amplitude => ("Amount", 0.1, 2, 0.0, 100_000.0),
            Param::Frequency => ("Speed", 0.05, 2, 0.01, 60.0),
            Param::Seed => ("Seed", 1.0, 0, 0.0, f64::from(u32::MAX)),
            Param::Octaves => ("Detail", 1.0, 0, 1.0, 4.0),
            Param::Period => ("Period", 100.0, 0, 1.0, 3_600_000.0),
            Param::Phase => ("Phase", 0.05, 2, 0.0, 0.99),
            Param::Rate => ("Rate", 1.0, 2, -100_000.0, 100_000.0),
            Param::Wrap => ("Wrap", 10.0, 1, 0.0, 100_000.0),
        }
    }
}

/// `kind`'s value of `param`, or `None` when it has no such field.
pub fn param_value(kind: &MotionKind, param: Param) -> Option<f64> {
    let f = |v: Finite32| Some(f64::from(v.get()));
    match (*kind, param) {
        (MotionKind::Wiggle { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Oscillate { amplitude, .. }, Param::Amplitude) => f(amplitude),
        (MotionKind::Wiggle { frequency_hz, .. }, Param::Frequency) => f(frequency_hz),
        (MotionKind::Wiggle { seed, .. }, Param::Seed) => Some(f64::from(seed)),
        (MotionKind::Wiggle { octaves, .. }, Param::Octaves) => Some(f64::from(octaves)),
        (MotionKind::Oscillate { period_ms, .. }, Param::Period) => Some(f64::from(period_ms)),
        (MotionKind::Oscillate { phase, .. }, Param::Phase) => f(phase),
        (MotionKind::Drift { rate_per_s, .. }, Param::Rate) => f(rate_per_s),
        (MotionKind::Drift { wrap, .. }, Param::Wrap) => {
            Some(wrap.map_or(0.0, |w| f64::from(w.get())))
        }
        _ => None,
    }
}

/// `kind` with `param` set to `value`, or `None` when it has no such field. A wrap of 0
/// is no wrap.
pub fn with_param(kind: &MotionKind, param: Param, value: f64) -> Option<MotionKind> {
    let v = finite(value as f32);
    let mut next = *kind;
    match (&mut next, param) {
        (MotionKind::Wiggle { amplitude, .. }, Param::Amplitude)
        | (MotionKind::Oscillate { amplitude, .. }, Param::Amplitude) => *amplitude = v,
        (MotionKind::Wiggle { frequency_hz, .. }, Param::Frequency) => *frequency_hz = v,
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
        _ => return None,
    }
    Some(next)
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
                            Param::Frequency => "Hz",
                            Param::Period => "ms",
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
                                    true
                                });
                            }
                        >
                            {MotionProperty::ALL
                                .into_iter()
                                .map(|p| view! { <option value=property_name(p)>{property_name(p)}</option> })
                                .collect_view()}
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
                    {fields}
                    <label class="select-field">
                        <span class="field-label">"Starts"</span>
                        <select
                            aria-label="When its time starts"
                            title="With the studio clock, so every item with the same settings moves in step, or each time the item is shown"
                            prop:value=move || at().map_or("", |m| match m.clock {
                                MotionClock::Shown => "shown",
                                _ => "engine",
                            })
                            on:change=move |event| {
                                let clock = match event_target_value(&event).as_str() {
                                    "shown" => MotionClock::Shown,
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
                        </select>
                    </label>
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
        <Fold title="Motion" open=false hint=hint_text>
            <p class="note">
                "Continuous motion the engine draws each frame, on top of the item's values."
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
}
