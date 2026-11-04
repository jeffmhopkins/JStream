//! Motion modifiers ([ADR-0099](../../../docs/adr/0099-native-motion-modifiers-and-timelines.md)
//! decision 3): continuous changes to an item's properties, evaluated as pure functions of
//! the animation time, never stored. An audio modifier
//! ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md)) also reads an
//! envelope level the evaluator hands in.

use crate::audio::Audio;

/// The most modifiers one item holds (ADR-0099 decision 5).
pub const MAX_MODIFIERS: usize = 8;

/// The property a modifier changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Property {
    /// Horizontal position, in canvas pixels, added.
    X,
    /// Vertical position, in canvas pixels, added.
    Y,
    /// Size about the item's centre: the value `v` multiplies it by `1 + v`.
    Scale,
    /// In-plane rotation, in degrees, added.
    Rotation,
    /// The 3D tilt about the horizontal axis (ADR-0094), in degrees, added.
    Tilt,
    /// The 3D turn about the vertical axis (ADR-0094), in degrees, added.
    Turn,
    /// Opacity: the value `v` multiplies it by `1 + v`, clamped to `0..=1` after.
    Opacity,
    /// Source pixels cut off the left edge, added.
    CropLeft,
    /// Source pixels cut off the top edge, added.
    CropTop,
    /// Source pixels cut off the right edge, added.
    CropRight,
    /// Source pixels cut off the bottom edge, added.
    CropBottom,
}

impl Property {
    /// Every property, in wire order.
    pub const ALL: [Property; 11] = [
        Property::X,
        Property::Y,
        Property::Scale,
        Property::Rotation,
        Property::Tilt,
        Property::Turn,
        Property::Opacity,
        Property::CropLeft,
        Property::CropTop,
        Property::CropRight,
        Property::CropBottom,
    ];

    /// Its wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Property::X => "x",
            Property::Y => "y",
            Property::Scale => "scale",
            Property::Rotation => "rotation",
            Property::Tilt => "tilt",
            Property::Turn => "turn",
            Property::Opacity => "opacity",
            Property::CropLeft => "crop_left",
            Property::CropTop => "crop_top",
            Property::CropRight => "crop_right",
            Property::CropBottom => "crop_bottom",
        }
    }

    /// The property with wire name `name`.
    pub fn parse(name: &str) -> Option<Property> {
        Property::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

/// The waveform of an [`Kind::Oscillate`] modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// A sine: starts at 0 rising.
    Sine,
    /// A triangle: starts at 0 rising, linear between -1 and 1.
    Triangle,
    /// A square: 1 for the first half of each period, -1 for the second.
    Square,
    /// A sawtooth: rises linearly from -1 to 1 over each period, then jumps back.
    Saw,
}

impl Shape {
    /// The waveform's value in `-1..=1` at `cycle`, the fraction of a period in `0..1`.
    fn at(self, cycle: f64) -> f64 {
        match self {
            Shape::Sine => (cycle * std::f64::consts::TAU).sin(),
            Shape::Triangle => {
                // 0 → 0, 0.25 → 1, 0.75 → -1, 1 → 0.
                let c = (cycle + 0.25).rem_euclid(1.0);
                1.0 - 4.0 * (c - 0.5).abs()
            }
            Shape::Square => {
                if cycle < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Shape::Saw => 2.0 * cycle - 1.0,
        }
    }
}

/// What a modifier does, with its parameters. More kinds may follow, so code outside this
/// crate matches it with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Kind {
    /// Smooth seeded noise within `-amplitude..=amplitude`. The same seed gives the same
    /// motion on every machine. Each further octave adds detail at twice the frequency
    /// and half the amplitude, and the sum is scaled back to stay within the amplitude.
    Wiggle {
        /// The largest value it reaches.
        amplitude: f64,
        /// About how many turns of direction a second, for the first octave.
        frequency_hz: f64,
        /// Picks the noise.
        seed: u32,
        /// How many octaves, `1..=4`.
        octaves: u8,
    },
    /// A periodic wave within `-amplitude..=amplitude`.
    Oscillate {
        /// Its waveform.
        shape: Shape,
        /// The largest value it reaches.
        amplitude: f64,
        /// One period, in milliseconds.
        period_ms: f64,
        /// Its offset into the period, as a fraction in `0..1`.
        phase: f64,
    },
    /// A constant rate per second, wrapping into `0..wrap` when `wrap` is set.
    Drift {
        /// Units per second, of either sign.
        rate_per_s: f64,
        /// Where it wraps back to 0, if it does.
        wrap: Option<f64>,
    },
    /// Follows a measure of a bus or a source ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md)),
    /// through an [`Envelope`](crate::Envelope) the evaluator keeps per item and
    /// modifier. What it hears is named beside the modifier, outside this crate.
    Audio(Audio),
}

