//! Audio-reactive modifiers ([ADR-0100](../../../docs/adr/0100-audio-reactive-motion-modifiers.md)):
//! how a measure of a bus or a source becomes a modifier's value. Each step is a pure
//! function, so a client previews what the engine draws:
//!
//! 1. [`Audio::normalize`] maps a measure onto `0..=1`: a level in dBFS between the
//!    floor and the ceiling, or an onset as it is.
//! 2. [`Envelope::advance`] follows that with the attack and release times. It is the
//!    one piece of state, kept per item and modifier by whoever evaluates.
//! 3. [`Audio::value`] bends the envelope with the [`Curve`] and maps it onto
//!    `from..to`.
//!
//! What is heard (a bus or a source) and how it is measured (the FFT for a band) stay
//! outside this crate: the engine measures, and hands in a number per frame.

/// What a binding measures of what it hears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Measure {
    /// The RMS level of the latest window.
    Level,
    /// The peak absolute sample of the latest window.
    Peak,
    /// A jump of the level above its recent average, already `0..=1`.
    Onset,
    /// The spectrum between two frequencies, in hertz, within `20..=20000` and with
    /// `low_hz < high_hz`.
    Band {
        /// Its lowest frequency.
        low_hz: f32,
        /// Its highest frequency.
        high_hz: f32,
    },
}

/// How the `0..=1` envelope is bent before it is mapped onto the range. These are
/// shapes of their own, never a transition's easing, so none overshoots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Curve {
    /// As it is.
    #[default]
    Linear,
    /// Cubic, slow at first: quiet passages stay low and only loud ones lift.
    EaseIn,
    /// Cubic, fast at first: quiet passages already show.
    EaseOut,
    /// Cubic at both ends.
    EaseInOut,
}

impl Curve {
    /// Every curve, in wire order.
    pub const ALL: [Curve; 4] = [
        Curve::Linear,
        Curve::EaseIn,
        Curve::EaseOut,
        Curve::EaseInOut,
    ];

    /// Its wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::EaseIn => "ease_in",
            Curve::EaseOut => "ease_out",
            Curve::EaseInOut => "ease_in_out",
        }
    }

    /// The curve with wire name `name`.
    pub fn parse(name: &str) -> Option<Curve> {
        Curve::ALL.into_iter().find(|c| c.as_str() == name)
    }

    /// The bent value of `t`, which is clamped to `0..=1` first.
    pub fn at(self, t: f32) -> f32 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        match self {
            Curve::Linear => t,
            Curve::EaseIn => t * t * t,
            Curve::EaseOut => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
            Curve::EaseInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let u = -2.0 * t + 2.0;
                    1.0 - u * u * u / 2.0
                }
            }
        }
    }
}

/// The parameters of an audio-reactive modifier ([`crate::Kind::Audio`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Audio {
    /// What it measures.
    pub measure: Measure,
    /// The level, in dBFS, that maps to 0. `-90 <= floor_db < ceiling_db`.
    pub floor_db: f32,
    /// The level, in dBFS, that maps to 1. `floor_db < ceiling_db <= 0`.
    pub ceiling_db: f32,
    /// How fast the envelope rises, in milliseconds, `0..=10000`.
    pub attack_ms: f32,
    /// How fast it falls, in milliseconds, `0..=10000`.
    pub release_ms: f32,
    /// How the envelope is bent.
    pub curve: Curve,
    /// The modifier's value at silence, in its property's unit.
    pub from: f64,
    /// Its value at the ceiling. It may be below `from`, to invert.
    pub to: f64,
}

/// The quietest floor, in dBFS.
pub const MIN_DB: f32 = -90.0;
/// The longest attack or release, in milliseconds.
pub const MAX_ENVELOPE_MS: f32 = 10_000.0;
/// The lowest band edge, in hertz.
pub const LOW_HZ: f32 = 20.0;
/// The highest band edge, in hertz.
pub const HIGH_HZ: f32 = 20_000.0;

impl Default for Audio {
    /// The program mix's level from -60 to 0 dBFS, a 10 ms attack and a 250 ms release,
    /// linear, onto `0..1`.
    fn default() -> Self {
        Self {
            measure: Measure::Level,
            floor_db: -60.0,
            ceiling_db: 0.0,
            attack_ms: 10.0,
            release_ms: 250.0,
            curve: Curve::Linear,
            from: 0.0,
            to: 1.0,
        }
    }
}

