//! Motion modifiers on the wire ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 3, since 0.81).
//!
//! An item, a group's item included, holds an ordered list of at most
//! [`MAX_MOTION_MODIFIERS`] modifiers ([`MotionModifier`]). Each changes one property
//! continuously as a pure function of the engine's animation time, and is evaluated as
//! each frame is drawn, never stored: the item's own values don't change. Positions,
//! angles and crop edges add the modifier's value; scale and opacity are multiplied by
//! `1 + value`, and opacity is then clamped to `0..=1`. `item.set_motion` writes the
//! whole list, and a keyframe scene can override it whole (`OverrideField::Motion`).
//!
//! An `audio` modifier ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md),
//! since 0.82) follows a measure of a bus or a source instead of the clock: the engine
//! measures what it hears each frame and follows it with the modifier's attack and
//! release.

use crate::finite::Finite32;
use crate::{BusId, SourceId};
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
#[non_exhaustive]
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
    /// A property this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
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
#[non_exhaustive]
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
    /// A shape this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
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

/// What an `audio` modifier measures of what it hears (since 0.82).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionMeasure {
    /// The RMS level of the latest window.
    #[default]
    Level,
    /// The peak absolute sample of the latest window.
    Peak,
    /// A jump of the level above its recent average: 1 on the jump, else 0.
    Onset,
    /// The spectrum between `low_hz` and `high_hz`, as an amplitude: a full-scale sine
    /// inside the band reads 0 dBFS.
    Band,
    /// A measure this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument` and draws a stored
    /// one as silence.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// How an `audio` modifier bends its 0 to 1 envelope before mapping it onto its range.
/// None overshoots (since 0.82).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionCurve {
    /// As it is.
    #[default]
    Linear,
    /// Cubic, slow at first: quiet passages stay low and only loud ones lift.
    EaseIn,
    /// Cubic, fast at first: quiet passages already show.
    EaseOut,
    /// Cubic at both ends.
    EaseInOut,
    /// A curve this version of the protocol doesn't know, from a newer server. Never
    /// sent; the engine refuses it in an edit with `invalid_argument`.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// What a modifier does, with its parameters, tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
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
    /// Follows a measure of a bus or a source (ADR-0100, since 0.82): the measure in dBFS from
    /// `floor_db` to `ceiling_db` is mapped onto 0 to 1 (an onset as it is), followed with
    /// `attack_ms` and `release_ms`, bent by `curve`, and mapped onto `from..to`. Silence
    /// is `from`. It hears what a `spectrum` source with the same `audio` and
    /// `audio_source` would draw, and silence while that is silent.
    Audio {
        /// The bus it hears, after its fader. Omitted for the program mix: the master
        /// after its filters, as the outputs hear it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio: Option<BusId>,
        /// A source to hear instead of the bus: that source after its own filters,
        /// fader, mute and follow. Omitted to hear the bus.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_source: Option<SourceId>,
        /// What it measures. Default `level`.
        #[serde(default)]
        measure: MotionMeasure,
        /// A `band` measure's lowest frequency, in hertz, `20..=20000`. Only with `band`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        low_hz: Option<Finite32>,
        /// A `band` measure's highest frequency, in hertz, above `low_hz` and at most
        /// 20000. Only with `band`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        high_hz: Option<Finite32>,
        /// The level, in dBFS, that maps to 0: at least -90 and below `ceiling_db`.
        /// Default -60.
        #[serde(default = "default_floor_db")]
        floor_db: Finite32,
        /// The level, in dBFS, that maps to 1: above `floor_db` and at most 0. Default 0.
        #[serde(default)]
        ceiling_db: Finite32,
        /// How fast it rises, in milliseconds, `0..=10000`. Default 10.
        #[serde(default = "default_attack_ms")]
        attack_ms: u32,
        /// How fast it falls, in milliseconds, `0..=10000`. Default 250.
        #[serde(default = "default_release_ms")]
        release_ms: u32,
        /// How the envelope is bent. Default `linear`.
        #[serde(default)]
        curve: MotionCurve,
        /// Its value at silence, in the property's unit, within `-100000..=100000`.
        /// Default 0.
        #[serde(default)]
        from: Finite32,
        /// Its value at the ceiling, within `-100000..=100000`; below `from` to invert.
        /// Default 1.
        #[serde(default = "default_to")]
        to: Finite32,
    },
    /// A kind this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

fn one_octave() -> u8 {
    1
}

fn default_floor_db() -> Finite32 {
    Finite32::new(-60.0).unwrap_or(Finite32::ZERO)
}

fn default_attack_ms() -> u32 {
    10
}

fn default_release_ms() -> u32 {
    250
}

fn default_to() -> Finite32 {
    Finite32::ONE
}

