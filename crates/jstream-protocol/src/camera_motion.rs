//! A scene camera's motion modifiers and its follow mode
//! ([ADR-0113](../../../docs/adr/0113-camera-motion-and-follow-pointer.md)).
//!
//! A scene with a camera may also hold `camera_motion`, at most [`MAX_MOTION_MODIFIERS`]
//! [`CameraMotionModifier`]s, which change the camera continuously as it is drawn with the
//! same kinds and clocks as an item's ([`MotionKind`], [`MotionClock`]), less `audio`,
//! since a camera hears nothing. It may also hold
//! `camera_follow`, a [`CameraFollow`], whose camera glides after the pointer over a
//! display or window capture in the scene. Both belong to the scene family, beside the
//! camera. Camera motion is set with `scene.set_camera_motion`. Since 0.91 the follow
//! camera is a filter, [`FOLLOW_POINTER_KIND`], on the followed item's effects (ADR-0120):
//! `camera_follow` reports it, and `scene.set_camera_follow` sets that filter.

use crate::finite::Finite32;
use crate::id::ItemId;
use crate::kind::{KindCategory, KindDescriptor, ParamDescriptor, ParamType};
use crate::motion::{
    MAX_MOTION_MODIFIERS, MotionClock, MotionKind, MotionModifier, MotionProblem, MotionProperty,
    check_motion,
};
use crate::settings::Settings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The camera property a camera motion modifier changes. Every value is added to the
/// camera as it is drawn; the field of view is then held to 1 to 170°, focus above 0 and
/// blur to 0 to 64.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CameraProperty {
    /// The camera's position, x (right), in scene pixels.
    PositionX,
    /// The camera's position, y (down), in scene pixels.
    PositionY,
    /// The camera's position, z (toward the viewer), in scene pixels.
    PositionZ,
    /// The point it looks at, x, in scene pixels.
    TargetX,
    /// The point it looks at, y, in scene pixels.
    TargetY,
    /// The point it looks at, z, in scene pixels.
    TargetZ,
    /// The vertical field of view, in degrees.
    Fov,
    /// The roll, clockwise, in degrees.
    Roll,
    /// The focus distance, in scene pixels; only a camera with a depth of field shows it.
    Focus,
    /// The blur far from focus, in canvas pixels; only a camera with a focus shows it.
    Blur,
    /// A property this version of the protocol doesn't know, from a newer server. Never
    /// sent; an edit carrying one is refused, and a stored one draws as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

impl CameraProperty {
    /// Every property, in the order above.
    pub const ALL: [CameraProperty; 10] = [
        CameraProperty::PositionX,
        CameraProperty::PositionY,
        CameraProperty::PositionZ,
        CameraProperty::TargetX,
        CameraProperty::TargetY,
        CameraProperty::TargetZ,
        CameraProperty::Fov,
        CameraProperty::Roll,
        CameraProperty::Focus,
        CameraProperty::Blur,
    ];
}

/// One motion modifier on a scene camera: an item's [`MotionModifier`] with a camera
/// property.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraMotionModifier {
    /// Unique within the list: 1 to 64 bytes of ASCII letters, digits, `_` and `-`.
    pub modifier_id: String,
    /// What it changes.
    pub property: CameraProperty,
    /// How it changes it, with its parameters.
    #[serde(flatten)]
    pub kind: MotionKind,
    /// The clock it runs on. Default `engine`; `shown` starts when the scene starts being
    /// drawn on program.
    #[serde(default)]
    pub clock: MotionClock,
    /// Whether it runs. Default `true`, which isn't written.
    #[serde(default = "enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Whether a camera's motion list may be stored: at most 8 modifiers, unique well-formed