/// Where a modifier's time starts (ADR-0099 decision 3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Clock {
    /// The animation time: every item with the same parameters moves in step.
    #[default]
    Engine,
    /// From when the item was last shown on program, so it starts fresh each time.
    Shown,
}

/// One motion modifier on an item.
#[derive(Clone, Debug, PartialEq)]
pub struct Modifier {
    /// Unique within the item's list.
    pub id: String,
    /// What it changes.
    pub property: Property,
    /// What it does.
    pub kind: Kind,
    /// Where its time starts.
    pub clock: Clock,
    /// Whether it applies. A disabled modifier contributes nothing.
    pub enabled: bool,
}

/// Why a modifier, or an item's list of them, is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvalidModifier {
    /// The list holds more than [`MAX_MODIFIERS`].
    TooMany,
    /// Two modifiers share this ID.
    DuplicateId(String),
    /// A modifier's ID is empty.
    EmptyId,
    /// The parameter, by wire name, of the modifier with this ID is out of range or
    /// not finite.
    Parameter {
        /// The modifier.
        id: String,
        /// The parameter's wire name.
        param: &'static str,
    },
}

/// The largest amplitude or wrap any modifier may have, in its property's unit.
const MAX_MAGNITUDE: f64 = 100_000.0;
/// The highest wiggle frequency, in hertz.
const MAX_FREQUENCY_HZ: f64 = 60.0;
/// The longest oscillator period, in milliseconds: one hour.
const MAX_PERIOD_MS: f64 = 3_600_000.0;

