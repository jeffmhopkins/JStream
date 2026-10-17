//! Transitions: presets, the per-item settings, and the easing curves
//! ([ADR-0029](../../../docs/adr/0029-transitions.md),
//! [ADR-0037](../../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)).
//!
//! A preset is collection state: `{ preset_id, name, kind, duration_ms, easing,
//! settings }`, where `kind` is a transition kind with a descriptor (`kind.list` reports
//! the category `transition`) and `settings` is that kind's settings map. Exactly one
//! preset is the default. Scenes may name a preset to use when switching into them and
//! one for switching out of them, and each item carries [`ItemTransition`].
//!
//! The checks every implementation shares ([`check_preset_timing`],
//! [`ItemTransition::problem`]) and the easing curves ([`Easing::apply`]) live here, so
//! the engine, the mock and any UI that previews a transition agree.

use crate::id::TransitionPresetId;
use crate::kind::{EnumOption, KindCategory, KindDescriptor, ParamDescriptor, ParamType};
use crate::settings::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The built-in transition kind that replaces the picture on the next frame.
pub const CUT: &str = "cut";
/// The built-in transition kind that fades every outgoing item out and every incoming
/// item in, pairing nothing.
pub const FADE: &str = "fade";
/// The built-in transition kind that pairs items placing the same source and
/// interpolates each pair.
pub const MOVE: &str = "move";

/// The longest `duration_ms` a preset may have.
pub const MAX_DURATION_MS: u32 = 60_000;
/// The longest `delay_ms` an item may have.
pub const MAX_DELAY_MS: u32 = 60_000;
/// The largest `stagger_ms` setting of `fade` and `move`.
pub const MAX_STAGGER_MS: i64 = 10_000;

/// The ID of the preset every new collection starts with, which is its default.
pub const INITIAL_PRESET: &str = "cut";

/// How a value moves from its start to its end over a transition
/// ([ADR-0029](../../../docs/adr/0029-transitions.md) §5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    /// Cubic ease-in-out: slow at both ends.
    #[default]
    EaseInOut,
    /// Cubic ease-out: fast at the start, slow at the end.
    EaseOut,
    /// Constant speed.
    Linear,
    /// `1 − e^(−6p)·cos(11p)`: overshoots the end and settles back.
    Spring,
}

impl Easing {
    /// The eased progress for linear progress `p`. `p` at or below 0 is 0, and at or
    /// above 1 is 1; in between, every curve but `spring` stays within `[0, 1]`, and
    /// `spring` overshoots 1 by design. Clients that preview a transition call this, so
    /// they draw what the engine renders.
    pub fn apply(self, p: f32) -> f32 {
        if p.is_nan() || p <= 0.0 {
            return 0.0;
        }
        if p >= 1.0 {
            return 1.0;
        }
        match self {
            Easing::Linear => p,
            Easing::EaseOut => 1.0 - (1.0 - p).powi(3),
            Easing::Spring => 1.0 - (-6.0 * p).exp() * (11.0 * p).cos(),
            Easing::EaseInOut => {
                if p < 0.5 {
                    4.0 * p * p * p
                } else {
                    1.0 - (-2.0 * p + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

/// An item's own easing: one of the curves, or the preset's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemEasing {
    /// The preset's easing.
    #[default]
    Inherit,
    /// [`Easing::EaseInOut`].
    EaseInOut,
    /// [`Easing::EaseOut`].
    EaseOut,
    /// [`Easing::Linear`].
    Linear,
    /// [`Easing::Spring`].
    Spring,
}

impl ItemEasing {
    /// The curve this item uses under a preset whose easing is `preset`.
    pub fn resolve(self, preset: Easing) -> Easing {
        match self {
            ItemEasing::Inherit => preset,
            ItemEasing::EaseInOut => Easing::EaseInOut,
            ItemEasing::EaseOut => Easing::EaseOut,
            ItemEasing::Linear => Easing::Linear,
            ItemEasing::Spring => Easing::Spring,
        }
    }
}

/// Whether an item may pair with an item of another scene in a `move` transition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Glide {
    /// It pairs with the other scene's placement of the same source.
    #[default]
    Auto,
    /// It never pairs: it disappears and appears with its effects instead.
    Never,
}

/// How an item without a partner appears or disappears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionEffect {
    /// The preset's `unmatched` setting.
    #[default]
    Inherit,
    /// Opacity only.
    Fade,
    /// Toward 65% of its size about its centre, while fading.
    Scale,
    /// Toward the nearest canvas edge until it is off the canvas, while fading.
    Slide,
    /// At once, at the item's start: no animation.
    None,
}

impl TransitionEffect {
    /// The effect, with `inherit` replaced by the preset's `unmatched`, which is never
    /// `inherit` itself.
    pub fn resolve(self, unmatched: TransitionEffect) -> TransitionEffect {
        match self {
            TransitionEffect::Inherit => unmatched,
            other => other,
        }
    }

    /// The effect a `unmatched` settings value names (`fade`, `scale` or `slide`), or
    /// `fade` for anything else.
    pub fn from_setting(value: Option<&serde_json::Value>) -> TransitionEffect {
        match value.and_then(serde_json::Value::as_str) {
            Some("scale") => TransitionEffect::Scale,
            Some("slide") => TransitionEffect::Slide,
            _ => TransitionEffect::Fade,
        }
    }
}

/// An item's transition settings ([ADR-0029](../../../docs/adr/0029-transitions.md) §6).
/// They belong to the placement, keyed by item ID. Items are added with the default:
/// no delay, and the preset's easing and effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ItemTransition {
    /// Milliseconds before the item starts moving. An item of the scene being switched
    /// to also waits the preset's `stagger_ms` × its stacking index; a leaving item
    /// waits only its own delay (ADR-0052). At most 60000.
    #[serde(default)]
    pub delay_ms: u32,
    /// Its easing, or the preset's.
    #[serde(default)]
    pub easing: ItemEasing,
    /// Whether it may pair in a `move` transition.
    #[serde(default)]
    pub glide: Glide,
    /// How it appears when it has no partner.
    #[serde(default)]
    pub appear: TransitionEffect,
    /// How it disappears when it has no partner.
    #[serde(default)]
    pub disappear: TransitionEffect,
}

impl ItemTransition {
    /// Whether these are the settings an item is added with, which reads omit.
    pub fn is_default(&self) -> bool {
        *self == ItemTransition::default()
    }

    /// Why an engine refuses these settings, as the field (`delay_ms`) and the reason,
    /// or `None` when they are valid.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        if self.delay_ms > MAX_DELAY_MS {
            return Some(("delay_ms", format!("must be at most {MAX_DELAY_MS}")));
        }
        None
    }
}

