//! A scene camera's motion modifiers and its follow mode
//! ([ADR-0113](../../../docs/adr/0113-camera-motion-and-follow-pointer.md)).
//!
//! A scene with a camera may also hold `camera_motion`, at most [`MAX_MOTION_MODIFIERS`]
//! [`CameraMotionModifier`]s, which change the camera continuously as it is drawn with the
//! same kinds and clocks as an item's ([`MotionKind`], [`MotionClock`]). It may also hold
//! `camera_follow`, a [`CameraFollow`], whose camera glides after the pointer over a
//! display or window capture in the scene. Both belong to the scene family, beside the
//! camera, and are set with `scene.set_camera_motion` and `scene.set_camera_follow`.

use crate::finite::Finite32;
use crate::id::ItemId;
use crate::motion::{
    MAX_MOTION_MODIFIERS, MotionClock, MotionKind, MotionModifier, MotionProblem, MotionProperty,
    check_motion,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
/// [`check_motion`] checks an item's.
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
    // The rest is what an item's list checks: the property is the camera's own.
    let as_items: Vec<MotionModifier> = motion
        .iter()
        .map(|m| MotionModifier {
            modifier_id: m.modifier_id.clone(),
            property: MotionProperty::X,
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
        }
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
        Ok(())
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
    }
}