impl Modifier {
    /// The first parameter that is out of range, by wire name, or `None` when all are
    /// valid.
    pub fn problem(&self) -> Option<&'static str> {
        let magnitude = |v: f64| v.is_finite() && (0.0..=MAX_MAGNITUDE).contains(&v);
        match self.kind {
            Kind::Wiggle {
                amplitude,
                frequency_hz,
                octaves,
                ..
            } => {
                if !magnitude(amplitude) {
                    Some("amplitude")
                } else if !(frequency_hz.is_finite()
                    && frequency_hz > 0.0
                    && frequency_hz <= MAX_FREQUENCY_HZ)
                {
                    Some("frequency_hz")
                } else if !(1..=4).contains(&octaves) {
                    Some("octaves")
                } else {
                    None
                }
            }
            Kind::Oscillate {
                amplitude,
                period_ms,
                phase,
                ..
            } => {
                if !magnitude(amplitude) {
                    Some("amplitude")
                } else if !((1.0..=MAX_PERIOD_MS).contains(&period_ms)) {
                    Some("period_ms")
                } else if !(phase.is_finite() && (0.0..1.0).contains(&phase)) {
                    Some("phase")
                } else {
                    None
                }
            }
            Kind::Audio(audio) => audio.problem(MAX_MAGNITUDE),
            Kind::Drift { rate_per_s, wrap } => {
                if !(rate_per_s.is_finite() && rate_per_s.abs() <= MAX_MAGNITUDE) {
                    Some("rate_per_s")
                } else if wrap.is_some_and(|w| !(magnitude(w) && w > 0.0)) {
                    Some("wrap")
                } else {
                    None
                }
            }
        }
    }

    /// Its value at `seconds` on its clock, before it is applied to the property. A
    /// disabled modifier is 0. An audio modifier reads as silence here: its `from`
    /// ([`Modifier::value_heard`] takes its envelope).
    pub fn value_at(&self, seconds: f64) -> f64 {
        self.value_heard(seconds, 0.0)
    }

    /// Its value at `seconds` on its clock, with `envelope` (`0..=1`) as an audio
    /// modifier's envelope level. Every other kind ignores `envelope`. A disabled
    /// modifier is 0.
    pub fn value_heard(&self, seconds: f64, envelope: f32) -> f64 {
        if !self.enabled {
            return 0.0;
        }
        // `check_list` refuses every parameter that could make this non-finite (a period
        // under 1 ms, a zero wrap, a NaN); for unchecked input, any non-finite value is 0,
        // so nothing the render thread draws from it is NaN or infinite.
        let v = match self.kind {
            Kind::Wiggle {
                amplitude,
                frequency_hz,
                seed,
                octaves,
            } => amplitude * fractal_noise(seed, seconds * frequency_hz, octaves),
            Kind::Oscillate {
                shape,
                amplitude,
                period_ms,
                phase,
            } => {
                // A period that isn't positive has no cycle.
                if period_ms.is_nan() || period_ms <= 0.0 {
                    return 0.0;
                }
                let cycle = (seconds * 1000.0 / period_ms + phase).rem_euclid(1.0);
                amplitude * shape.at(cycle)
            }
            Kind::Drift { rate_per_s, wrap } => {
                let travelled = rate_per_s * seconds;
                match wrap {
                    Some(w) => travelled.rem_euclid(w),
                    None => travelled,
                }
            }
            Kind::Audio(audio) => audio.value(envelope),
        };
        if v.is_finite() { v } else { 0.0 }
    }
}

/// Checks an item's whole list: at most [`MAX_MODIFIERS`], unique non-empty IDs, every
/// parameter in range. Returns the first problem.
pub fn check_list(modifiers: &[Modifier]) -> Result<(), InvalidModifier> {
    if modifiers.len() > MAX_MODIFIERS {
        return Err(InvalidModifier::TooMany);
    }
    for (i, m) in modifiers.iter().enumerate() {
        if m.id.is_empty() {
            return Err(InvalidModifier::EmptyId);
        }
        if modifiers[..i].iter().any(|other| other.id == m.id) {
            return Err(InvalidModifier::DuplicateId(m.id.clone()));
        }
        if let Some(param) = m.problem() {
            return Err(InvalidModifier::Parameter {
                id: m.id.clone(),
                param,
            });
        }
    }
    Ok(())
}

/// What an item's modifiers add to it at one moment: an offset per additive property and
/// a factor per multiplied one. [`Offsets::IDENTITY`] changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offsets {
    /// Added to x, in canvas pixels.
    pub x: f32,
    /// Added to y, in canvas pixels.
    pub y: f32,
    /// Multiplies the size about the centre.
    pub scale: f32,
    /// Added to the rotation, in degrees.
    pub rotation: f32,
    /// Added to the tilt, in degrees.
    pub tilt: f32,
    /// Added to the turn, in degrees.
    pub turn: f32,
    /// Multiplies the opacity, which is then clamped to `0..=1` ([`Offsets::opacity_of`]).
    pub opacity: f32,
    /// Added to each crop edge, in source pixels: left, top, right, bottom.
    pub crop: [f32; 4],
}

impl Offsets {
    /// No change.
    pub const IDENTITY: Offsets = Offsets {
        x: 0.0,
        y: 0.0,
        scale: 1.0,
        rotation: 0.0,
        tilt: 0.0,
        turn: 0.0,
        opacity: 1.0,
        crop: [0.0; 4],
    };