/// IDs, known properties, kinds and clocks, and each kind's parameters in range, as
/// [`check_motion`] checks an item's. An `audio` modifier is refused: a camera hears
/// nothing, so it would only ever sit at its `from` (ADR-0100).
///
/// # Errors
/// The first problem, naming its field (`motion` or `motion.N.<param>`).
pub fn check_camera_motion(motion: &[CameraMotionModifier]) -> Result<(), MotionProblem> {
    if motion.len() > MAX_MOTION_MODIFIERS {
        return Err(MotionProblem {
            field: "motion".into(),
            reason: "a camera holds at most 8 motion modifiers".into(),
        });
    }
    if let Some(n) = motion
        .iter()
        .position(|m| m.property == CameraProperty::Unknown)
    {
        return Err(MotionProblem {
            field: format!("motion.{n}.property"),
            reason: "is not one this version of the protocol knows".into(),
        });
    }
    if let Some(n) = motion
        .iter()
        .position(|m| matches!(m.kind, MotionKind::Audio { .. }))
    {
        return Err(MotionProblem {
            field: format!("motion.{n}.kind"),
            reason: "audio motion is for items: a camera hears nothing".into(),
        });
    }
    // The rest is what an item's list checks: the property is the camera's own.
    let as_items: Vec<MotionModifier> = motion
        .iter()
        .map(|m| MotionModifier {
            modifier_id: m.modifier_id.clone(),
            property: MotionProperty::X,
            effect: None,
            kind: m.kind.clone(),
            clock: m.clock,
            enabled: m.enabled,
        })
        .collect();
    check_motion(&as_items)
}

/// The smallest and largest zoom a follow camera has.
pub const FOLLOW_ZOOM_RANGE: (f32, f32) = (1.0, 4.0);
/// What one zoom step multiplies a follow camera's zoom by.
pub const FOLLOW_ZOOM_STEP: f32 = 1.25;

/// A camera that follows the pointer over a display or window capture (ADR-0113 §2).
/// While a scene has one, the engine computes its camera's position and target every
/// frame; the camera's field of view, roll, depth sorting and blur apply as always, and
/// with a blur its focus is where it looks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraFollow {
    /// The item it follows: one of the scene's, showing a display or window capture.
    pub item_id: ItemId,
    /// The resting zoom, from 1 to 4. At 1 the frame shows two thirds of the item's
    /// height; at `z`, `1/z` of that. Default 1.
    #[serde(default = "one")]
    pub zoom: Finite32,
    /// The resting angle to the right of the item's normal, in degrees, within ±60.
    /// Default 20.
    #[serde(default = "default_yaw")]
    pub yaw_deg: Finite32,
    /// The resting angle below the item's normal, in degrees, within ±60: positive looks
    /// up at it from below, negative down from above. Default −10.
    #[serde(default = "default_pitch")]
    pub pitch_deg: Finite32,
    /// How far the camera leans with the pointer's speed, in degrees, from 0 (off) to 30.
    /// Default 9.
    #[serde(default = "default_lean")]
    pub lean_deg: Finite32,
    /// The time constant of each of its two springs, in milliseconds, from 50 to 5000.
    /// Default 450.
    #[serde(default = "default_smooth")]
    pub smooth_ms: u32,
    /// The safe margin, in percent of the frame's width and height on each side, from 0
    /// to 40 (since 0.91, #1216). Default 10: the pointer stays in the middle 80% of the
    /// frame on every frame, whatever the smoothing, wherever the item's box lets the
    /// frame reach it. At 0 it stays inside the frame.
    #[serde(default = "default_margin")]
    pub safe_margin_pct: u32,
}

fn one() -> Finite32 {
    Finite32::ONE
}

fn finite(value: f32) -> Finite32 {
    Finite32::new(value).unwrap_or(Finite32::ZERO)
}

fn default_yaw() -> Finite32 {
    finite(20.0)
}

fn default_pitch() -> Finite32 {
    finite(-10.0)
}

fn default_lean() -> Finite32 {
    finite(9.0)
}

fn default_smooth() -> u32 {
    450
}

fn default_margin() -> u32 {
    10
}

/// The filter kind that is a follow camera (ADR-0120): a `video_filter` on a display or
/// window capture item's effects, which draws nothing itself and drives its scene's
/// camera instead. Its settings are a [`CameraFollow`]'s numbers.
pub const FOLLOW_POINTER_KIND: &str = "follow_pointer";

/// The largest safe margin a follow camera takes, in percent of the frame on each side.
pub const MAX_FOLLOW_MARGIN_PCT: u32 = 40;