/// Where a modifier's time starts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MotionClock {
    /// The engine's animation time: every item with the same parameters moves in step.
    #[default]
    Engine,
    /// From when the engine last started drawing the item for program or a video mix,
    /// so it starts fresh each time it is shown. While it isn't shown, and in a scene
    /// or source preview, the modifier is at its start.
    Shown,
    /// A clock this version of the protocol doesn't know, from a newer server, so a
    /// 0.81 client can still read `state.get`. Never sent; the engine refuses it in an
    /// edit with `invalid_argument` and draws a stored one as still.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
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
        let unknown = "is not one this version of the protocol knows";
        if m.property == MotionProperty::Unknown {
            return Err(problem(field("property"), unknown));
        }
        if m.clock == MotionClock::Unknown {
            return Err(problem(field("clock"), unknown));
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
            MotionKind::Audio {
                measure,
                low_hz,
                high_hz,
                floor_db,
                ceiling_db,
                attack_ms,
                release_ms,
                curve,
                from,
                to,
                ..
            } => audio_problem(
                measure,
                (low_hz, high_hz),
                (floor_db, ceiling_db),
                (attack_ms, release_ms),
                curve,
                (from, to),
            ),
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
                shape,
                amplitude,
                period_ms,
                phase,
            } => {
                if !magnitude(amplitude) {
                    Some(("amplitude", "must be within 0..=100000"))
                } else if !(1..=MAX_PERIOD_MS).contains(&period_ms) {
                    Some(("period_ms", "must be within 1..=3600000"))
                } else if shape == MotionShape::Unknown {
                    Some(("shape", "is not one this version of the protocol knows"))
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
            MotionKind::Unknown => Some(("kind", "is not one this version of the protocol knows")),
        }
    }
}

/// The longest attack or release, in milliseconds.
const MAX_ENVELOPE_MS: u32 = 10_000;

