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

use crate::finite::Finite32;
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
/// The built-in transition kind that plays a scene over the switch: the scene's items
/// come in over the old picture, the picture cuts to the new scene under them at
/// `cut_ms`, and they go again.
pub const STINGER: &str = "stinger";

/// The longest `duration_ms` a preset may have.
pub const MAX_DURATION_MS: u32 = 60_000;
/// The longest `delay_ms` an item may have.
pub const MAX_DELAY_MS: u32 = 60_000;
/// The largest `stagger_ms` setting of `fade` and `move`.
pub const MAX_STAGGER_MS: i64 = 10_000;
/// The farthest a `cubic_bezier` easing's `y1` or `y2` may be from `[0, 1]`: they lie in
/// `[-MAX_BEZIER_OVERSHOOT, 1 + MAX_BEZIER_OVERSHOOT]` (ADR-0129).
pub const MAX_BEZIER_OVERSHOOT: f32 = 2.0;

/// The ID of the preset every new collection starts with, which is its default.
pub const INITIAL_PRESET: &str = "cut";

/// How a value moves from its start to its end over a transition
/// ([ADR-0029](../../../docs/adr/0029-transitions.md) §5; `ease_in`, `hold` and
/// `cubic_bezier` since 0.99, [ADR-0129](../../../docs/adr/0129-keyframe-timing-and-easings.md)).
/// The named curves are strings (`"ease_out"`); a `cubic_bezier` is an object
/// (`{"cubic_bezier": {"x1": 0.2, "y1": 0, "x2": 0, "y2": 1}}`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    /// Cubic ease-in-out: slow at both ends.
    #[default]
    EaseInOut,
    /// Cubic ease-out: fast at the start, slow at the end.
    EaseOut,
    /// Cubic ease-in: slow at the start, fast at the end.
    EaseIn,
    /// Constant speed.
    Linear,
    /// `1 − e^(−6p)·cos(11p)`: overshoots the end and settles back.
    Spring,
    /// A step: the value holds its start until the end, then takes its end at once.
    Hold,
    /// The CSS `cubic-bezier(x1, y1, x2, y2)` curve from `(0, 0)` to `(1, 1)`.
    CubicBezier(Bezier),
}

/// The two control points of a [`Easing::CubicBezier`] curve, as CSS writes them. `x1`
/// and `x2` (time) lie in `[0, 1]`; `y1` and `y2` (progress) in
/// `[-2, 3]` ([`MAX_BEZIER_OVERSHOOT`]), so the curve may overshoot either end.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bezier {
    /// The first control point's time.
    pub x1: Finite32,
    /// The first control point's progress.
    pub y1: Finite32,
    /// The second control point's time.
    pub x2: Finite32,
    /// The second control point's progress.
    pub y2: Finite32,
}

// A `Finite32` is never NaN, so equality is total. `-0.0` and `0.0` compare equal and
// hash alike, since the hash uses each value with its sign of zero removed.
impl Eq for Bezier {}

impl std::hash::Hash for Bezier {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        for v in [self.x1, self.y1, self.x2, self.y2] {
            (v.get() + 0.0).to_bits().hash(state);
        }
    }
}

impl Bezier {
    /// Why an engine refuses these points, as the field (`x1`) and the reason, or
    /// `None` when they are valid.
    pub fn problem(&self) -> Option<(&'static str, String)> {
        for (field, x) in [("x1", self.x1), ("x2", self.x2)] {
            if !(0.0..=1.0).contains(&x.get()) {
                return Some((field, "must be between 0 and 1".into()));
            }
        }
        let (low, high) = (-MAX_BEZIER_OVERSHOOT, 1.0 + MAX_BEZIER_OVERSHOOT);
        for (field, y) in [("y1", self.y1), ("y2", self.y2)] {
            if !(low..=high).contains(&y.get()) {
                return Some((field, format!("must be between {low} and {high}")));
            }
        }
        None
    }

