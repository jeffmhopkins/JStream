//! MIDI-driven modifiers ([ADR-0168](../../../docs/adr/0168-midi-controllers-drive-motion.md)):
//! how a controller's value becomes a modifier's value. Each step is a pure function, so
//! a client previews what the engine draws:
//!
//! 1. The engine reads the controller's latest value and maps it onto `0..=1`
//!    ([`Midi::normalize_cc`], [`Midi::normalize_bend`]).
//! 2. [`Smoothing::advance`] follows that over `smoothing_ms`. It is the one piece of
//!    state, kept per item and modifier by whoever evaluates; 0 ms jumps.
//! 3. [`Midi::value`] bends it with the [`Curve`] and maps it onto `from..to`.
//!
//! Which input, channel and controller it reads stay outside this crate: the engine
//! reads them, and hands in a number per frame.

use crate::audio::{Curve, MAX_ENVELOPE_MS};

/// The parameters of a MIDI-driven modifier ([`crate::Kind::Midi`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Midi {
    /// How long it takes to follow a new value, in milliseconds, `0..=10000`: the time
    /// constant of a one-pole smoothing. 0 jumps.
    pub smoothing_ms: f32,
    /// How the `0..=1` value is bent.
    pub curve: Curve,
    /// The modifier's value at the controller's lowest, in its property's unit.
    pub from: f64,
    /// Its value at the controller's highest. It may be below `from`, to invert.
    pub to: f64,
}

impl Default for Midi {
    /// A 50 ms smoothing, linear, onto `0..1`.
    fn default() -> Self {
        Self {
            smoothing_ms: 50.0,
            curve: Curve::Linear,
            from: 0.0,
            to: 1.0,
        }
    }
}

impl Midi {
    /// The first parameter out of range, by wire name, or `None`. `magnitude` is the
    /// largest `from` or `to` the caller allows.
    pub(crate) fn problem(&self, magnitude: f64) -> Option<&'static str> {
        let end = |v: f64| v.is_finite() && v.abs() <= magnitude;
        if !(self.smoothing_ms.is_finite() && (0.0..=MAX_ENVELOPE_MS).contains(&self.smoothing_ms))
        {
            Some("smoothing_ms")
        } else if !end(self.from) {
            Some("from")
        } else if !end(self.to) {
            Some("to")
        } else {
            None
        }
    }

    /// A 7-bit value (a control change or channel pressure, 0 to 127) as `0..=1`.
    pub fn normalize_cc(value: u8) -> f32 {
        f32::from(value.min(127)) / 127.0
    }

    /// A pitch bend, -8192 to 8191, as `0..=1`: the wheel at rest (0) is one half, and
    /// each end reaches its end.
    pub fn normalize_bend(value: i16) -> f32 {
        let v = f32::from(value.clamp(-8192, 8191));
        if v < 0.0 {
            0.5 + v / 16384.0
        } else {
            0.5 + v / 16382.0
        }
    }

    /// `smoothed` (`0..=1`) bent with the curve and mapped onto `from..to`.
    pub fn value(&self, smoothed: f32) -> f64 {
        let t = f64::from(self.curve.at(smoothed));
        self.from + (self.to - self.from) * t
    }
}

/// A MIDI modifier's smoothing state: the followed `0..=1` value. One per item and MIDI
/// modifier, advanced once per frame on the animation clock.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Smoothing {
    /// The followed value, or `None` before the first step, which lands on its target:
    /// an item that appears shows the controller where it is, rather than gliding from 0.
    level: Option<f32>,
}

impl Smoothing {
    /// Its value, `0..=1`; 0 before the first step.
    pub fn level(&self) -> f32 {
        self.level.unwrap_or(0.0)
    }