impl CameraFollow {
    /// Following `item_id` with every other field at its default.
    pub fn of(item_id: ItemId) -> Self {
        Self {
            item_id,
            zoom: one(),
            yaw_deg: default_yaw(),
            pitch_deg: default_pitch(),
            lean_deg: default_lean(),
            smooth_ms: default_smooth(),
            safe_margin_pct: default_margin(),
        }
    }

    /// The follow camera a [`FOLLOW_POINTER_KIND`] filter on `item_id` with `settings`
    /// is: each number read from its setting, a missing or mistyped one as its default,
    /// and each held to its range.
    pub fn from_settings(item_id: ItemId, settings: &Settings) -> Self {
        let float = |id: &str, default: f32, (lo, hi): (f32, f32)| {
            let value = settings
                .get(id)
                .and_then(Value::as_f64)
                .map_or(default, |v| v as f32);
            finite(value.clamp(lo, hi))
        };
        let int = |id: &str, default: u32, (lo, hi): (u32, u32)| {
            settings
                .get(id)
                .and_then(Value::as_f64)
                .map_or(default, |v| {
                    v.round().clamp(0.0, f64::from(u32::MAX)) as u32
                })
                .clamp(lo, hi)
        };
        Self {
            item_id,
            zoom: float("zoom", 1.0, FOLLOW_ZOOM_RANGE),
            yaw_deg: float("yaw_deg", default_yaw().get(), (-60.0, 60.0)),
            pitch_deg: float("pitch_deg", default_pitch().get(), (-60.0, 60.0)),
            lean_deg: float("lean_deg", default_lean().get(), (0.0, 30.0)),
            smooth_ms: int("smooth_ms", default_smooth(), (50, 5000)),
            safe_margin_pct: int(
                "safe_margin_pct",
                default_margin(),
                (0, MAX_FOLLOW_MARGIN_PCT),
            ),
        }
    }

    /// Its numbers as a [`FOLLOW_POINTER_KIND`] filter's settings: every parameter of
    /// [`follow_pointer_descriptor`].
    pub fn settings(&self) -> Settings {
        // An `f32` widened as it is would read 0.1 as 0.10000000149.
        let float = |v: Finite32| Value::from((f64::from(v.get()) * 1e4).round() / 1e4);
        let mut settings = Settings::new();
        settings.insert("zoom", float(self.zoom));
        settings.insert("yaw_deg", float(self.yaw_deg));
        settings.insert("pitch_deg", float(self.pitch_deg));
        settings.insert("lean_deg", float(self.lean_deg));
        settings.insert("smooth_ms", Value::from(self.smooth_ms));
        settings.insert("safe_margin_pct", Value::from(self.safe_margin_pct));
        settings
    }

    /// Whether its numbers are in range. The item is checked by the edit.
    ///
    /// # Errors
    /// The first field out of range (`follow.<field>`), with what it must be.
    pub fn check(&self) -> Result<(), MotionProblem> {
        let problem = |field: &str, reason: &str| MotionProblem {
            field: format!("follow.{field}"),
            reason: reason.into(),
        };
        let (min, max) = FOLLOW_ZOOM_RANGE;
        if !(min..=max).contains(&self.zoom.get()) {
            return Err(problem("zoom", "must be within 1..=4"));
        }
        if self.yaw_deg.get().abs() > 60.0 {
            return Err(problem("yaw_deg", "must be within -60..=60"));
        }
        if self.pitch_deg.get().abs() > 60.0 {
            return Err(problem("pitch_deg", "must be within -60..=60"));
        }
        if !(0.0..=30.0).contains(&self.lean_deg.get()) {
            return Err(problem("lean_deg", "must be within 0..=30"));
        }
        if !(50..=5000).contains(&self.smooth_ms) {
            return Err(problem("smooth_ms", "must be within 50..=5000"));
        }
        if self.safe_margin_pct > MAX_FOLLOW_MARGIN_PCT {
            return Err(problem("safe_margin_pct", "must be within 0..=40"));
        }
        Ok(())
    }
}