    /// Whether it changes nothing.
    pub fn is_identity(&self) -> bool {
        *self == Offsets::IDENTITY
    }

    /// Applies one modifier's `value` to its property.
    fn add(&mut self, property: Property, value: f64) {
        let v = value as f32;
        match property {
            Property::X => self.x += v,
            Property::Y => self.y += v,
            Property::Scale => self.scale *= 1.0 + v,
            Property::Rotation => self.rotation += v,
            Property::Tilt => self.tilt += v,
            Property::Turn => self.turn += v,
            Property::Opacity => self.opacity *= 1.0 + v,
            Property::CropLeft => self.crop[0] += v,
            Property::CropTop => self.crop[1] += v,
            Property::CropRight => self.crop[2] += v,
            Property::CropBottom => self.crop[3] += v,
        }
    }

    /// An item's `opacity` with these offsets applied, clamped to `0..=1`.
    pub fn opacity_of(&self, opacity: f32) -> f32 {
        (opacity * self.opacity).clamp(0.0, 1.0)
    }

    /// An item's scale with these offsets applied, never negative.
    pub fn scale_of(&self, scale: f32) -> f32 {
        (scale * self.scale).max(0.0)
    }

    /// The blend from `self` (progress 0) to `to` (progress 1) at `progress`, which a
    /// spring may take past 1. During a Move between keyframes the item evaluates both
    /// sides' modifiers and blends their outputs, so every wave keeps its phase
    /// (ADR-0099 decision 3).
    pub fn blend(&self, to: &Offsets, progress: f32) -> Offsets {
        let lerp = |a: f32, b: f32| a + (b - a) * progress;
        Offsets {
            x: lerp(self.x, to.x),
            y: lerp(self.y, to.y),
            scale: lerp(self.scale, to.scale),
            rotation: lerp(self.rotation, to.rotation),
            tilt: lerp(self.tilt, to.tilt),
            turn: lerp(self.turn, to.turn),
            opacity: lerp(self.opacity, to.opacity),
            crop: [
                lerp(self.crop[0], to.crop[0]),
                lerp(self.crop[1], to.crop[1]),
                lerp(self.crop[2], to.crop[2]),
                lerp(self.crop[3], to.crop[3]),
            ],
        }
    }
}

/// Evaluates an item's modifiers at one moment, in list order. The list should have
/// passed [`check_list`]; unchecked input never panics or loops, and an oscillator with
/// a period that isn't positive contributes 0. `engine_s` is the
/// animation time in seconds; `shown_s` is the time since the item was last shown on
/// program, `None` when it isn't shown, in which case [`Clock::Shown`] modifiers are at
/// their start. Allocates nothing, so the render thread calls it per item per frame.
pub fn evaluate(modifiers: &[Modifier], engine_s: f64, shown_s: Option<f64>) -> Offsets {
    evaluate_with(modifiers, engine_s, shown_s, |_| None)
}

/// [`evaluate`], with `heard` giving each audio modifier's envelope level (`0..=1`) by
/// modifier ID: the evaluator's [`Envelope`](crate::Envelope) for that item and
/// modifier. An audio modifier `heard` has no level for reads as silence. Allocates
/// nothing.
pub fn evaluate_with(
    modifiers: &[Modifier],
    engine_s: f64,
    shown_s: Option<f64>,
    heard: impl Fn(&str) -> Option<f32>,
) -> Offsets {
    let mut out = Offsets::IDENTITY;
    for m in modifiers {
        let t = match m.clock {
            Clock::Engine => engine_s,
            Clock::Shown => shown_s.unwrap_or(0.0),
        };
        let envelope = match m.kind {
            Kind::Audio(_) => heard(&m.id).unwrap_or(0.0),
            _ => 0.0,
        };
        out.add(m.property, m.value_heard(t, envelope));
    }
    out
}

