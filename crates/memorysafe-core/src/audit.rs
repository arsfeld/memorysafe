use crate::assessment::Assessment;
use crate::decision::Decision;
use crate::ids::{AuditId, ItemId, Scope};
use crate::item::MemoryItem;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEvent {
    Admitted,
    Rejected,
    Merged,
    Forgotten,
    Recalled,
    Exported,
    Imported,
    SubjectPurged,
    Reembedded,
    PolicyChanged,
    MaintenanceRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Agent,
    Human,
    ApiKey,
    Cli,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    pub id: Option<String>,
}

impl Actor {
    pub fn system() -> Self {
        Self {
            kind: ActorKind::System,
            id: None,
        }
    }
}

/// An item's identity in an audit row. Deliberately has no body field — the
/// type system is what enforces "audit never stores bodies".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    pub id: ItemId,
    pub digest: String,
}

impl ItemRef {
    pub fn from_item(item: &MemoryItem) -> Self {
        Self {
            id: item.id.clone(),
            digest: item.digest(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub id: AuditId,
    #[serde(with = "time::serde::timestamp")]
    pub at: OffsetDateTime,
    pub scope: Scope,
    pub event: AuditEvent,
    pub items: Vec<ItemRef>,
    pub assessment: Option<Assessment>,
    pub decision: Option<Decision>,
    pub actor: Actor,
}

impl AuditRecord {
    pub fn new(scope: Scope, event: AuditEvent, items: Vec<ItemRef>, actor: Actor) -> Self {
        Self {
            id: AuditId::new(),
            at: OffsetDateTime::now_utc(),
            scope,
            event,
            items,
            assessment: None,
            decision: None,
            actor,
        }
    }

    pub fn with_assessment(mut self, a: Assessment) -> Self {
        self.assessment = Some(a);
        self
    }

    pub fn with_decision(mut self, d: Decision) -> Self {
        self.decision = Some(d);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditFilter {
    /// Empty means "all events".
    pub events: Vec<AuditEvent>,
    pub item: Option<ItemId>,
    #[serde(with = "time::serde::timestamp::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub until: Option<OffsetDateTime>,
    pub limit: usize,
}

impl Default for AuditFilter {
    fn default() -> Self {
        Self {
            events: vec![],
            item: None,
            since: None,
            until: None,
            limit: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assessment::SensitivityLevel;
    use crate::item::{Protection, Source, SourceKind};
    use crate::{ItemId, MemoryItem, Scope};
    use time::OffsetDateTime;

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: body.to_string(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: None,
            },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: Default::default(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    #[test]
    fn item_ref_carries_a_digest_and_never_the_body() {
        let i = item("a secret diagnosis");
        let r = ItemRef::from_item(&i);
        assert_eq!(r.id, i.id);
        assert_eq!(r.digest, i.digest());
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("secret"),
            "audit must not carry bodies: {json}"
        );
    }

    #[test]
    fn audit_record_serializes_without_any_body_text() {
        let i = item("patient has a rare allergy");
        let rec = AuditRecord::new(
            i.scope.clone(),
            AuditEvent::Admitted,
            vec![ItemRef::from_item(&i)],
            Actor {
                kind: ActorKind::Agent,
                id: Some("agent-7".into()),
            },
        );
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("allergy"),
            "audit must not carry bodies: {json}"
        );
        assert!(json.contains("admitted"));
    }

    #[test]
    fn audit_events_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&AuditEvent::SubjectPurged).unwrap(),
            "\"subject_purged\""
        );
        assert_eq!(
            serde_json::to_string(&AuditEvent::Recalled).unwrap(),
            "\"recalled\""
        );
    }

    #[test]
    fn default_audit_filter_matches_everything() {
        let f = AuditFilter::default();
        assert!(f.events.is_empty());
        assert_eq!(f.limit, 100);
        assert!(f.since.is_none());
    }

    #[test]
    fn audit_record_golden_json_with_assessment_and_decision() {
        use crate::assessment::{
            AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityCategory,
        };
        use crate::decision::{PolicyId, Reason, ReasonCode};
        use crate::features;

        let rec = AuditRecord {
            id: AuditId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            at: OffsetDateTime::from_unix_timestamp(1234567890).unwrap(),
            scope: Scope::new("tenant1", "subject1", "namespace1").unwrap(),
            event: AuditEvent::Admitted,
            items: vec![ItemRef {
                id: ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVRZ").unwrap(),
                digest: "abc123def456".to_string(),
            }],
            assessment: Some(Assessment {
                value: crate::Score::clamped(0.85),
                fragility: crate::Score::clamped(0.12),
                sensitivity: SensitivityAssessment {
                    level: SensitivityLevel::Personal,
                    categories: vec![SensitivityCategory::Pii],
                    confidence: crate::Score::clamped(0.95),
                },
                redundancy: RedundancyAssessment {
                    score: crate::Score::clamped(0.05),
                    near_duplicates: vec![],
                },
                features: features! { "test_feature" => 1.5 },
                assessor: AssessorId::new("test_assessor", "1.0.0"),
            }),
            decision: Some(Decision {
                action: crate::decision::Action::Retain {
                    protection: crate::item::Protection::Normal,
                },
                evictions: vec![],
                reasons: vec![Reason::new(
                    ReasonCode::NovelContent,
                    "novel content detected",
                    features! { "similarity" => 0.1 },
                )],
                policy: PolicyId::new("default", "1.0.0"),
            }),
            actor: Actor {
                kind: ActorKind::Agent,
                id: Some("agent-1".into()),
            },
        };

        let json = serde_json::to_string(&rec).unwrap();
        // Print for inspection (will be captured if test fails)
        eprintln!("Generated JSON: {}", json);

        // Verify it round-trips
        let back: AuditRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec, "audit record did not survive round trip");

        // Verify the literal bytes match the expected format
        let expected = concat!(
            r#"{"id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","at":1234567890,"#,
            r#""scope":{"tenant":"tenant1","subject":"subject1","namespace":"namespace1"},"#,
            r#""event":"admitted","items":[{"id":"01BX5ZZKBKACTAV9WEVGEMMVRZ","digest":"abc123def456"}],"#,
            r#""assessment":{"value":0.85,"fragility":0.12,"sensitivity":{"level":"personal","#,
            r#""categories":["pii"],"confidence":0.95},"redundancy":{"score":0.05,"near_duplicates":[]},"#,
            r#""features":{"test_feature":1.5},"assessor":{"name":"test_assessor","version":"1.0.0"}},"#,
            r#""decision":{"action":{"kind":"retain","protection":{"kind":"normal"}},"evictions":[],"#,
            r#""reasons":[{"code":"novel_content","detail":"novel content detected","evidence":{"similarity":0.1}}],"#,
            r#""policy":{"name":"default","version":"1.0.0"}},"#,
            r#""actor":{"kind":"agent","id":"agent-1"}}"#
        );
        assert_eq!(json, expected, "audit record wire format changed");
    }
}