/// An `audio` modifier's first parameter out of range (ADR-0100 decision 3).
fn audio_problem(
    measure: MotionMeasure,
    (low_hz, high_hz): (Option<Finite32>, Option<Finite32>),
    (floor_db, ceiling_db): (Finite32, Finite32),
    (attack_ms, release_ms): (u32, u32),
    curve: MotionCurve,
    (from, to): (Finite32, Finite32),
) -> Option<(&'static str, &'static str)> {
    let unknown = "is not one this version of the protocol knows";
    let hz = |v: Finite32| (20.0..=20_000.0).contains(&v.get());
    let end = |v: Finite32| v.get().abs() <= MAX_MAGNITUDE;
    match measure {
        MotionMeasure::Unknown => return Some(("measure", unknown)),
        MotionMeasure::Band => match (low_hz, high_hz) {
            (None, _) => return Some(("low_hz", "a band needs it")),
            (Some(low), _) if !hz(low) => return Some(("low_hz", "must be within 20..=20000")),
            (_, None) => return Some(("high_hz", "a band needs it")),
            (Some(low), Some(high)) if !(hz(high) && high.get() > low.get()) => {
                return Some(("high_hz", "must be above low_hz and at most 20000"));
            }
            _ => {}
        },
        _ => {
            if low_hz.is_some() {
                return Some(("low_hz", "only a band measure has it"));
            }
            if high_hz.is_some() {
                return Some(("high_hz", "only a band measure has it"));
            }
        }
    }
    if !(-90.0..0.0).contains(&floor_db.get()) {
        Some(("floor_db", "must be at least -90 and below 0"))
    } else if !(ceiling_db.get() <= 0.0 && ceiling_db.get() > floor_db.get()) {
        Some(("ceiling_db", "must be above floor_db and at most 0"))
    } else if attack_ms > MAX_ENVELOPE_MS {
        Some(("attack_ms", "must be within 0..=10000"))
    } else if release_ms > MAX_ENVELOPE_MS {
        Some(("release_ms", "must be within 0..=10000"))
    } else if curve == MotionCurve::Unknown {
        Some(("curve", unknown))
    } else if !end(from) {
        Some(("from", "must be within -100000..=100000"))
    } else if !end(to) {
        Some(("to", "must be within -100000..=100000"))
    } else {
        None
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

    /// #1125 review N2: a value from a newer server parses as `Unknown`, so an older
    /// client still reads `state.get`, and an edit carrying one is refused.
    #[test]
    fn a_newer_value_parses_as_unknown_and_is_refused() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let base = serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "drift", "rate_per_s": 1.0,
            "clock": "engine"
        });
        let with = |key: &str, value: serde_json::Value| {
            let mut json = base.clone();
            json[key] = value;
            json
        };
        let glow = parse(with("property", "glow".into()));
        assert_eq!(glow.property, MotionProperty::Unknown);
        assert_eq!(field_of(&[glow]).as_deref(), Some("motion.0.property"));
        let beat = parse(with("clock", "beat".into()));
        assert_eq!(beat.clock, MotionClock::Unknown);
        assert_eq!(field_of(&[beat]).as_deref(), Some("motion.0.clock"));
        let pulse = parse(serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "pulse", "every_ms": 5
        }));
        assert_eq!(pulse.kind, MotionKind::Unknown);
        assert_eq!(field_of(&[pulse]).as_deref(), Some("motion.0.kind"));
        let blob = parse(serde_json::json!({
            "modifier_id": "m", "property": "x", "kind": "oscillate", "shape": "blob",
            "amplitude": 1.0, "period_ms": 100
        }));
        assert_eq!(field_of(&[blob]).as_deref(), Some("motion.0.shape"));
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
                field_of(&[with(kind.clone())]),
                Some(format!("motion.0.{param}")),
                "{kind:?}"
            );
        }
    }

    /// ADR-0100: an `audio` modifier parses with its defaults, hears a bus or a source,
    /// and each of its parameters is checked by name.
    #[test]
    fn an_audio_modifier_takes_defaults_and_checks_each_parameter() {
        let parse = |json: serde_json::Value| -> MotionModifier {
            serde_json::from_value(json).expect("parses")
        };
        let light = parse(serde_json::json!({
            "modifier_id": "light", "property": "opacity", "kind": "audio",
            "from": -0.6, "to": 0.0
        }));
        let MotionKind::Audio {
            audio,
            audio_source,
            measure,
            floor_db,
            ceiling_db,
            attack_ms,
            release_ms,
            curve,
            ..
        } = &light.kind
        else {
            panic!("{light:?}");
        };
        assert_eq!((audio, audio_source), (&None, &None), "the program mix");
        assert_eq!(*measure, MotionMeasure::Level);
        assert_eq!((floor_db.get(), ceiling_db.get()), (-60.0, 0.0));
        assert_eq!((*attack_ms, *release_ms), (10, 250));
        assert_eq!(*curve, MotionCurve::Linear);
        assert_eq!(field_of(std::slice::from_ref(&light)), None);
        // It writes back what it read, and only what differs from a default is omitted.
        let back = serde_json::to_value(&light).expect("writes");
        assert_eq!(parse(back), light);

        let bass = parse(serde_json::json!({
            "modifier_id": "bass", "property": "scale", "kind": "audio",
            "audio": "music", "measure": "band", "low_hz": 40.0, "high_hz": 120.0,
            "curve": "ease_out", "attack_ms": 0, "release_ms": 400, "to": 0.1
        }));
        assert_eq!(field_of(std::slice::from_ref(&bass)), None);
        let MotionKind::Audio { audio, .. } = &bass.kind else {
            panic!("{bass:?}");
        };
        assert_eq!(audio.as_ref().map(BusId::as_str), Some("music"));

        let field = |json: serde_json::Value| field_of(&[parse(json)]);
        let audio = |extra: serde_json::Value| {
            let mut json = serde_json::json!({
                "modifier_id": "a", "property": "x", "kind": "audio"
            });
            for (k, v) in extra.as_object().expect("object") {
                json[k] = v.clone();
            }
            json
        };
        let cases = [
            (serde_json::json!({ "measure": "band" }), "low_hz"),
            (
                serde_json::json!({ "measure": "band", "low_hz": 10.0, "high_hz": 100.0 }),
                "low_hz",
            ),
            (
                serde_json::json!({ "measure": "band", "low_hz": 100.0 }),
                "high_hz",
            ),
            (
                serde_json::json!({ "measure": "band", "low_hz": 100.0, "high_hz": 100.0 }),
                "high_hz",
            ),
            (
                serde_json::json!({ "measure": "level", "low_hz": 100.0 }),
                "low_hz",
            ),
            (
                serde_json::json!({ "measure": "peak", "high_hz": 100.0 }),
                "high_hz",
            ),
            (serde_json::json!({ "measure": "loudness" }), "measure"),
            (serde_json::json!({ "floor_db": -91.0 }), "floor_db"),
            (serde_json::json!({ "floor_db": 0.0 }), "floor_db"),
            (
                serde_json::json!({ "floor_db": -20.0, "ceiling_db": -30.0 }),
                "ceiling_db",
            ),
            (serde_json::json!({ "ceiling_db": 1.0 }), "ceiling_db"),
            (serde_json::json!({ "attack_ms": 10_001 }), "attack_ms"),
            (serde_json::json!({ "release_ms": 10_001 }), "release_ms"),
            (serde_json::json!({ "curve": "spring" }), "curve"),
            (serde_json::json!({ "from": -100_001.0 }), "from"),
            (serde_json::json!({ "to": 100_001.0 }), "to"),
        ];
        for (extra, param) in cases {
            assert_eq!(
                field(audio(extra.clone())),
                Some(format!("motion.0.{param}")),
                "{extra}"
            );
        }
    }
}