    /// The curve's progress at time `t` in `[0, 1]`: the `y` of the point whose `x` is
    /// `t`, found by bisection, which needs no derivative and always converges since `x`
    /// rises with the curve's parameter while `x1` and `x2` lie in `[0, 1]`.
    pub fn at(&self, t: f32) -> f32 {
        let (x1, y1, x2, y2) = (
            f64::from(self.x1.get()),
            f64::from(self.y1.get()),
            f64::from(self.x2.get()),
            f64::from(self.y2.get()),
        );
        let cubic = |a: f64, b: f64, u: f64| {
            let v = 1.0 - u;
            3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
        };
        let t = f64::from(t);
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..40 {
            let mid = (lo + hi) * 0.5;
            if cubic(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        cubic(y1, y2, (lo + hi) * 0.5) as f32
    }
}

impl Easing {
    /// The eased progress for linear progress `p`. `p` at or below 0 is 0, and at or
    /// above 1 is 1; in between, the named curves but `spring` stay within `[0, 1]`,
    /// `spring` overshoots 1 by design, `hold` is 0, and a `cubic_bezier` goes where
    /// its control points take it. Clients that preview a transition call this, so
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
            Easing::EaseIn => p * p * p,
            Easing::Hold => 0.0,
            Easing::CubicBezier(bezier) => bezier.at(p),
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

    /// Why an engine refuses this easing, as the field (`cubic_bezier.x1`) and the
    /// reason, or `None` when it is valid: only a `cubic_bezier` can be refused.
    pub fn problem(&self) -> Option<(String, String)> {
        match self {
            Easing::CubicBezier(bezier) => bezier
                .problem()
                .map(|(field, reason)| (format!("cubic_bezier.{field}"), reason)),
            _ => None,
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
    /// [`Easing::EaseIn`] (since 0.99).
    EaseIn,
    /// [`Easing::Hold`] (since 0.99).
    Hold,
    /// [`Easing::CubicBezier`] (since 0.99).
    CubicBezier(Bezier),
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
            ItemEasing::EaseIn => Easing::EaseIn,
            ItemEasing::Hold => Easing::Hold,
            ItemEasing::CubicBezier(bezier) => Easing::CubicBezier(bezier),
        }
    }

    /// Why an engine refuses this easing, as for [`Easing::problem`].
    pub fn problem(&self) -> Option<(String, String)> {
        self.resolve(Easing::Linear).problem()
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
        if let Some((_, reason)) = self.easing.problem() {
            return Some(("easing", reason));
        }
        None
    }
}

/// A keyframe scene's timing for one item in transitions into it (ADR-0129, since
/// 0.99): each field set replaces the item's own (`delay_ms`, `easing`) or the preset's
/// (`duration_ms`) when a switch, a placed keyframe or a hotkey moves program to that
/// keyframe. An omitted field follows the item and the preset. A transition into any
/// other member of the family doesn't use it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct KeyframeTiming {
    /// Milliseconds before the item starts moving, in place of its own `delay_ms`; the
    /// preset's `stagger_ms` × its index still adds to it. At most 60000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
    /// Its easing in place of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub easing: Option<ItemEasing>,
    /// How long the item takes, in place of the preset's `duration_ms`. At most 60000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
}

impl KeyframeTiming {
    /// Whether no field is set, which a keyframe stores as no override.
    pub fn is_empty(&self) -> bool {
        *self == KeyframeTiming::default()
    }

    /// Why an engine refuses this timing, as the field (`delay_ms`) and the reason, or
    /// `None` when it is valid.
    pub fn problem(&self) -> Option<(String, String)> {
        if self.delay_ms.is_some_and(|ms| ms > MAX_DELAY_MS) {
            return Some(("delay_ms".into(), format!("must be at most {MAX_DELAY_MS}")));
        }
        if self.duration_ms.is_some_and(|ms| ms > MAX_DURATION_MS) {
            return Some((
                "duration_ms".into(),
                format!("must be at most {MAX_DURATION_MS}"),
            ));
        }
        if let Some((field, reason)) = self.easing.and_then(|easing| easing.problem()) {
            return Some((format!("easing.{field}"), reason));
        }
        None
    }