    /// Follows `target` (`0..=1`) for `dt_s` seconds with `midi`'s smoothing time: a
    /// one-pole smoothing whose time constant is that time, so it is about two thirds
    /// of the way there after it. A time of 0 jumps, and so does the first step. A later
    /// step that isn't a finite, positive time changes nothing. Returns the new value.
    /// Allocates nothing.
    pub fn advance(&mut self, midi: &Midi, target: f32, dt_s: f32) -> f32 {
        let target = if target.is_finite() {
            target.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let Some(level) = self.level else {
            self.level = Some(target);
            return target;
        };
        if !(dt_s.is_finite() && dt_s > 0.0) {
            return level;
        }
        let tau = midi.smoothing_ms / 1000.0;
        let k = if tau.is_finite() && tau > 0.0 {
            1.0 - (-dt_s / tau).exp()
        } else {
            1.0
        };
        let next = level + (target - level) * k;
        let next = if next.is_finite() {
            next.clamp(0.0, 1.0)
        } else {
            target
        };
        self.level = Some(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn a_controller_maps_onto_zero_to_one() {
        assert_eq!(Midi::normalize_cc(0), 0.0);
        assert_eq!(Midi::normalize_cc(127), 1.0);
        assert!(close(Midi::normalize_cc(64), 64.0 / 127.0));
        assert_eq!(Midi::normalize_cc(200), 1.0, "a value past 127 is its top");
        assert_eq!(Midi::normalize_bend(-8192), 0.0);
        assert_eq!(Midi::normalize_bend(0), 0.5);
        assert_eq!(Midi::normalize_bend(8191), 1.0);
    }

    #[test]
    fn the_value_is_bent_and_mapped_onto_its_range() {
        let lever = Midi {
            from: -25.0,
            to: 25.0,
            ..Midi::default()
        };
        assert_eq!(lever.value(0.0), -25.0);
        assert_eq!(lever.value(1.0), 25.0);
        assert_eq!(lever.value(0.5), 0.0);
        let inverted = Midi {
            from: 1.0,
            to: 0.0,
            curve: Curve::EaseIn,
            ..Midi::default()
        };
        assert_eq!(inverted.value(1.0), 0.0);
        assert!((inverted.value(0.5) - 0.875).abs() < 1e-6);
    }

    #[test]
    fn smoothing_lands_first_then_follows_and_zero_jumps() {
        let smooth = Midi {
            smoothing_ms: 100.0,
            ..Midi::default()
        };
        let mut s = Smoothing::default();
        assert_eq!(s.level(), 0.0);
        assert_eq!(s.advance(&smooth, 0.25, 0.0), 0.25, "the first step lands");
        // One time constant later it is about 63% of the way from 0.25 to 1.
        let after = s.advance(&smooth, 1.0, 0.1);
        assert!(close(after, 0.25 + 0.75 * (1.0 - (-1f32).exp())), "{after}");
        assert_eq!(s.advance(&smooth, 1.0, 0.0), after, "no time, no change");
        assert_eq!(s.advance(&smooth, 1.0, f32::NAN), after);
        // Down as up: the same time constant.
        let mut down = Smoothing::default();
        down.advance(&smooth, 1.0, 0.0);
        let fell = down.advance(&smooth, 0.0, 0.1);
        assert!(close(fell, (-1f32).exp()), "{fell}");

        let instant = Midi {
            smoothing_ms: 0.0,
            ..Midi::default()
        };
        let mut jump = Smoothing::default();
        jump.advance(&instant, 0.0, 0.0);
        assert_eq!(jump.advance(&instant, 1.0, 0.016), 1.0, "0 ms jumps");
        assert_eq!(jump.advance(&instant, f32::NAN, 0.016), 0.0, "NaN is 0");
    }

    #[test]
    fn out_of_range_parameters_are_named() {
        assert_eq!(Midi::default().problem(100_000.0), None);
        let slow = Midi {
            smoothing_ms: 10_001.0,
            ..Midi::default()
        };
        assert_eq!(slow.problem(100_000.0), Some("smoothing_ms"));
        let far = Midi {
            to: 1e9,
            ..Midi::default()
        };
        assert_eq!(far.problem(100_000.0), Some("to"));
        let nan = Midi {
            from: f64::NAN,
            ..Midi::default()
        };
        assert_eq!(nan.problem(100_000.0), Some("from"));
    }
}
