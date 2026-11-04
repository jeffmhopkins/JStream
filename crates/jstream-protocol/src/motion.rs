//! Motion modifiers on the wire ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 3, since 0.75).
//!
//! An item, a group's item included, holds an ordered list of at most
//! [`MAX_MOTION_MODIFIERS`] modifiers ([`MotionModifier`]). Each changes one property
//! continuously as a pure function of the engine's animation time, and is evaluated as
//! each frame is drawn, never stored: the item's own values don't change. Positions,
//! angles and crop edges add the modifier's value; scale and opacity are multiplied by
//! `1 + value`, and opacity is then clamped to `0..=1`. `item.set_motion` writes the
//! whole list, and a keyframe scene can override it whole (`OverrideField::Motion`).

use crate::finite::Finite32;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most motion modifiers one item holds.
pub const MAX_MOTION_MODIFIERS: usize = 8;

/// The longest motion modifier ID, in bytes.
pub const MAX_MOTION_ID_LEN: usize = 64;

/// The property a motion modifier changes.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MotionProperty {
    /// Horizontal position of the box's centre, in canvas pixels: added.
    X,
    /// Vertical position of the box's centre, in canvas pixels: added.
    Y,
    /// The box's size about its centre: multiplied by `1 + value`.
    Scale,
    /// Rotation in the canvas plane, in degrees: added.
    Rotation,
    /// 3D tilt (`tilt_deg`), in degrees: added.
    Tilt,
    /// 3D turn (`turn_deg`), in degrees: added.
    Turn,
    /// 3D offset along the viewing axis (`depth_px`), in pixels toward the viewer:
    /// added.
    Depth,
    /// Opacity: multiplied by `1 + value`, then clamped to `0..=1`.
    Opacity,
    /// Source pixels cut from the left edge: added, never below 0.
    CropLeft,
    /// Source pixels cut from the top edge: added, never below 0.
    CropTop,
    /// Source pixels cut from the right edge: added, never below 0.
    CropRight,
    /// Source pixels cut from the bottom edge: added, never below 0.
    CropBottom,
}

impl MotionProperty {
    /// Every property, in the order above.
    pub const ALL: [MotionProperty; 12] = [
        MotionProperty::X,
        MotionProperty::Y,
        MotionProperty::Scale,
        MotionProperty::Rotation,
        MotionProperty::Tilt,
        MotionProperty::Turn,
        MotionProperty::Depth,
        MotionProperty::Opacity,
        MotionProperty::CropLeft,
        MotionProperty::CropTop,
        MotionProperty::CropRight,
        MotionProperty::CropBottom,
    ];
}

/// The waveform of an `oscillate` modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MotionShape {
    /// A sine: starts at 0, rising.
    #[default]
    Sine,
    /// A triangle: starts at 0, rising, straight between -1 and 1.
    Triangle,
    /// A square: 1 for the first half of each period, -1 for the second.
    Square,
    /// A sawtooth: rises from -1 to 1 over each period, then jumps back.
    Saw,
}

impl MotionShape {
    /// Every shape, in the order above.
    pub const ALL: [MotionShape; 4] = [
        MotionShape::Sine,
        MotionShape::Triangle,
        MotionShape::Square,
        MotionShape::Saw,
    ];
}

/// What a modifier does, with its parameters, tagged by `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MotionKind {
    /// Smooth seeded noise within `-amplitude..=amplitude`. The same seed gives the same
    /// motion on every machine; each extra octave adds detail at twice the frequency and
    /// half the size.
    Wiggle {
        /// The largest value it reaches, `0..=100000`.
        amplitude: Finite32,
        /// About how many turns of direction a second, `0..=60` and not 0.
        frequency_hz: Finite32,
        /// Picks the noise. Default 0.
        #[serde(default)]
        seed: u32,
        /// How many octaves, `1..=4`. Default 1.
        #[serde(default = "one_octave")]
        octaves: u8,
    },
    /// A periodic wave within `-amplitude..=amplitude`.
    Oscillate {
        /// Its waveform. Default `sine`.
        #[serde(default)]
        shape: MotionShape,
        /// The largest value it reaches, `0..=100000`.
        amplitude: Finite32,
        /// One period, in milliseconds, `1..=3600000`.
        period_ms: u32,
        /// Its offset into the period, as a fraction in `0..1`. Default 0.
        #[serde(default)]
        phase: Finite32,
    },
    /// A constant rate per second, of either sign, wrapping into `0..wrap` when `wrap`
    /// is set.
    Drift {
        /// Units per second, within `-100000..=100000`.
        rate_per_s: Finite32,
        /// Where it wraps back to 0, `0..=100000` and not 0. Omitted for no wrap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wrap: Option<Finite32>,
    },
}