/// Why an engine refuses a preset's `duration_ms`, or `None` when it is valid.
pub fn check_preset_timing(duration_ms: u32) -> Option<String> {
    (duration_ms > MAX_DURATION_MS).then(|| format!("must be at most {MAX_DURATION_MS}"))
}

/// One transition preset, as `state.get` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitionPresetInfo {
    /// The preset's ID.
    pub preset_id: TransitionPresetId,
    /// Its display name.
    pub name: String,
    /// Its transition kind's ID, as `kind.list` reports it (`cut`, `fade`, `move`).
    pub kind: String,
    /// How long one item takes, in milliseconds. A kind that ignores it (`cut`) still
    /// keeps it.
    pub duration_ms: u32,
    /// The curve its items follow unless they set their own.
    pub easing: Easing,
    /// Its kind's settings: every parameter of the kind. Omitted for a kind without
    /// parameters.
    #[serde(default, skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
}

/// The collection's transition presets, as `state.get` reports them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TransitionsInfo {
    /// Every preset, ordered by ID.
    pub presets: Vec<TransitionPresetInfo>,
    /// The preset a switch uses when nothing else names one. It always exists.
    pub default: TransitionPresetId,
}

impl Default for TransitionsInfo {
    /// What every new collection starts with: the one preset [`initial_preset`], which
    /// is the default. A server older than 0.27 reports no presets, and this is what a
    /// switch did there: a cut.
    fn default() -> Self {
        TransitionsInfo {
            presets: vec![initial_preset()],
            default: TransitionPresetId::new(INITIAL_PRESET),
        }
    }
}

/// The preset every new collection starts with: `cut`, a 300 ms ease-in-out cut, so a
/// switch behaves as it did before presets existed until the collection gets others.
pub fn initial_preset() -> TransitionPresetInfo {
    TransitionPresetInfo {
        preset_id: TransitionPresetId::new(INITIAL_PRESET),
        name: "Cut".into(),
        kind: CUT.into(),
        duration_ms: 300,
        easing: Easing::EaseInOut,
        settings: Settings::new(),
    }
}

/// What `studio.changed` reports about the transition a switch started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TransitionStarted {
    /// The preset the engine chose: the one the switch named, else the new scene's
    /// `transition_in`, else the old scene's `transition_out`, else the default.
    pub preset_id: TransitionPresetId,
    /// How long the transition runs, in milliseconds, from the switch to its last item's
    /// end. 0 for a cut, which shows the new scene on the next frame.
    pub duration_ms: u32,
}

