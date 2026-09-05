use crate::ids::ItemId;
use crate::item::Protection;
use crate::score::FeatureMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyId {
    pub name: String,
    pub version: String,
}

impl PolicyId {
    pub fn new(name: &str, version: &str) -> Self {
        Self {
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }
}

impl std::fmt::Display for PolicyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// Machine-queryable. These strings are a wire format stored in audit rows;
/// renaming a variant is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    NovelContent,
    HighValue,
    HighRedundancy,
    ExactDuplicate,
    CapacityPressure,
    ProtectedFragile,
    SensitivityCap,
    SensitivityConflict,
    TtlExpired,
    Pinned,
    LowValue,
    ReplayDue,
    DiversityCut,
    BudgetExhausted,
    PolicyInvalid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reason {
    pub code: ReasonCode,
    pub detail: String,
    pub evidence: FeatureMap,
}

impl Reason {
    pub fn new(code: ReasonCode, detail: &str, evidence: FeatureMap) -> Self {
        Self {
            code,
            detail: detail.to_owned(),
            evidence,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Eviction {
    pub item: ItemId,
    pub reason: Reason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Append the new body to the target, union tags and attrs, keep the
    /// earliest `occurred_at` and the latest `created_at`.
    AppendAndUnion,
    /// Replace the target's body with the new one, union tags and attrs.
    ReplaceBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Action {
    Retain {
        protection: Protection,
    },
    Merge {
        into: ItemId,
        strategy: MergeStrategy,
    },
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    pub evictions: Vec<Eviction>,
    pub reasons: Vec<Reason>,
    pub policy: PolicyId,
}

impl Decision {
    pub fn retain(policy: PolicyId, reason: Reason) -> Self {
        Self {
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![],
            reasons: vec![reason],
            policy,
        }
    }

    pub fn reject(policy: PolicyId, reason: Reason) -> Self {
        Self {
            action: Action::Reject,
            evictions: vec![],
            reasons: vec![reason],
            policy,
        }
    }

    pub fn has_reason(&self, code: ReasonCode) -> bool {
        self.reasons.iter().any(|r| r.code == code)
            || self.evictions.iter().any(|e| e.reason.code == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features;

    #[test]
    fn reason_codes_serialize_as_stable_snake_case_strings() {
        // Audit rows are queried by these strings; they are a wire format.
        let json = serde_json::to_string(&ReasonCode::HighRedundancy).unwrap();
        assert_eq!(json, "\"high_redundancy\"");
        let back: ReasonCode = serde_json::from_str("\"capacity_pressure\"").unwrap();
        assert_eq!(back, ReasonCode::CapacityPressure);
    }

    #[test]
    fn retain_constructor_produces_a_normal_protection_decision() {
        let d = Decision::retain(
            PolicyId::new("baseline", "0.1.0"),
            Reason::new(ReasonCode::NovelContent, "no near duplicates", features! {}),
        );
        assert!(matches!(
            d.action,
            Action::Retain {
                protection: Protection::Normal
            }
        ));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
    }

    #[test]
    fn reject_constructor_carries_its_reason() {
        let d = Decision::reject(
            PolicyId::new("baseline", "0.1.0"),
            Reason::new(
                ReasonCode::ExactDuplicate,
                "cosine 0.99",
                features! { "sim" => 0.99 },
            ),
        );
        assert!(matches!(d.action, Action::Reject));
        assert!(d.has_reason(ReasonCode::ExactDuplicate));
        assert_eq!(d.reasons[0].evidence.get("sim"), Some(&0.99));
    }

    #[test]
    fn evictions_name_both_the_item_and_the_reason() {
        let e = Eviction {
            item: ItemId::new(),
            reason: Reason::new(
                ReasonCode::LowValue,
                "value 0.02",
                features! { "value" => 0.02 },
            ),
        };
        assert_eq!(e.reason.code, ReasonCode::LowValue);
    }
}