impl Audio {
    /// The first parameter out of range, by wire name, or `None`. `magnitude` is the
    /// largest `from` or `to` the caller allows.
    pub(crate) fn problem(&self, magnitude: f64) -> Option<&'static str> {
        let ms = |v: f32| v.is_finite() && (0.0..=MAX_ENVELOPE_MS).contains(&v);
        let end = |v: f64| v.is_finite() && v.abs() <= magnitude;
        let hz = |v: f32| v.is_finite() && (LOW_HZ..=HIGH_HZ).contains(&v);
        if let Measure::Band { low_hz, high_hz } = self.measure {
            if !hz(low_hz) {
                return Some("low_hz");
            }
            if !(hz(high_hz) && low_hz < high_hz) {
                return Some("high_hz");
            }
        }
        if !(self.floor_db.is_finite() && (MIN_DB..0.0).contains(&self.floor_db)) {
            Some("floor_db")
        } else if !(self.ceiling_db.is_finite()
            && self.ceiling_db <= 0.0
            && self.floor_db < self.ceiling_db)
        {
            Some("ceiling_db")
        } else if !ms(self.attack_ms) {
            Some("attack_ms")
        } else if !ms(self.release_ms) {
            Some("release_ms")
        } else if !end(self.from) {
            Some("from")
        } else if !end(self.to) {
            Some("to")
        } else {
            None
        }
    }

    /// A measure mapped onto `0..=1`. For [`Measure::Onset`], `measured` is already
    /// `0..=1` and is only clamped. For the others it is a linear full-scale amplitude
    /// (1 is 0 dBFS), mapped in dB from the floor to the ceiling: for a band, the square
    /// root of its bins' summed power, scaled so a full-scale sine in it is 1, never the
    /// power itself. Anything that isn't a
    /// finite, positive number is silence.
    pub fn normalize(&self, measured: f32) -> f32 {
        if !measured.is_finite() || measured <= 0.0 {
            return 0.0;
        }
        if self.measure == Measure::Onset {
            return measured.min(1.0);
        }
        let span = self.ceiling_db - self.floor_db;
        if span.is_nan() || span <= 0.0 {
            return 0.0;
        }
        let db = 20.0 * measured.log10();
        ((db - self.floor_db) / span).clamp(0.0, 1.0)
    }

    /// The modifier's value for an envelope of `envelope` (`0..=1`): bent by the curve,
    /// then mapped onto `from..to`. Silence (0) is `from`.
    pub fn value(&self, envelope: f32) -> f64 {
        let t = f64::from(self.curve.at(envelope));
        self.from + (self.to - self.from) * t
    }
}

/// An envelope follower's state: the smoothed `0..=1` level. One per item and audio
/// modifier, advanced once per frame on the animation clock.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Envelope {
    level: f32,
}

impl Envelope {
    /// Its level, `0..=1`.
    pub fn level(&self) -> f32 {
        self.level
    }

    /// Follows `measured` (a raw measure, as [`Audio::normalize`] takes it) for `dt_s`
    /// seconds: toward a higher level with the attack time, toward a lower one with the
    /// release time, each a one-pole smoothing whose time constant is that time. A time
    /// of 0 jumps. A step that isn't a finite, positive time changes nothing. Returns
    /// the new level. Allocates nothing.
    pub fn advance(&mut self, audio: &Audio, measured: f32, dt_s: f32) -> f32 {
        if !(dt_s.is_finite() && dt_s > 0.0) {
            return self.level;
        }
        let target = audio.normalize(measured);
        let ms = if target > self.level {
            audio.attack_ms
        } else {
            audio.release_ms
        };
        let tau = ms / 1000.0;
        let k = if tau.is_finite() && tau > 0.0 {
            1.0 - (-dt_s / tau).exp()
        } else {
            1.0
        };
        let next = self.level + (target - self.level) * k;
        self.level = if next.is_finite() {
            next.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn db(v: f32) -> f32 {
        10f32.powf(v / 20.0)
    }

    #[test]
    fn curve_names_round_trip_and_ends_hold() {
        for c in Curve::ALL {
            assert_eq!(Curve::parse(c.as_str()), Some(c));
            assert!(close(c.at(0.0), 0.0), "{c:?} at 0");
            assert!(close(c.at(1.0), 1.0), "{c:?} at 1");
            assert!(close(c.at(-3.0), 0.0) && close(c.at(4.0), 1.0));
            assert!(close(c.at(f32::NAN), 0.0));
            // Monotone and within 0..=1: no overshoot.
            let mut last = 0.0;
            for i in 0..=100 {
                let v = c.at(i as f32 / 100.0);
                assert!((0.0..=1.0).contains(&v) && v >= last - 1e-6, "{c:?} at {i}");
                last = v;
            }
        }
        assert!(Curve::EaseIn.at(0.5) < 0.5 && Curve::EaseOut.at(0.5) > 0.5);
        assert!(close(Curve::EaseInOut.at(0.5), 0.5));
        assert_eq!(Curve::parse("spring"), None);
    }

    #[test]
    fn normalize_maps_db_from_floor_to_ceiling() {
        let a = Audio::default();
        assert!(close(a.normalize(1.0), 1.0));
        assert!(close(a.normalize(db(-30.0)), 0.5));
        assert!(close(a.normalize(db(-60.0)), 0.0));
        assert!(close(a.normalize(db(-80.0)), 0.0));
        assert!(close(a.normalize(2.0), 1.0), "over full scale clamps");
        for silent in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(a.normalize(silent), 0.0);
        }
        let narrow = Audio {
            floor_db: -24.0,
            ceiling_db: -6.0,
            ..a
        };
        assert!(close(narrow.normalize(db(-15.0)), 0.5));
    }

    #[test]
    fn an_onset_passes_through() {
        let a = Audio {
            measure: Measure::Onset,
            ..Audio::default()
        };
        assert!(close(a.normalize(0.25), 0.25));
        assert!(close(a.normalize(3.0), 1.0));
    }

    #[test]
    fn value_maps_onto_the_range_and_inverts() {
        let a = Audio {
            from: -0.6,
            to: 0.0,
            ..Audio::default()
        };
        assert_eq!(a.value(0.0), -0.6);
        assert!((a.value(1.0)).abs() < 1e-9);
        assert!((a.value(0.5) + 0.3).abs() < 1e-6);
        let inverted = Audio {
            from: 6.0,
            to: 0.0,
            ..Audio::default()
        };
        assert!(inverted.value(1.0).abs() < 1e-9 && inverted.value(0.0) == 6.0);
        let eased = Audio {
            curve: Curve::EaseIn,
            ..Audio::default()
        };
        assert!((eased.value(0.5) - 0.125).abs() < 1e-6);
    }

    #[test]
    fn the_envelope_attacks_and_releases_with_its_times() {
        let a = Audio {
            attack_ms: 10.0,
            release_ms: 250.0,
            ..Audio::default()
        };
        let mut e = Envelope::default();
        // One time constant of attack reaches 1 - 1/e of the way.
        let up = e.advance(&a, 1.0, 0.010);
        assert!(close(up, 1.0 - (-1.0f32).exp()), "{up}");
        for _ in 0..50 {
            e.advance(&a, 1.0, 1.0 / 60.0);
        }
        assert!(close(e.level(), 1.0));
        // One time constant of release falls to 1/e.
        let down = e.advance(&a, 0.0, 0.250);
        assert!(close(down, (-1.0f32).exp()), "{down}");
        // Release is slower than attack: one frame up gains more than one frame down loses.
        let mut rise = Envelope::default();
        let gained = rise.advance(&a, 1.0, 1.0 / 60.0);
        let mut fall = Envelope { level: 1.0 };
        let lost = 1.0 - fall.advance(&a, 0.0, 1.0 / 60.0);
        assert!(gained > lost);
    }

    #[test]
    fn zero_times_jump_and_bad_steps_hold() {
        let a = Audio {
            attack_ms: 0.0,
            release_ms: 0.0,
            ..Audio::default()
        };
        let mut e = Envelope::default();
        assert!(close(e.advance(&a, db(-30.0), 1.0 / 60.0), 0.5));
        assert!(close(e.advance(&a, 0.0, 1.0 / 60.0), 0.0));
        e.advance(&a, 1.0, 1.0);
        for dt in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(e.advance(&a, 0.0, dt), 1.0, "dt {dt}");
        }
    }

