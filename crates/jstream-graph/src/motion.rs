//! Motion modifiers (ADR-0099 decision 3): checking an item's list as `item.set_motion`
//! writes it, and turning it into `jstream-anim`'s model, which evaluates it.
//!
//! The graph stores the wire form, so a read reports exactly what was written. The
//! checks are the wire's ([`check_motion`]), which the mock shares; every list they
//! accept, `jstream-anim`'s own check accepts too.
//!
//! Docs: docs/adr/0099-native-motion-modifiers-and-timelines.md

use crate::apply::invalid;
use jstream_anim::{Clock, Kind, Modifier, Property, Shape};
use jstream_protocol::{
    CameraMotionModifier, CameraProperty, MotionClock, MotionKind, MotionModifier, MotionProperty,
    MotionShape, ProtocolError, check_motion,
};

/// Refuses `motion` unless it passes the wire's check ([`check_motion`]): at most 8
/// modifiers, unique well-formed IDs, every parameter in range. The field is `motion`,
/// or `motion.<n>.<param>` for one modifier's.
pub(crate) fn check(motion: &[MotionModifier], at: usize) -> Result<(), ProtocolError> {
    check_motion(motion).map_err(|problem| invalid(at, &problem.field, problem.reason))
}

/// `motion` as `jstream-anim` evaluates it. A modifier with a value this version
/// doesn't know (`check_motion` refuses one, so only a list built without it can hold
/// one) is left out, so the item stays still for it.
pub fn anim_modifiers(motion: &[MotionModifier]) -> Vec<Modifier> {
    motion.iter().filter_map(modifier).collect()
}

/// A scene camera's motion (ADR-0113) as `jstream-anim` evaluates it: each modifier with
/// the camera property it adds to, the modifier itself adding to `x`, so evaluating it
/// alone gives its value. One with a value this version doesn't know is left out.
pub fn camera_anim_modifiers(motion: &[CameraMotionModifier]) -> Vec<(CameraProperty, Modifier)> {
    motion
        .iter()
        .filter(|m| m.property != CameraProperty::Unknown)
        .filter_map(|m| {
            let as_item = MotionModifier {
                modifier_id: m.modifier_id.clone(),
                property: MotionProperty::X,
                kind: m.kind,
                clock: m.clock,
                enabled: m.enabled,
            };
            Some((m.property, modifier(&as_item)?))
        })
        .collect()
}

fn modifier(m: &MotionModifier) -> Option<Modifier> {
    Some(Modifier {
        id: m.modifier_id.clone(),
        property: property(m.property)?,
        kind: kind(&m.kind)?,
        clock: match m.clock {
            MotionClock::Engine => Clock::Engine,
            MotionClock::Shown => Clock::Shown,
            _ => return None,
        },
        enabled: m.enabled,
    })
}

fn property(p: MotionProperty) -> Option<Property> {
    Some(match p {
        MotionProperty::X => Property::X,
        MotionProperty::Y => Property::Y,
        MotionProperty::Scale => Property::Scale,
        MotionProperty::Rotation => Property::Rotation,
        MotionProperty::Tilt => Property::Tilt,
        MotionProperty::Turn => Property::Turn,
        MotionProperty::Depth => Property::Depth,
        MotionProperty::Opacity => Property::Opacity,
        MotionProperty::CropLeft => Property::CropLeft,
        MotionProperty::CropTop => Property::CropTop,
        MotionProperty::CropRight => Property::CropRight,
        MotionProperty::CropBottom => Property::CropBottom,
        _ => return None,
    })
}

fn kind(k: &MotionKind) -> Option<Kind> {
    Some(match *k {
        MotionKind::Wiggle {
            amplitude,
            frequency_hz,
            seed,
            octaves,
        } => Kind::Wiggle {
            amplitude: f64::from(amplitude.get()),
            frequency_hz: f64::from(frequency_hz.get()),
            seed,
            octaves,
        },
        MotionKind::Oscillate {
            shape,
            amplitude,
            period_ms,
            phase,
        } => Kind::Oscillate {
            shape: match shape {
                MotionShape::Sine => Shape::Sine,
                MotionShape::Triangle => Shape::Triangle,
                MotionShape::Square => Shape::Square,
                MotionShape::Saw => Shape::Saw,
                _ => return None,
            },
            amplitude: f64::from(amplitude.get()),
            period_ms: f64::from(period_ms),
            phase: f64::from(phase.get()),
        },
        MotionKind::Drift { rate_per_s, wrap } => Kind::Drift {
            rate_per_s: f64::from(rate_per_s.get()),
            wrap: wrap.map(|w| f64::from(w.get())),
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_protocol::Finite32;

    /// Every list the wire's check accepts, `jstream-anim`'s accepts too, at the edges
    /// of each range, so the render thread never meets a list its model refuses.
    #[test]
    fn the_anim_check_accepts_what_the_wire_accepts() {
        let f = |v: f32| Finite32::new(v).expect("finite");
        let kinds = [
            MotionKind::Wiggle {
                amplitude: f(100_000.0),
                frequency_hz: f(60.0),
                seed: u32::MAX,
                octaves: 4,
            },
            MotionKind::Wiggle {
                amplitude: f(0.0),
                frequency_hz: f(f32::MIN_POSITIVE),
                seed: 0,
                octaves: 1,
            },
            MotionKind::Oscillate {
                shape: MotionShape::Saw,
                amplitude: f(100_000.0),
                period_ms: 3_600_000,
                phase: f(0.999_999),
            },
            MotionKind::Oscillate {
                shape: MotionShape::Square,
                amplitude: f(0.0),
                period_ms: 1,
                phase: f(0.0),
            },
            MotionKind::Drift {
                rate_per_s: f(-100_000.0),
                wrap: Some(f(100_000.0)),
            },
            MotionKind::Drift {
                rate_per_s: f(100_000.0),
                wrap: Some(f(f32::MIN_POSITIVE)),
            },
        ];
        let motion: Vec<MotionModifier> = kinds
            .iter()
            .enumerate()
            .map(|(n, kind)| MotionModifier {
                modifier_id: format!("m-{n}_{}", "x".repeat(50)),
                property: MotionProperty::ALL[n],
                kind: *kind,
                clock: MotionClock::Shown,
                enabled: n % 2 == 0,
            })
            .collect();
        assert_eq!(check_motion(&motion), Ok(()));
        assert_eq!(jstream_anim::check_list(&anim_modifiers(&motion)), Ok(()));
    }
}