    /// `settings` with this timing's delay and easing in place of the item's own.
    pub fn apply(&self, settings: ItemTransition) -> ItemTransition {
        ItemTransition {
            delay_ms: self.delay_ms.unwrap_or(settings.delay_ms),
            easing: self.easing.unwrap_or(settings.easing),
            ..settings
        }
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
    /// is the default. A server older than 0.30 reports no presets, and this is what a
    /// switch did there: a cut.
    fn default() -> Self {
        TransitionsInfo {
            presets: vec![initial_preset()],
            default: TransitionPresetId::new(INITIAL_PRESET),
        }
    }
}

/// The preset every new collection starts with: `cut`, named Cut, lasting no time.
/// A switch that uses it shows the new scene on the next frame ([`TransitionStarted`]
/// reports `duration_ms` 0). A `cut` ignores `duration_ms`; 0 is what the preset stores
/// so a client that reads it doesn't play a fade (#1209).
pub fn initial_preset() -> TransitionPresetInfo {
    TransitionPresetInfo {
        preset_id: TransitionPresetId::new(INITIAL_PRESET),
        name: "Cut".into(),
        kind: CUT.into(),
        duration_ms: 0,
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

/// The built-in transition kinds, ordered by ID: `cut`, `fade`, `move` and `stinger`
/// ([ADR-0037](../../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md)
/// §1). `fade` and `move` take `unmatched` and `stagger_ms`; `stinger` takes the
/// `scene` it plays, its `cut_ms` and `stagger_ms`; `cut` takes nothing.
pub fn builtin_kinds() -> Vec<KindDescriptor> {
    let kind = |id: &str, name: &str, params: Vec<ParamDescriptor>| KindDescriptor {
        kind_id: id.into(),
        category: KindCategory::Transition,
        name: name.into(),
        version: 1,
        video: true,
        audio: false,
        max_outset_px: None,
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
    let stinger = {
        let mut params = vec![
            ParamDescriptor {
                id: "scene".into(),
                ty: ParamType::Scene,
                default: serde_json::Value::Null,
                label: "Stinger scene".into(),
                description: Some(
                    "The scene played over the switch. Its items come in by their own \
                     appear effect and delay, cover the cut, and go by their disappear \
                     effect. Without one, the switch cuts at the cut point."
                        .into(),
                ),
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
            ParamDescriptor {
                id: "cut_ms".into(),
                ty: ParamType::Int {
                    min: Some(0),
                    max: Some(i64::from(MAX_DURATION_MS)),
                    step: Some(10),
                    unit: Some("ms".into()),
                },
                default: json!(500),
                label: "Cut point".into(),
                description: Some(
                    "When the picture under the stinger cuts to the new scene, from the \
                     start; at most the preset's duration."
                        .into(),
                ),
                group: None,
                advanced: false,
                visible_if: None,
                live: true,
            },
        ];
        params.extend(per_item().into_iter().filter(|p| p.id == "stagger_ms"));
        params
    };
    vec![
        kind(CUT, "Cut", vec![]),
        kind(FADE, "Fade", per_item()),
        kind(MOVE, "Move", per_item()),
        kind(STINGER, "Stinger", stinger),
    ]
}

#[cfg(test)]
mod tests {
    use super::{Easing, ItemTransition, TransitionEffect, builtin_kinds};
    use serde_json::json;

    /// #1209: the preset a new collection starts with is an instant cut.
    #[test]
    fn the_initial_preset_is_an_instant_cut() {
        let preset = super::initial_preset();
        assert_eq!(preset.preset_id.as_str(), super::INITIAL_PRESET);
        assert_eq!(preset.name, "Cut");
        assert_eq!(preset.kind, super::CUT);
        assert_eq!(preset.duration_ms, 0);
        assert!(preset.settings.is_empty());
    }

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
        assert_eq!(ids, ["cut", "fade", "move", "stinger"]);
        for kind in &kinds {
            for param in &kind.params {
                assert_eq!(param.check(&param.default), Ok(()), "{}", param.id);
            }
        }
    }
}