/// The built-in transition kinds, ordered by ID: `cut`, `fade` and `move`
/// ([ADR-0037](../../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)
/// §1). `fade` and `move` take `unmatched` and `stagger_ms`; `cut` takes nothing.
pub fn builtin_kinds() -> Vec<KindDescriptor> {
    let kind = |id: &str, name: &str, params: Vec<ParamDescriptor>| KindDescriptor {
        kind_id: id.into(),
        category: KindCategory::Transition,
        name: name.into(),
        version: 1,
        video: true,
        audio: false,
        params,
    };
    let per_item = || {
        vec![
            ParamDescriptor {
                id: "unmatched".into(),
                ty: ParamType::Enum {
                    options: vec![
                        EnumOption {
                            value: "fade".into(),
                            label: "Fade".into(),
                        },
                        EnumOption {
                            value: "scale".into(),
                            label: "Scale".into(),
                        },
                        EnumOption {
                            value: "slide".into(),
                            label: "Slide".into(),
                        },
                    ],
                },
                default: json!("fade"),
                label: "Unmatched items".into(),
                description: Some(
                    "How an item without a partner appears or disappears, unless it sets \
                     its own effect."
                        .into(),
                ),
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
            ParamDescriptor {
                id: "stagger_ms".into(),
                ty: ParamType::Int {
                    min: Some(0),
                    max: Some(MAX_STAGGER_MS),
                    step: Some(10),
                    unit: Some("ms".into()),
                },
                default: json!(0),
                label: "Stagger".into(),
                description: Some(
                    "An extra delay per incoming item, by stacking order from the bottom.".into(),
                ),
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
        ]
    };
    vec![
        kind(CUT, "Cut", vec![]),
        kind(FADE, "Fade", per_item()),
        kind(MOVE, "Move", per_item()),
    ]
}

#[cfg(test)]
mod tests {
    use super::{Easing, ItemTransition, TransitionEffect, builtin_kinds};
    use serde_json::json;

    /// The curves at fixed points, to six decimals: what every UI that previews a
    /// transition must draw (ADR-0029 §5).
    #[test]
    fn easing_golden_values() {
        let cases: [(Easing, [f32; 5]); 4] = [
            (Easing::Linear, [0.0, 0.25, 0.5, 0.75, 1.0]),
            (Easing::EaseOut, [0.0, 0.578125, 0.875, 0.984375, 1.0]),
            (Easing::EaseInOut, [0.0, 0.0625, 0.5, 0.9375, 1.0]),
            (Easing::Spring, [0.0, 1.206_24, 0.964_717, 1.004_285, 1.0]),
        ];
        for (easing, expected) in cases {
            for (p, want) in [0.0, 0.25, 0.5, 0.75, 1.0].into_iter().zip(expected) {
                let got = easing.apply(p);
                assert!(
                    (got - want).abs() < 1e-5,
                    "{easing:?}({p}) = {got}, not {want}"
                );
            }
        }
    }

    #[test]
    fn easing_clamps_progress_outside_zero_to_one() {
        for easing in [
            Easing::Linear,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::Spring,
        ] {
            assert_eq!(easing.apply(-1.0), 0.0);
            assert_eq!(easing.apply(f32::NAN), 0.0);
            assert_eq!(easing.apply(2.0), 1.0);
        }
    }

    #[test]
    fn item_settings_default_on_the_wire_and_check_the_delay() {
        let parsed: ItemTransition = serde_json::from_value(json!({})).expect("parses");
        assert!(parsed.is_default());
        assert_eq!(
            serde_json::to_value(ItemTransition::default()).ok(),
            Some(json!({
                "delay_ms": 0,
                "easing": "inherit",
                "glide": "auto",
                "appear": "inherit",
                "disappear": "inherit",
            }))
        );
        let late = ItemTransition {
            delay_ms: 60_001,
            ..ItemTransition::default()
        };
        assert_eq!(late.problem().map(|(field, _)| field), Some("delay_ms"));
    }

    #[test]
    fn effects_resolve_against_the_unmatched_setting() {
        let unmatched = TransitionEffect::from_setting(Some(&json!("slide")));
        assert_eq!(unmatched, TransitionEffect::Slide);
        assert_eq!(TransitionEffect::Inherit.resolve(unmatched), unmatched);
        assert_eq!(
            TransitionEffect::None.resolve(unmatched),
            TransitionEffect::None
        );
        assert_eq!(TransitionEffect::from_setting(None), TransitionEffect::Fade);
    }

    #[test]
    fn builtin_kinds_are_sorted_and_their_defaults_valid() {
        let kinds = builtin_kinds();
        let ids: Vec<&str> = kinds.iter().map(|k| k.kind_id.as_str()).collect();
        assert_eq!(ids, ["cut", "fade", "move"]);
        for kind in &kinds {
            for param in &kind.params {
                assert_eq!(param.check(&param.default), Ok(()), "{}", param.id);
            }
        }
    }
}
