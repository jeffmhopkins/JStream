//! `Finite32`: an `f32` that is never NaN or infinite.
//!
//! serde narrows a JSON number to `f32` with `as`, so a finite JSON number outside the
//! `f32` range (`1e39`) would arrive as infinity, and a non-finite `f32` serializes as
//! `null`, which no longer parses. Every float in the protocol is therefore a `Finite32`:
//! it can only be built from a finite value, and deserializing a non-finite or
//! out-of-range number fails, so the whole message is rejected as `malformed`.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A finite `f32`. On the wire it is a plain JSON number, which must narrow to a finite
/// `f32` (magnitude up to about `3.4028235e38`); anything else fails to deserialize.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, JsonSchema)]
#[schemars(transparent)]
pub struct Finite32(f32);

impl Finite32 {
    /// Zero.
    pub const ZERO: Finite32 = Finite32(0.0);
    /// One.
    pub const ONE: Finite32 = Finite32(1.0);

    /// Wraps `value`, or returns `None` if it is NaN or infinite.
    pub const fn new(value: f32) -> Option<Finite32> {
        if value.is_finite() {
            Some(Finite32(value))
        } else {
            None
        }
    }

    /// The value.
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for Finite32 {
    fn default() -> Self {
        Finite32::ZERO
    }
}

impl From<Finite32> for f32 {
    fn from(value: Finite32) -> f32 {
        value.0
    }
}

impl fmt::Display for Finite32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for Finite32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f32(self.0)
    }
}

impl<'de> Deserialize<'de> for Finite32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f32::deserialize(deserializer)?;
        Finite32::new(value).ok_or_else(|| {
            serde::de::Error::custom(format_args!(
                "number is not a finite f32 (out of range or non-finite): {value}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Finite32;

    #[test]
    fn constructor_rejects_non_finite() {
        assert_eq!(Finite32::new(1.5).map(Finite32::get), Some(1.5));
        assert_eq!(Finite32::new(f32::NAN), None);
        assert_eq!(Finite32::new(f32::INFINITY), None);
        assert_eq!(Finite32::new(f32::NEG_INFINITY), None);
    }

    #[test]
    fn out_of_range_json_numbers_are_rejected() {
        // Regression (review round 2, B2): `1e39` used to narrow to infinity.
        for text in ["1e39", "-1e39", "1e400"] {
            assert!(
                serde_json::from_str::<Finite32>(text).is_err(),
                "{text} was accepted"
            );
        }
        assert!(serde_json::from_str::<Finite32>("null").is_err());
    }

    #[test]
    fn finite_values_round_trip_including_f32_max() {
        for value in [0.0, -0.5, 1280.0, f32::MAX, f32::MIN, f32::MIN_POSITIVE] {
            let finite = Finite32::new(value).expect("finite");
            let json = serde_json::to_string(&finite).expect("serializes");
            assert_ne!(json, "null");
            let back: Finite32 = serde_json::from_str(&json).expect("parses back");
            assert_eq!(back, finite, "{json}");
        }
    }
}