    #[test]
    fn the_envelope_is_frame_rate_independent() {
        let a = Audio::default();
        let mut at_30 = Envelope::default();
        let mut at_120 = Envelope::default();
        for _ in 0..3 {
            at_30.advance(&a, 1.0, 1.0 / 30.0);
        }
        for _ in 0..12 {
            at_120.advance(&a, 1.0, 1.0 / 120.0);
        }
        assert!(close(at_30.level(), at_120.level()));
    }

    #[test]
    fn problem_names_each_bad_parameter() {
        let ok = Audio::default();
        assert_eq!(ok.problem(100.0), None);
        let cases: [(Audio, &str); 10] = [
            (
                Audio {
                    floor_db: -91.0,
                    ..ok
                },
                "floor_db",
            ),
            (
                Audio {
                    floor_db: 0.0,
                    ceiling_db: 0.0,
                    ..ok
                },
                "floor_db",
            ),
            (
                Audio {
                    floor_db: -20.0,
                    ceiling_db: -30.0,
                    ..ok
                },
                "ceiling_db",
            ),
            (
                Audio {
                    ceiling_db: 1.0,
                    ..ok
                },
                "ceiling_db",
            ),
            (
                Audio {
                    attack_ms: -1.0,
                    ..ok
                },
                "attack_ms",
            ),
            (
                Audio {
                    release_ms: 10_001.0,
                    ..ok
                },
                "release_ms",
            ),
            (
                Audio {
                    from: f64::NAN,
                    ..ok
                },
                "from",
            ),
            (Audio { to: 101.0, ..ok }, "to"),
            (
                Audio {
                    measure: Measure::Band {
                        low_hz: 10.0,
                        high_hz: 200.0,
                    },
                    ..ok
                },
                "low_hz",
            ),
            (
                Audio {
                    measure: Measure::Band {
                        low_hz: 200.0,
                        high_hz: 200.0,
                    },
                    ..ok
                },
                "high_hz",
            ),
        ];
        for (audio, param) in cases {
            assert_eq!(audio.problem(100.0), Some(param), "{audio:?}");
        }
        let band = Audio {
            measure: Measure::Band {
                low_hz: 40.0,
                high_hz: 120.0,
            },
            ..ok
        };
        assert_eq!(band.problem(100.0), None);
    }
}
