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
    /// Append the new body to the target and union tags and attrs.
    ///
    /// Keeps the EARLIEST `occurred_at` and `created_at`, and the SHORTEST
    /// remaining TTL. Keeping the latest `created_at` would push an item's
    /// expiry forward on every merge, so an item under a retention limit would
    /// never expire as long as anything merged into it. `sensitivity` takes the
    /// maximum of the two — a merge must never downgrade a classification.
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
    /// The item this decision is about. `None` means the admission candidate,
    /// which has no id yet. `maintain` MUST set it: a maintenance decision with
    /// no subject names nobody, so `Action::Retain { protection }` returned from
    /// `maintain` — the way an expired protection window is released — would be
    /// counted and never applied.
    pub subject: Option<ItemId>,
    pub action: Action,
    pub evictions: Vec<Eviction>,
    pub reasons: Vec<Reason>,
    pub policy: PolicyId,
}

impl Decision {
    pub fn retain(policy: PolicyId, reason: Reason) -> Self {
        Self {
            subject: None,
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
            subject: None,
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
            Reason::new(
                ReasonCode::NovelContent,
                "no near duplicates",
                features! { "best_similarity" => 0.12 },
            ),
        );
        assert!(matches!(
            d.action,
            Action::Retain {
                protection: Protection::Normal
            }
        ));
        assert!(d.evictions.is_empty());
        assert!(d.has_reason(ReasonCode::NovelContent));
        // `has_reason` compares only `.code`, so assert the rest of the Reason
        // survived too — a constructor that rebuilt it from just the code would
        // otherwise pass.
        assert_eq!(d.reasons[0].detail, "no near duplicates");
        assert_eq!(d.reasons[0].evidence.get("best_similarity"), Some(&0.12));
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

    #[test]
    fn has_reason_searches_evictions_as_well_as_reasons() {
        // `has_reason` searches BOTH vectors. Every other test here builds a
        // decision whose match is in `reasons`, and `||` short-circuits, so the
        // evictions branch is never evaluated — deleting it passes all of them.
        let d = Decision {
            subject: None,
            action: Action::Retain {
                protection: Protection::Normal,
            },
            evictions: vec![Eviction {
                item: ItemId::new(),
                reason: Reason::new(ReasonCode::CapacityPressure, "made room", features! {}),
            }],
            reasons: vec![Reason::new(ReasonCode::NovelContent, "novel", features! {})],
            policy: PolicyId::new("baseline", "0.1.0"),
        };
        assert!(
            d.has_reason(ReasonCode::NovelContent),
            "missed the reasons vector"
        );
        assert!(
            d.has_reason(ReasonCode::CapacityPressure),
            "missed the evictions vector"
        );
        assert!(!d.has_reason(ReasonCode::TtlExpired));
    }

    #[test]
    fn action_is_internally_tagged_on_the_wire() {
        // Audit rows store this shape. Dropping `tag = "kind"` would silently
        // switch to serde's externally-tagged form and orphan every stored row,
        // and no Rust-level `matches!` assertion would notice.
        assert_eq!(
            serde_json::to_string(&Action::Retain {
                protection: Protection::Normal
            })
            .unwrap(),
            r#"{"kind":"retain","protection":{"kind":"normal"}}"#
        );
        assert_eq!(
            serde_json::to_string(&Action::Reject).unwrap(),
            r#"{"kind":"reject"}"#
        );
        let merge = Action::Merge {
            into: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            strategy: MergeStrategy::AppendAndUnion,
        };
        assert_eq!(
            serde_json::to_string(&merge).unwrap(),
            r#"{"kind":"merge","into":"01ARZ3NDEKTSV4RRFFQ69G5FAV","strategy":"append_and_union"}"#
        );
        let back: Action = serde_json::from_str(r#"{"kind":"reject"}"#).unwrap();
        assert!(matches!(back, Action::Reject));
    }

    #[test]
    fn a_full_decision_round_trips_through_json() {
        // The audit table stores this whole struct in its `decision` column and
        // reads it back with serde_json. Nothing else here exercises Merge,
        // ReplaceBody, a populated evictions vector, or PolicyId on the wire.
        let d = Decision {
            subject: Some(ItemId::parse("01M1SS3PMM9JP6G7R65XMKET6V").unwrap()),
            action: Action::Merge {
                into: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
                strategy: MergeStrategy::ReplaceBody,
            },
            evictions: vec![Eviction {
                item: ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVRZ").unwrap(),
                reason: Reason::new(
                    ReasonCode::CapacityPressure,
                    "evicted to make room",
                    features! { "value" => 0.11, "eviction_cost" => 0.02 },
                ),
            }],
            reasons: vec![Reason::new(
                ReasonCode::HighRedundancy,
                "folded into a close neighbour",
                features! { "similarity" => 0.95 },
            )],
            policy: PolicyId::new("baseline", "0.1.0"),
        };

        let json = serde_json::to_string(&d).unwrap();
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d, "a stored decision did not survive the round trip");

        // A round trip alone proves only self-consistency: serialise and
        // deserialise share the same code, so ANY symmetric rename round-trips
        // perfectly while orphaning every row already on disk. Pin the literal
        // bytes, which is the property stored data actually depends on.
        let expected = concat!(
            r#"{"subject":"01M1SS3PMM9JP6G7R65XMKET6V","#,
            r#""action":{"kind":"merge","into":"01ARZ3NDEKTSV4RRFFQ69G5FAV","#,
            r#""strategy":"replace_body"},"#,
            r#""evictions":[{"item":"01BX5ZZKBKACTAV9WEVGEMMVRZ","#,
            r#""reason":{"code":"capacity_pressure","detail":"evicted to make room","#,
            r#""evidence":{"eviction_cost":0.02,"value":0.11}}}],"#,
            r#""reasons":[{"code":"high_redundancy","#,
            r#""detail":"folded into a close neighbour","#,
            r#""evidence":{"similarity":0.95}}],"#,
            r#""policy":{"name":"baseline","version":"0.1.0"}}"#
        );
        assert_eq!(json, expected, "the stored decision format changed");

        // And prove old bytes still parse — the actual compatibility question.
        let from_disk: Decision = serde_json::from_str(expected).unwrap();
        assert_eq!(from_disk, d);
    }
}