fn one_octave() -> u8 {
    1
}

/// Where a modifier's time starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MotionClock {
    /// The engine's animation time: every item with the same parameters moves in step.
    #[default]
    Engine,
    /// From when the item last became visible on program, so it starts fresh each
    /// time it is shown. While it isn't shown, the modifier is at its start.
    Shown,
}

/// One motion modifier on an item (ADR-0099 decision 3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MotionModifier {
    /// Its ID, unique in the item's list: 1 to [`MAX_MOTION_ID_LEN`] bytes of ASCII
    /// letters, digits, `_` and `-`.
    pub modifier_id: String,
    /// What it changes.
    pub property: MotionProperty,
    /// What it does.
    #[serde(flatten)]
    pub kind: MotionKind,
    /// Where its time starts. Default `engine`.
    #[serde(default)]
    pub clock: MotionClock,
    /// Whether it applies. Default `true`; omitted when `true`.
    #[serde(default = "enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// Why [`check_motion`] refuses a list: the field, `motion` or `motion.<n>.<param>`, and
/// what it must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MotionProblem {
    /// The field, as an `invalid_argument` error names it.
    pub field: String,
    /// What is wrong, or what the field must be.
    pub reason: String,
}

/// The largest amplitude or wrap, and the largest drift rate either way.
const MAX_MAGNITUDE: f32 = 100_000.0;
/// The highest wiggle frequency, in hertz.
const MAX_FREQUENCY_HZ: f32 = 60.0;
/// The longest oscillator period, in milliseconds: one hour.
const MAX_PERIOD_MS: u32 = 3_600_000;

/// Checks an item's whole motion list as `item.set_motion` does (ADR-0099): at most
/// [`MAX_MOTION_MODIFIERS`], each ID well formed and unique, and every parameter in the
/// range its documentation gives. Returns the first problem.
pub fn check_motion(motion: &[MotionModifier]) -> Result<(), MotionProblem> {
    let problem = |field: String, reason: &str| MotionProblem {
        field,
        reason: reason.to_owned(),
    };
    if motion.len() > MAX_MOTION_MODIFIERS {
        return Err(problem(
            "motion".into(),
            "an item holds at most 8 motion modifiers",
        ));
    }
    for (n, m) in motion.iter().enumerate() {
        let field = |param: &str| format!("motion.{n}.{param}");
        let id = &m.modifier_id;
        let well_formed = !id.is_empty()
            && id.len() <= MAX_MOTION_ID_LEN
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !well_formed {
            return Err(problem(
                field("modifier_id"),
                "must be 1 to 64 bytes of ASCII letters, digits, `_` and `-`",
            ));
        }
        if motion[..n].iter().any(|other| other.modifier_id == *id) {
            return Err(MotionProblem {
                field: field("modifier_id"),
                reason: format!("modifier `{id}` is in the list twice"),
            });
        }
        if let Some((param, reason)) = m.kind.problem() {
            return Err(problem(field(param), reason));
        }
    }
    Ok(())
}