/// A 32-bit integer hash (lowbias32), for lattice values of the noise.
fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// The noise's value at integer `lattice` for `seed`, in `-1..=1`.
fn lattice(seed: u32, lattice: i64) -> f64 {
    let folded = (lattice as u64 ^ ((lattice as u64) >> 32)) as u32;
    let h = hash(seed ^ hash(folded.wrapping_add(0x9e37_79b9)));
    f64::from(h) / f64::from(u32::MAX) * 2.0 - 1.0
}

/// Smooth 1D value noise at `x` in `-1..=1`: the lattice values eased together with a
/// quintic, so its slope is continuous.
fn value_noise(seed: u32, x: f64) -> f64 {
    let floor = x.floor();
    let i = floor as i64;
    let f = x - floor;
    let ease = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = lattice(seed, i);
    let b = lattice(seed, i.wrapping_add(1));
    a + (b - a) * ease
}

/// `octaves` of value noise, each twice the frequency and half the weight of the last,
/// scaled back to `-1..=1`.
fn fractal_noise(seed: u32, x: f64, octaves: u8) -> f64 {
    let mut sum = 0.0;
    let mut weight = 1.0;
    let mut total = 0.0;
    let mut freq = 1.0;
    for octave in 0..octaves.max(1) {
        sum += weight * value_noise(seed.wrapping_add(u32::from(octave)), x * freq);
        total += weight;
        weight *= 0.5;
        freq *= 2.0;
    }
    sum / total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modifier(id: &str, property: Property, kind: Kind) -> Modifier {
        Modifier {
            id: id.into(),
            property,
            kind,
            clock: Clock::Engine,
            enabled: true,
        }
    }

    fn sine(amplitude: f64, period_ms: f64) -> Kind {
        Kind::Oscillate {
            shape: Shape::Sine,
            amplitude,
            period_ms,
            phase: 0.0,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn wire_names_round_trip() {
        for p in Property::ALL {
            assert_eq!(Property::parse(p.as_str()), Some(p));
        }
        assert_eq!(Property::parse("width"), None);
    }

    #[test]
    fn each_shape_hits_its_landmarks() {
        let at = |shape: Shape, cycle: f64| shape.at(cycle);
        assert!(close(at(Shape::Sine, 0.0), 0.0));
        assert!(close(at(Shape::Sine, 0.25), 1.0));
        assert!(close(at(Shape::Triangle, 0.0), 0.0));
        assert!(close(at(Shape::Triangle, 0.25), 1.0));
        assert!(close(at(Shape::Triangle, 0.5), 0.0));
        assert!(close(at(Shape::Triangle, 0.75), -1.0));
        assert!(close(at(Shape::Square, 0.1), 1.0));
        assert!(close(at(Shape::Square, 0.6), -1.0));
        assert!(close(at(Shape::Saw, 0.0), -1.0));
        assert!(close(at(Shape::Saw, 0.5), 0.0));
    }

    #[test]
    fn an_oscillator_repeats_with_its_period_and_phase() {
        let m = modifier("osc", Property::X, sine(3.0, 4000.0));
        assert!(close(m.value_at(1.0), 3.0), "a quarter period in");
        assert!(close(m.value_at(5.0), 3.0), "one period later");
        let shifted = Modifier {
            kind: Kind::Oscillate {
                shape: Shape::Sine,
                amplitude: 3.0,
                period_ms: 4000.0,
                phase: 0.25,
            },
            ..m.clone()
        };
        assert!(close(shifted.value_at(0.0), m.value_at(1.0)));
    }

    #[test]
    fn a_drift_wraps_and_runs_backwards() {
        let wrap = modifier(
            "drift",
            Property::X,
            Kind::Drift {
                rate_per_s: 10.0,
                wrap: Some(100.0),
            },
        );
        assert!(close(wrap.value_at(3.0), 30.0));
        assert!(close(wrap.value_at(12.0), 20.0));
        let back = modifier(
            "drift",
            Property::X,
            Kind::Drift {
                rate_per_s: -10.0,
                wrap: Some(100.0),
            },
        );
        assert!(close(back.value_at(3.0), 70.0), "wraps into 0..wrap");
        let open = modifier(
            "drift",
            Property::X,
            Kind::Drift {
                rate_per_s: 2.5,
                wrap: None,
            },
        );
        assert!(close(open.value_at(4.0), 10.0));
    }

    #[test]
    fn a_wiggle_is_seeded_bounded_and_smooth() {
        let wiggle = |seed| {
            modifier(
                "w",
                Property::Rotation,
                Kind::Wiggle {
                    amplitude: 0.1,
                    frequency_hz: 0.5,
                    seed,
                    octaves: 3,
                },
            )
        };
        let a = wiggle(7);
        let b = wiggle(8);
        let mut differs = false;
        let mut last = a.value_at(0.0);
        for step in 1..20_000 {
            let t = f64::from(step) * 0.005;
            let v = a.value_at(t);
            assert!(v.abs() <= 0.1 + 1e-12, "within its amplitude at {t}");
            assert_eq!(
                v,
                wiggle(7).value_at(t),
                "the same seed gives the same value"
            );
            assert!((v - last).abs() < 0.01, "no jump between frames at {t}");
            differs |= (v - b.value_at(t)).abs() > 1e-6;
            last = v;
        }
        assert!(differs, "another seed gives other motion");
    }

    #[test]
    fn a_wiggle_stays_precise_after_days() {
        let m = modifier(
            "w",
            Property::X,
            Kind::Wiggle {
                amplitude: 3.0,
                frequency_hz: 1.0,
                seed: 1,
                octaves: 1,
            },
        );
        let day = 86_400.0 * 3.0;
        let frame = 1.0 / 60.0;
        // One octave of amplitude 3 at 1 Hz moves at most 3 × 2 × 1.875 per second
        // (the quintic's steepest slope), so about 0.19 a frame.
        let step = (m.value_at(day + frame) - m.value_at(day)).abs();
        assert!(
            step < 0.2,
            "a frame's change stays small three days in: {step}"
        );
        // Three days is a whole number of 4 s periods, so an oscillator is back where it
        // was after 1 s, to well under a millionth.
        let osc = modifier("o", Property::X, sine(3.0, 4000.0));
        let drift = (osc.value_at(day + 1.0) - osc.value_at(1.0)).abs();
        assert!(drift < 1e-6, "no precision lost three days in: {drift}");
    }

    #[test]
    fn unchecked_bad_periods_give_zero_not_nan() {
        for period_ms in [0.0, -5.0, f64::NAN] {
            let m = modifier("o", Property::Opacity, sine(1.0, period_ms));
            assert_eq!(m.value_at(2.0), 0.0, "{period_ms}");
            assert!(evaluate(&[m], 2.0, None).opacity_of(1.0).is_finite());
        }
    }

    #[test]
    fn any_unchecked_input_gives_a_finite_value() {
        let bad = [
            // Positive but subnormal: 2 s over it is infinite.
            sine(1.0, 1e-310),
            sine(f64::NAN, 1000.0),
            Kind::Oscillate {
                shape: Shape::Sine,
                amplitude: 1.0,
                period_ms: 1000.0,
                phase: f64::NAN,
            },
            Kind::Wiggle {
                amplitude: 1.0,
                frequency_hz: f64::NAN,
                seed: 1,
                octaves: 1,
            },
            Kind::Drift {
                rate_per_s: 1.0,
                wrap: Some(0.0),
            },
            Kind::Drift {
                rate_per_s: f64::INFINITY,
                wrap: None,
            },
        ];
        for kind in bad {
            let m = modifier("x", Property::Opacity, kind);
            assert!(check_list(std::slice::from_ref(&m)).is_err(), "{kind:?}");
            assert_eq!(m.value_at(2.0), 0.0, "{kind:?}");
            assert!(evaluate(&[m], 2.0, None).opacity_of(1.0).is_finite());
        }
    }

    #[test]
    fn evaluate_adds_multiplies_and_skips_disabled() {
        let mods = [
            modifier(
                "a",
                Property::X,
                Kind::Drift {
                    rate_per_s: 2.0,
                    wrap: None,
                },
            ),
            modifier(
                "b",
                Property::X,
                Kind::Drift {
                    rate_per_s: 1.0,
                    wrap: None,
                },
            ),
            modifier(
                "c",
                Property::Scale,
                Kind::Drift {
                    rate_per_s: 0.5,
                    wrap: None,
                },
            ),
            modifier(
                "d",
                Property::Opacity,
                Kind::Drift {
                    rate_per_s: -0.25,
                    wrap: None,
                },
            ),
            Modifier {
                enabled: false,
                ..modifier(
                    "e",
                    Property::Y,
                    Kind::Drift {
                        rate_per_s: 9.0,
                        wrap: None,
                    },
                )
            },
        ];
        let out = evaluate(&mods, 2.0, None);
        assert_eq!(out.x, 6.0);
        assert_eq!(out.y, 0.0, "a disabled modifier adds nothing");
        assert_eq!(out.scale, 2.0);
        assert_eq!(out.opacity, 0.5);
        assert_eq!(out.opacity_of(0.8), 0.4);
        assert_eq!(evaluate(&[], 5.0, None), Offsets::IDENTITY);
    }

    #[test]
    fn an_audio_modifier_reads_its_envelope_by_id() {
        use crate::{Envelope, Measure};
        let pulse = Audio {
            from: -0.6,
            to: 0.0,
            ..Audio::default()
        };
        let push = Audio {
            measure: Measure::Band {
                low_hz: 40.0,
                high_hz: 120.0,
            },
            from: 0.0,
            to: 6.0,
            ..Audio::default()
        };
        let mods = [
            modifier("light", Property::Opacity, Kind::Audio(pulse)),
            modifier("bass", Property::X, Kind::Audio(push)),
            modifier("sway", Property::X, sine(2.0, 1000.0)),
        ];
        assert_eq!(check_list(&mods), Ok(()));
        // Unheard, each reads as silence: its `from`.
        let silent = evaluate(&mods, 0.25, None);
        assert!((silent.opacity - 0.4).abs() < 1e-6);
        assert!((silent.x - 2.0).abs() < 1e-6);
        // Heard: full on the light, half on the bass, the sine added as before.
        let out = evaluate_with(&mods, 0.25, None, |id| match id {
            "light" => Some(1.0),
            "bass" => Some(0.5),
            _ => None,
        });
        assert!((out.opacity - 1.0).abs() < 1e-6);
        assert!((out.x - 5.0).abs() < 1e-6);
        // An envelope fed full scale drives it to the top of its range.
        let mut env = Envelope::default();
        for _ in 0..60 {
            env.advance(&pulse, 1.0, 1.0 / 60.0);
        }
        let lit = evaluate_with(&mods[..1], 0.0, None, |_| Some(env.level()));
        assert!((lit.opacity_of(1.0) - 1.0).abs() < 1e-4);
        // A disabled audio modifier adds nothing, heard or not.
        let off = Modifier {
            enabled: false,
            ..mods[1].clone()
        };
        assert_eq!(
            evaluate_with(&[off], 0.0, None, |_| Some(1.0)),
            Offsets::IDENTITY
        );
    }

    #[test]
    fn a_bad_audio_parameter_is_refused_by_name() {
        let bad = Audio {
            attack_ms: f32::NAN,
            ..Audio::default()
        };
        let m = modifier("a", Property::Opacity, Kind::Audio(bad));
        assert_eq!(
            check_list(std::slice::from_ref(&m)),
            Err(InvalidModifier::Parameter {
                id: "a".into(),
                param: "attack_ms"
            })
        );
        let huge = Audio {
            to: 1e9,
            ..Audio::default()
        };
        let m = modifier("b", Property::X, Kind::Audio(huge));
        assert!(check_list(std::slice::from_ref(&m)).is_err());
    }

    #[test]
    fn opacity_and_scale_are_clamped_when_applied() {
        let up = Offsets {
            opacity: 1.5,
            scale: -0.5,
            ..Offsets::IDENTITY
        };
        assert_eq!(up.opacity_of(0.9), 1.0);
        assert_eq!(up.scale_of(1.0), 0.0);
    }

    #[test]
    fn a_shown_clock_starts_when_the_item_is_shown() {
        let m = Modifier {
            clock: Clock::Shown,
            ..modifier(
                "p",
                Property::X,
                Kind::Drift {
                    rate_per_s: 10.0,
                    wrap: None,
                },
            )
        };
        let mods = [m];
        assert_eq!(evaluate(&mods, 1000.0, Some(1.5)).x, 15.0);
        assert_eq!(
            evaluate(&mods, 1000.0, None).x,
            0.0,
            "at its start while hidden"
        );
    }

    #[test]
    fn blending_is_continuous_and_ends_on_each_side() {
        let a = Offsets {
            x: 2.0,
            rotation: 0.1,
            ..Offsets::IDENTITY
        };
        let b = Offsets {
            x: -4.0,
            opacity: 0.5,
            ..Offsets::IDENTITY
        };
        assert_eq!(a.blend(&b, 0.0), a);
        assert_eq!(a.blend(&b, 1.0), b);
        let mid = a.blend(&b, 0.5);
        assert_eq!(mid.x, -1.0);
        assert_eq!(mid.opacity, 0.75);
        assert!(a.blend(&b, 1.1).x < b.x, "a spring overshoots");
    }

    #[test]
    fn check_list_refuses_each_problem() {
        let ok = modifier("a", Property::X, sine(3.0, 4000.0));
        assert_eq!(check_list(std::slice::from_ref(&ok)), Ok(()));
        let many: Vec<_> = (0..=MAX_MODIFIERS)
            .map(|i| modifier(&i.to_string(), Property::X, sine(1.0, 100.0)))
            .collect();
        assert_eq!(check_list(&many), Err(InvalidModifier::TooMany));
        assert_eq!(
            check_list(&[ok.clone(), ok.clone()]),
            Err(InvalidModifier::DuplicateId("a".into()))
        );
        assert_eq!(
            check_list(&[modifier("", Property::X, sine(1.0, 100.0))]),
            Err(InvalidModifier::EmptyId)
        );
        let bad = |kind, param| {
            assert_eq!(
                check_list(&[modifier("m", Property::X, kind)]),
                Err(InvalidModifier::Parameter {
                    id: "m".into(),
                    param
                }),
                "{param}"
            );
        };
        bad(sine(f64::NAN, 100.0), "amplitude");
        bad(sine(-1.0, 100.0), "amplitude");
        bad(sine(1.0, 0.5), "period_ms");
        bad(
            Kind::Oscillate {
                shape: Shape::Saw,
                amplitude: 1.0,
                period_ms: 100.0,
                phase: 1.0,
            },
            "phase",
        );
        bad(
            Kind::Wiggle {
                amplitude: 1.0,
                frequency_hz: 0.0,
                seed: 0,
                octaves: 1,
            },
            "frequency_hz",
        );
        bad(
            Kind::Wiggle {
                amplitude: 1.0,
                frequency_hz: 1.0,
                seed: 0,
                octaves: 5,
            },
            "octaves",
        );
        bad(
            Kind::Drift {
                rate_per_s: f64::INFINITY,
                wrap: None,
            },
            "rate_per_s",
        );
        bad(
            Kind::Drift {
                rate_per_s: 1.0,
                wrap: Some(0.0),
            },
            "wrap",
        );
    }
}
