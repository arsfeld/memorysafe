use crate::ids::ItemId;
use crate::score::{FeatureMap, Score};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevel {
    Public,
    Internal,
    Personal,
    Sensitive,
    Restricted,
}

impl SensitivityLevel {
    pub const ALL: [SensitivityLevel; 5] = [
        SensitivityLevel::Public,
        SensitivityLevel::Internal,
        SensitivityLevel::Personal,
        SensitivityLevel::Sensitive,
        SensitivityLevel::Restricted,
    ];

    /// Stored as an INTEGER so SQL can express `sensitivity <= ceiling`.
    pub fn ordinal(self) -> i64 {
        match self {
            SensitivityLevel::Public => 0,
            SensitivityLevel::Internal => 1,
            SensitivityLevel::Personal => 2,
            SensitivityLevel::Sensitive => 3,
            SensitivityLevel::Restricted => 4,
        }
    }

    pub fn from_ordinal(value: i64) -> Option<Self> {
        Self::ALL.get(usize::try_from(value).ok()?).copied()
    }

    /// A caller's `sensitivity_hint` may raise the detected level, never lower it.
    pub fn raised_by(self, hint: Option<SensitivityLevel>) -> Self {
        match hint {
            Some(h) => self.max(h),
            None => self,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityCategory {
    Pii,
    Health,
    Financial,
    Credential,
    Legal,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensitivityAssessment {
    pub level: SensitivityLevel,
    pub categories: Vec<SensitivityCategory>,
    pub confidence: Score,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedundancyAssessment {
    pub score: Score,
    /// Descending by similarity. `Score` rather than a raw `f32`: cosine spans
    /// [-1, 1], but this list is filtered at `near_duplicate_floor` before it
    /// is built, so a negative similarity — meaning "definitely not a
    /// duplicate" — never belongs here.
    pub near_duplicates: Vec<(ItemId, Score)>,
}

impl RedundancyAssessment {
    pub fn best(&self) -> Option<(&ItemId, Score)> {
        self.near_duplicates.first().map(|(id, s)| (id, *s))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssessorId {
    pub name: String,
    pub version: String,
}

impl AssessorId {
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }
}

impl std::fmt::Display for AssessorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub value: Score,
    pub fragility: Score,
    pub sensitivity: SensitivityAssessment,
    pub redundancy: RedundancyAssessment,
    pub features: FeatureMap,
    pub assessor: AssessorId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;

    #[test]
    fn sensitivity_levels_are_ordered_least_to_most_restrictive() {
        assert!(SensitivityLevel::Public < SensitivityLevel::Internal);
        assert!(SensitivityLevel::Internal < SensitivityLevel::Personal);
        assert!(SensitivityLevel::Personal < SensitivityLevel::Sensitive);
        assert!(SensitivityLevel::Sensitive < SensitivityLevel::Restricted);
    }

    #[test]
    fn sensitivity_ordinals_round_trip_for_sql_storage() {
        for level in SensitivityLevel::ALL {
            assert_eq!(SensitivityLevel::from_ordinal(level.ordinal()), Some(level));
        }
        assert_eq!(SensitivityLevel::from_ordinal(99), None);
    }

    #[test]
    fn a_hint_may_only_raise_the_level() {
        let resolved = SensitivityLevel::Internal.raised_by(Some(SensitivityLevel::Restricted));
        assert_eq!(resolved, SensitivityLevel::Restricted);
        let ignored = SensitivityLevel::Sensitive.raised_by(Some(SensitivityLevel::Public));
        assert_eq!(ignored, SensitivityLevel::Sensitive);
        assert_eq!(
            SensitivityLevel::Personal.raised_by(None),
            SensitivityLevel::Personal
        );
    }

    #[test]
    fn assessment_carries_its_evidence() {
        let a = Assessment {
            value: Score::clamped(0.7),
            fragility: Score::clamped(0.3),
            sensitivity: SensitivityAssessment {
                level: SensitivityLevel::Personal,
                categories: vec![SensitivityCategory::Pii],
                confidence: Score::clamped(0.8),
            },
            redundancy: RedundancyAssessment {
                score: Score::clamped(0.1),
                near_duplicates: vec![],
            },
            features: features! { "neighbour_count" => 3.0 },
            assessor: AssessorId::new("baseline", "0.1.0"),
        };
        assert_eq!(a.features.get("neighbour_count"), Some(&3.0));
        assert_eq!(a.assessor.to_string(), "baseline@0.1.0");
    }

    #[test]
    fn ordinal_order_agrees_with_derived_ord() {
        // Two independent representations of the same ordering: `Ord` from
        // declaration order, `ordinal()` from a hand-written match. SQL filters
        // use the ordinal; `raised_by` uses `Ord`. They must never drift.
        for a in SensitivityLevel::ALL {
            for b in SensitivityLevel::ALL {
                assert_eq!(
                    a.cmp(&b),
                    a.ordinal().cmp(&b.ordinal()),
                    "Ord and ordinal() disagree for {a:?} vs {b:?}"
                );
            }
        }
    }

    #[test]
    fn raised_by_never_lowers_for_any_pair() {
        for base in SensitivityLevel::ALL {
            for hint in SensitivityLevel::ALL {
                let out = base.raised_by(Some(hint));
                assert!(out >= base, "{base:?} + hint {hint:?} lowered to {out:?}");
                assert_eq!(out, base.max(hint));
            }
            assert_eq!(base.raised_by(Some(base)), base);
            assert_eq!(base.raised_by(None), base);
        }
    }

    #[test]
    fn from_ordinal_rejects_hostile_database_values_without_panicking() {
        for bad in [-1i64, i64::MIN, 5, 99, i64::MAX] {
            assert_eq!(SensitivityLevel::from_ordinal(bad), None, "accepted {bad}");
        }
    }

    #[test]
    fn serde_forms_are_a_stored_wire_format() {
        // Audit rows store these as JSON strings and query them as strings.
        // Renaming a variant is a data-compatibility break, not a refactor.
        assert_eq!(
            serde_json::to_string(&SensitivityLevel::Public).unwrap(),
            "\"public\""
        );
        assert_eq!(
            serde_json::to_string(&SensitivityLevel::Restricted).unwrap(),
            "\"restricted\""
        );
        assert_eq!(
            serde_json::to_string(&SensitivityCategory::Pii).unwrap(),
            "\"pii\""
        );
        assert_eq!(
            serde_json::to_string(&SensitivityCategory::Credential).unwrap(),
            "\"credential\""
        );
        let back: SensitivityLevel = serde_json::from_str("\"sensitive\"").unwrap();
        assert_eq!(back, SensitivityLevel::Sensitive);
    }

    #[test]
    fn best_returns_the_head_of_the_descending_list() {
        let (a, b) = (ItemId::new(), ItemId::new());
        let r = RedundancyAssessment {
            score: Score::clamped(0.9),
            near_duplicates: vec![(a.clone(), Score::clamped(0.9)), (b, Score::clamped(0.4))],
        };
        let (id, sim) = r.best().unwrap();
        assert_eq!(*id, a);
        assert_eq!(sim.get(), 0.9);

        let empty = RedundancyAssessment {
            score: Score::ZERO,
            near_duplicates: vec![],
        };
        assert!(empty.best().is_none());
    }
}