impl MotionKind {
    /// The first parameter out of range, by wire name, with what it must be.
    fn problem(&self) -> Option<(&'static str, &'static str)> {
        let magnitude = |v: Finite32| (0.0..=MAX_MAGNITUDE).contains(&v.get());
        let frequency = |v: Finite32| v.get() > 0.0 && v.get() <= MAX_FREQUENCY_HZ;
        match *self {
            MotionKind::Wiggle {
                amplitude,
                frequency_hz,
                octaves,
                ..
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !frequency(frequency_hz) {
                    Some(("frequency_hz", "must be above 0 and at most 60"))
                } else if !(1..=4).contains(&octaves) {
                    Some(("octaves", "must be within 1..=4"))
                } else {
                    None
                }
            }
            MotionKind::Oscillate {
                amplitude,
                period_ms,
                phase,
                ..
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !(1..=MAX_PERIOD_MS).contains(&period_ms) {
                    Some(("period_ms", "must be within 1..=3600000"))
                } else if !(0.0..1.0).contains(&phase.get()) {
                    Some(("phase", "must be at least 0 and below 1"))
                } else {
                    None
                }
            }
            MotionKind::Drift { rate_per_s, wrap } => {
                if rate_per_s.get().abs() > MAX_MAGNITUDE {
                    Some(("rate_per_s", "must be within -100000..=100000"))
                } else if wrap.is_some_and(|w| !(magnitude(w) && w.get() > 0.0)) {
                    Some(("wrap", "must be above 0 and at most 100000"))
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiggle(id: &str, amplitude: f32) -> MotionModifier {
        MotionModifier {
            modifier_id: id.into(),
            property: MotionProperty::X,
            kind: MotionKind::Wiggle {
                amplitude: Finite32::new(amplitude).expect("finite"),
                frequency_hz: Finite32::ONE,
                seed: 0,
                octaves: 1,
            },
            clock: MotionClock::Engine,
            enabled: true,
        }
    }

    fn field_of(motion: &[MotionModifier]) -> Option<String> {
        check_motion(motion).err().map(|p| p.field)
    }

    #[test]
    fn a_valid_list_passes_and_each_problem_names_its_field() {
        assert_eq!(field_of(&[wiggle("a", 3.0), wiggle("b", 0.0)]), None);
        assert_eq!(field_of(&[]), None);
        let nine: Vec<_> = (0..9).map(|n| wiggle(&format!("m{n}"), 1.0)).collect();
        assert_eq!(field_of(&nine).as_deref(), Some("motion"));
        assert_eq!(
            field_of(&[wiggle("a", 1.0), wiggle("a", 1.0)]).as_deref(),
            Some("motion.1.modifier_id")
        );
        for bad in ["", "has space", "ünï", &"x".repeat(65)] {
            assert_eq!(
                field_of(&[wiggle(bad, 1.0)]).as_deref(),
                Some("motion.0.modifier_id"),
                "{bad:?}"
            );
        }
        assert_eq!(
            field_of(&[wiggle("a", 1.0), wiggle("b", -1.0)]).as_deref(),
            Some("motion.1.amplitude")
        );
    }

    #[test]
    fn each_kind_checks_its_ranges() {
        let f = |v: f32| Finite32::new(v).expect("finite");
        let with = |kind| MotionModifier {
            kind,
            ..wiggle("a", 1.0)
        };
        let cases = [
            (
                MotionKind::Wiggle {
                    amplitude: f(1.0),
                    frequency_hz: f(0.0),
                    seed: 0,
                    octaves: 1,
                },
                "frequency_hz",
            ),
            (
                MotionKind::Wiggle {
                    amplitude: f(1.0),
                    frequency_hz: f(1.0),
                    seed: 0,
                    octaves: 5,
                },
                "octaves",
            ),
            (
                MotionKind::Oscillate {
                    shape: MotionShape::Sine,
                    amplitude: f(1.0),
                    period_ms: 0,
                    phase: f(0.0),
                },
                "period_ms",
            ),
            (
                MotionKind::Oscillate {
                    shape: MotionShape::Sine,
                    amplitude: f(1.0),
                    period_ms: 10,
                    phase: f(1.0),
                },
                "phase",
            ),
            (
                MotionKind::Drift {
                    rate_per_s: f(-100_001.0),
                    wrap: None,
                },
                "rate_per_s",
            ),
            (
                MotionKind::Drift {
                    rate_per_s: f(-5.0),
                    wrap: Some(f(0.0)),
                },
                "wrap",
            ),
        ];
        for (kind, param) in cases {
            assert_eq!(
                field_of(&[with(kind)]),
                Some(format!("motion.0.{param}")),
                "{kind:?}"
            );
        }
    }
}