/// The descriptor of [`FOLLOW_POINTER_KIND`], as `kind.list` reports it: one parameter
/// per number of a [`CameraFollow`], with its range and default.
pub fn follow_pointer_descriptor() -> KindDescriptor {
    let number =
        |id: &str, label: &str, (min, max): (f64, f64), default: f64, unit: &str| ParamDescriptor {
            warning: None,
            id: id.into(),
            ty: ParamType::Float {
                min: Some(min),
                max: Some(max),
                step: None,
                unit: (!unit.is_empty()).then(|| unit.into()),
            },
            default: Value::from(default),
            label: label.into(),
            description: None,
            group: None,
            advanced: false,
            visible_if: None,
            live: true,
        };
    let whole =
        |id: &str, label: &str, (min, max): (i64, i64), default: i64, unit: &str| ParamDescriptor {
            ty: ParamType::Int {
                min: Some(min),
                max: Some(max),
                step: None,
                unit: Some(unit.into()),
            },
            default: Value::from(default),
            ..number(id, label, (0.0, 0.0), 0.0, "")
        };
    let describe = |param: ParamDescriptor, text: &str| ParamDescriptor {
        description: Some(text.into()),
        ..param
    };
    let (zmin, zmax) = FOLLOW_ZOOM_RANGE;
    KindDescriptor {
        kind_id: FOLLOW_POINTER_KIND.into(),
        category: KindCategory::VideoFilter,
        name: "Follow pointer".into(),
        version: 1,
        video: true,
        audio: false,
        max_outset_px: None,
        params: vec![
            describe(
                number("zoom", "Zoom", (f64::from(zmin), f64::from(zmax)), 1.0, "×"),
                "The resting zoom. At 1 the frame shows two thirds of the capture's height.",
            ),
            describe(
                number("yaw_deg", "From the right", (-60.0, 60.0), 20.0, "°"),
                "The camera's angle to the right of the capture; negative is from the left.",
            ),
            describe(
                number("pitch_deg", "From below", (-60.0, 60.0), -10.0, "°"),
                "The camera's angle below the capture; negative looks down from above.",
            ),
            describe(
                number("lean_deg", "Lean", (0.0, 30.0), 9.0, "°"),
                "How far the camera leans as the pointer moves. 0 is off.",
            ),
            describe(
                whole("smooth_ms", "Smoothing", (50, 5000), 450, "ms"),
                "How long the camera takes to catch up with the pointer.",
            ),
            describe(
                whole(
                    "safe_margin_pct",
                    "Safe margin",
                    (0, i64::from(MAX_FOLLOW_MARGIN_PCT)),
                    i64::from(default_margin()),
                    "%",
                ),
                "How far in from each edge of the frame the pointer always stays.",
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift(id: &str, property: CameraProperty) -> CameraMotionModifier {
        CameraMotionModifier {
            modifier_id: id.into(),
            property,
            kind: MotionKind::Drift {
                rate_per_s: Finite32::ONE,
                wrap: None,
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    fn field_of(motion: &[CameraMotionModifier]) -> Option<String> {
        check_camera_motion(motion).err().map(|p| p.field)
    }

    #[test]
    fn a_camera_motion_list_is_checked_as_an_items_is() {
        assert_eq!(field_of(&[]), None);
        let all: Vec<_> = CameraProperty::ALL
            .iter()
            .take(8)
            .enumerate()
            .map(|(n, p)| drift(&format!("m{n}"), *p))
            .collect();
        assert_eq!(field_of(&all), None);
        let nine: Vec<_> = (0..9)
            .map(|n| drift(&format!("m{n}"), CameraProperty::Roll))
            .collect();
        assert_eq!(field_of(&nine).as_deref(), Some("motion"));
        assert_eq!(
            field_of(&[
                drift("a", CameraProperty::Fov),
                drift("a", CameraProperty::Fov)
            ])
            .as_deref(),
            Some("motion.1.modifier_id")
        );
        let newer: CameraMotionModifier = serde_json::from_value(serde_json::json!({
            "modifier_id": "m", "property": "zoom", "kind": "drift", "rate_per_s": 1.0
        }))
        .expect("parses");
        assert_eq!(newer.property, CameraProperty::Unknown);
        assert_eq!(field_of(&[newer]).as_deref(), Some("motion.0.property"));
        // An audio modifier would never hear anything on a camera (#1149 review B4).
        let heard: CameraMotionModifier = serde_json::from_value(serde_json::json!({
            "modifier_id": "pulse", "property": "fov", "kind": "audio"
        }))
        .expect("parses");
        assert!(matches!(heard.kind, MotionKind::Audio { .. }));
        assert_eq!(
            field_of(&[drift("a", CameraProperty::Fov), heard]).as_deref(),
            Some("motion.1.kind")
        );
    }

    #[test]
    fn a_follow_takes_its_defaults_and_checks_its_ranges() {
        let parsed: CameraFollow =
            serde_json::from_value(serde_json::json!({ "item_id": "screen" })).expect("parses");
        assert_eq!(parsed, CameraFollow::of(ItemId::new("screen")));
        assert!(parsed.check().is_ok());
        let f = |v: f32| Finite32::new(v).expect("finite");
        let cases = [
            (
                "zoom",
                CameraFollow {
                    zoom: f(0.5),
                    ..parsed.clone()
                },
            ),
            (
                "zoom",
                CameraFollow {
                    zoom: f(4.5),
                    ..parsed.clone()
                },
            ),
            (
                "yaw_deg",
                CameraFollow {
                    yaw_deg: f(-61.0),
                    ..parsed.clone()
                },
            ),
            (
                "pitch_deg",
                CameraFollow {
                    pitch_deg: f(61.0),
                    ..parsed.clone()
                },
            ),
            (
                "lean_deg",
                CameraFollow {
                    lean_deg: f(-1.0),
                    ..parsed.clone()
                },
            ),
            (
                "smooth_ms",
                CameraFollow {
                    smooth_ms: 49,
                    ..parsed.clone()
                },
            ),
        ];
        for (field, follow) in cases {
            let problem = follow.check().expect_err(field);
            assert_eq!(problem.field, format!("follow.{field}"));
        }
        let wide = CameraFollow {
            safe_margin_pct: 41,
            ..parsed
        };
        assert_eq!(
            wide.check().map_err(|p| p.field),
            Err("follow.safe_margin_pct".into())
        );
    }

    /// ADR-0120: a follow's numbers are its filter's settings, every parameter of the
    /// descriptor, and read back the same; a missing or out-of-range one reads as its
    /// default or its range's end.
    #[test]
    fn a_follow_round_trips_through_its_filters_settings() {
        let f = |v: f32| Finite32::new(v).expect("finite");
        let follow = CameraFollow {
            zoom: f(2.5),
            yaw_deg: f(-15.0),
            pitch_deg: f(5.0),
            lean_deg: f(0.0),
            smooth_ms: 900,
            safe_margin_pct: 25,
            ..CameraFollow::of(ItemId::new("screen"))
        };
        let settings = follow.settings();
        let descriptor = follow_pointer_descriptor();
        let ids: Vec<&str> = settings.iter().map(|(id, _)| id).collect();
        let mut params: Vec<&str> = descriptor.params.iter().map(|p| p.id.as_str()).collect();
        params.sort_unstable();
        assert_eq!(ids, params);
        assert_eq!(
            CameraFollow::from_settings(ItemId::new("screen"), &settings),
            follow
        );
        let defaults: Settings = descriptor
            .params
            .iter()
            .map(|p| (p.id.clone(), p.default.clone()))
            .collect();
        assert_eq!(
            CameraFollow::from_settings(ItemId::new("screen"), &defaults),
            CameraFollow::of(ItemId::new("screen"))
        );
        assert_eq!(
            CameraFollow::from_settings(ItemId::new("screen"), &Settings::new()),
            CameraFollow::of(ItemId::new("screen"))
        );
        let wild: Settings = [
            ("zoom", serde_json::json!(9.0)),
            ("safe_margin_pct", serde_json::json!(90)),
        ]
        .into_iter()
        .collect();
        let read = CameraFollow::from_settings(ItemId::new("screen"), &wild);
        assert_eq!((read.zoom.get(), read.safe_margin_pct), (4.0, 40));
    }
}
