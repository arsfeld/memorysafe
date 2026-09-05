use crate::error::CoreError;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// Evidence numbers attached to a reason or an assessment. Ordered so that
/// audit rows serialize deterministically.
pub type FeatureMap = BTreeMap<String, f64>;

#[macro_export]
macro_rules! features {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = $crate::score::FeatureMap::new();
        $( m.insert($k.to_string(), $v as f64); )*
        m
    }};
}

/// A value in `[0.0, 1.0]`. Never a bare `f32` anywhere in the API.
///
/// `Deserialize` goes through `TryFrom<f32>`, not a transparent passthrough.
/// A derived transparent `Deserialize` would delegate to `f32`'s own impl and
/// bypass both constructors, so a stored `2.5` or a NaN from a binary format
/// would enter the type unchecked — and the hand-written `Ord` below, plus the
/// `Eq` marker, are sound only while that cannot happen. Invalid stored data
/// fails loudly rather than being silently corrected.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Score(f32);

impl Score {
    pub const ZERO: Score = Score(0.0);
    pub const ONE: Score = Score(1.0);

    pub fn new(value: f32) -> Result<Self, CoreError> {
        if value.is_nan() || !(0.0..=1.0).contains(&value) {
            return Err(CoreError::OutOfRange {
                field: "score",
                value,
            });
        }
        Ok(Self(value))
    }

    /// Total function for internal arithmetic. NaN maps to zero.
    pub fn clamped(value: f32) -> Self {
        if value.is_nan() {
            return Self(0.0);
        }
        Self(value.clamp(0.0, 1.0))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

impl Eq for Score {}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Safe: the constructors exclude NaN.
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<f32> for Score {
    type Error = CoreError;
    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Score::new(value)
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f32::deserialize(deserializer)?;
        Score::try_from(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_rejects_out_of_range_and_nan() {
        assert!(Score::new(-0.1).is_err());
        assert!(Score::new(1.1).is_err());
        assert!(Score::new(f32::NAN).is_err());
        assert!(Score::new(0.0).is_ok());
        assert!(Score::new(1.0).is_ok());
    }

    #[test]
    fn clamped_never_fails_and_maps_nan_to_zero() {
        assert_eq!(Score::clamped(-5.0).get(), 0.0);
        assert_eq!(Score::clamped(5.0).get(), 1.0);
        assert_eq!(Score::clamped(f32::NAN).get(), 0.0);
    }

    #[test]
    fn scores_have_a_total_order() {
        let mut v = [
            Score::clamped(0.5),
            Score::clamped(0.1),
            Score::clamped(0.9),
        ];
        v.sort();
        assert_eq!(v[0].get(), 0.1);
        assert_eq!(v[2].get(), 0.9);
    }

    #[test]
    fn features_macro_builds_a_map() {
        let f = features! { "redundancy" => 0.93, "neighbours" => 4.0 };
        assert_eq!(f.get("redundancy"), Some(&0.93));
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn deserialization_cannot_bypass_the_range_invariant() {
        // `Ord` and `Eq` on this type are sound only because no `Score` can
        // hold NaN or a value outside [0,1]. Deserialization is the one path
        // that does not go through a constructor, so it is validated too.
        assert_eq!(serde_json::from_str::<Score>("0.5").unwrap().get(), 0.5);
        assert!(serde_json::from_str::<Score>("2.5").is_err());
        assert!(serde_json::from_str::<Score>("-0.1").is_err());
        // Round-trip of a valid score is unaffected.
        let s = Score::clamped(0.25);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Score>(&json).unwrap(), s);
    }
}
