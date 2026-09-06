use crate::assessment::Assessment;
use crate::decision::Decision;
use crate::ids::{AuditId, ItemId, Namespace, Scope, SubjectId};
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

impl AuditEvent {
    /// The variant's serialised snake_case name — the one string that is
    /// simultaneously the serde form, the value a backend stores, the key
    /// `memorysafe_backend::AggregateKey` sorts by, and what
    /// `Backend::audit_aggregates` means by "the event's serialised snake_case
    /// name". Without it those are four independent encodings of the same
    /// thing, free to drift.
    ///
    /// **`AuditEvent` deliberately has no `Ord`.** Deriving one would give
    /// *declaration* order, which is nothing like this order: `rejected` is
    /// declared second and sorts tenth, `exported` is declared sixth and sorts
    /// second. Anything ordering audit events must compare `as_str()`, and the
    /// absence of a derive is what stops `a.event.cmp(&b.event)` from
    /// compiling into the wrong answer.
    ///
    /// The same trap has a storage form: a backend that stores the event as an
    /// integer ordinal and orders by that column gets declaration order.
    /// Storing an enum as an integer is an established pattern here —
    /// `SensitivityLevel::ordinal` does it, and for a good reason — so a
    /// backend author following the local convention lands on the wrong order
    /// by doing the idiomatic thing. Order by the stored *name*.
    pub fn as_str(&self) -> &'static str {
        match self {
            AuditEvent::Admitted => "admitted",
            AuditEvent::Rejected => "rejected",
            AuditEvent::Merged => "merged",
            AuditEvent::Forgotten => "forgotten",
            AuditEvent::Recalled => "recalled",
            AuditEvent::Exported => "exported",
            AuditEvent::Imported => "imported",
            AuditEvent::SubjectPurged => "subject_purged",
            AuditEvent::Reembedded => "reembedded",
            AuditEvent::PolicyChanged => "policy_changed",
            AuditEvent::MaintenanceRun => "maintenance_run",
        }
    }

    /// Every variant, for exhaustive iteration in tests and for a backend that
    /// needs to enumerate the event space. Adding a variant without adding it
    /// here is caught by `tests::every_audit_event_name_matches_its_serde_form`.
    pub const ALL: [AuditEvent; 11] = [
        AuditEvent::Admitted,
        AuditEvent::Rejected,
        AuditEvent::Merged,
        AuditEvent::Forgotten,
        AuditEvent::Recalled,
        AuditEvent::Exported,
        AuditEvent::Imported,
        AuditEvent::SubjectPurged,
        AuditEvent::Reembedded,
        AuditEvent::PolicyChanged,
        AuditEvent::MaintenanceRun,
    ];
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

/// An item's identity in an audit row: an id and a content digest, never the
/// body.
///
/// Fields are private and `from_item` is the only constructor, matching how
/// every other identity type in this crate is built. That closes the
/// accidental path — `ItemRef { id, digest: item.body.clone() }` will not
/// compile — but be precise about what it does not close: `Deserialize` can
/// still produce an `ItemRef` holding arbitrary text, so audit JSON arriving
/// from outside this process is not covered.
///
/// The guarantee is therefore structural for code in this workspace and
/// conventional at the deserialization boundary. See also `Reason::detail`,
/// which is free prose a policy writes and which no type can constrain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRef {
    id: ItemId,
    digest: String,
}

impl ItemRef {
    pub fn from_item(item: &MemoryItem) -> Self {
        Self {
            id: item.id.clone(),
            digest: item.digest(),
        }
    }

    pub fn id(&self) -> &ItemId {
        &self.id
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// What a subject purge does to that subject's **audit** rows. Items,
/// vectors, idempotency records and capacity accounting go either way; this
/// decides only the audit detail.
///
/// **An enum in `memorysafe-core`, not a `bool` on `Backend::purge_subject`.**
/// It lives here rather than in the engine because it is a parameter of the
/// backend trait, and `memorysafe-backend` may only depend on this crate. It
/// is not a `bool` because the call site is
/// `purge_subject(&tenant, &subject, cascade, audit)`: `true` there would say
/// nothing about which way it cascades, and — worse — a `bool` swapped with a
/// neighbouring argument is a type error only by luck, whereas swapping this
/// one will not compile. The cost of the wrong value is a destroyed audit
/// trail or an undeleted one, in the erasure path.
///
/// The retention profile that chooses between these lives in the engine
/// (`RetentionProfile`, Task 36); the backend is handed the decision, never
/// the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PurgeCascade {
    /// The subject's audit rows are deleted along with everything else.
    Cascade,
    /// The subject's audit rows survive the subject. Bodies were never in
    /// them, so what remains is ids, digests, and feature numbers.
    Preserve,
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
    /// `at` is supplied, not read from the wall clock: replaying an audit log
    /// against a new policy version must produce comparable rows, and a
    /// constructor that stamped `now_utc()` would make every replayed row
    /// differ from the original.
    pub fn new(
        scope: Scope,
        event: AuditEvent,
        items: Vec<ItemRef>,
        actor: Actor,
        at: OffsetDateTime,
    ) -> Self {
        Self {
            id: AuditId::new(),
            at,
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
    /// Narrow to one subject. Subject is the delete/export unit, so "produce
    /// every audit row for subject X" is THE compliance query — and without
    /// this field it is inexpressible, because after a purge the caller no
    /// longer has the item ids to ask by.
    pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>,
    #[serde(with = "time::serde::timestamp::option")]
    pub since: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp::option")]
    pub until: Option<OffsetDateTime>,
    /// Cursor. Rows are ordered by `AuditId`, a millisecond ULID and therefore
    /// a total order — `at` is whole seconds and cannot separate rows written
    /// in the same second, so a time-based cursor would repeat or skip them.
    pub after: Option<AuditId>,
    /// Maximum rows to return. Defaults to 100.
    ///
    /// No `truncated` flag, and none is needed: `after` is a ULID cursor, so
    /// a caller detects the end of the log from the page size alone —
    /// `returned.len() < limit` means exhausted. `Backend::audit` states the
    /// rule and requires implementations to return exactly
    /// `min(limit, remaining)`; a backend that returns a short page for any
    /// other reason breaks the signal.
    pub limit: usize,
}

impl Default for AuditFilter {
    fn default() -> Self {
        Self {
            events: vec![],
            item: None,
            subject: None,
            namespace: None,
            since: None,
            until: None,
            after: None,
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
    fn every_audit_event_name_matches_its_serde_form() {
        // `as_str` is a second copy of the names serde writes, kept so that
        // ordering and storage can borrow one instead of allocating. A second
        // copy is only safe while something compares the two, and this is that
        // something: serde's output is a stored wire format, so a drift here
        // would rename a storage key silently.
        //
        // Vacuous if `ALL` is ever shortened — a variant left out of it is a
        // variant this loop never checks — so the length is asserted against
        // the count the array type fixes, and `as_str`'s match has no wildcard
        // arm, which makes a new variant a compile error there rather than a
        // silent omission here.
        assert_eq!(AuditEvent::ALL.len(), 11);
        for event in AuditEvent::ALL {
            assert_eq!(
                format!("\"{}\"", event.as_str()),
                serde_json::to_string(&event).unwrap(),
                "as_str and the serde form must be the same string: {event:?}"
            );
        }
        // And the names are distinct, or the ordering they key would not be a
        // total order over the event space.
        let mut names: Vec<&str> = AuditEvent::ALL.iter().map(|e| e.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), AuditEvent::ALL.len());
    }

    #[test]
    fn item_ref_carries_a_digest_and_never_the_body() {
        let i = item("a secret diagnosis");
        let r = ItemRef::from_item(&i);
        assert_eq!(*r.id(), i.id);
        assert_eq!(r.digest(), i.digest());
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
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
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
    fn purge_cascade_serializes_as_snake_case() {
        // The wire form is load-bearing: `AuditRetention` (Task 36) is
        // deserialized from tenant configuration, so a variant renamed on the
        // wire silently turns a configured `preserve` into a parse error in
        // the one path that erases a subject.
        //
        // Rejects: the plausible enum written without the
        // `#[serde(rename_all = "snake_case")]` attribute every neighbour in
        // this module carries — it would emit `"Cascade"`/`"Preserve"`.
        //
        // Vacuous if: `snake_case` and `lowercase` are indistinguishable for
        // both variants, since each is a single lowercase word. This test
        // pins the wire form, not the choice of rename strategy — see
        // `actor_kinds_serialize_as_snake_case`, where `ApiKey` does separate
        // the two.
        assert_eq!(
            serde_json::to_string(&PurgeCascade::Cascade).unwrap(),
            "\"cascade\""
        );
        assert_eq!(
            serde_json::to_string(&PurgeCascade::Preserve).unwrap(),
            "\"preserve\""
        );
        assert_eq!(
            serde_json::from_str::<PurgeCascade>("\"preserve\"").unwrap(),
            PurgeCascade::Preserve
        );
    }

    #[test]
    fn default_audit_filter_matches_everything() {
        let f = AuditFilter::default();
        assert!(f.events.is_empty());
        assert_eq!(f.limit, 100);
        assert!(f.since.is_none());
        assert!(f.until.is_none());
        assert!(f.item.is_none());
        assert!(f.subject.is_none());
        assert!(f.namespace.is_none());
        assert!(f.after.is_none());
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
            items: vec![
                serde_json::from_str(
                    r#"{"id":"01BX5ZZKBKACTAV9WEVGEMMVRZ","digest":"abc123def456"}"#,
                )
                .unwrap(),
            ],
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
                subject: None,
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
            r#""decision":{"subject":null,"action":{"kind":"retain","protection":{"kind":"normal"}},"evictions":[],"#,
            r#""reasons":[{"code":"novel_content","detail":"novel content detected","evidence":{"similarity":0.1}}],"#,
            r#""policy":{"name":"default","version":"1.0.0"}},"#,
            r#""actor":{"kind":"agent","id":"agent-1"}}"#
        );
        assert_eq!(json, expected, "audit record wire format changed");
    }

    fn sample_assessment() -> Assessment {
        use crate::assessment::{
            AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityCategory,
        };
        use crate::features;

        // Deterministic for equality assertions: all fixed values.
        Assessment {
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
        }
    }

    fn sample_decision() -> Decision {
        use crate::decision::{PolicyId, Reason, ReasonCode};
        use crate::features;

        Decision {
            subject: None,
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
        }
    }

    #[test]
    fn actor_kinds_serialize_as_snake_case() {
        // `ApiKey` is the only variant where snake_case and lowercase diverge,
        // so it is the only one that proves which strategy is in force.
        assert_eq!(
            serde_json::to_string(&ActorKind::ApiKey).unwrap(),
            "\"api_key\""
        );
        assert_eq!(serde_json::to_string(&ActorKind::Cli).unwrap(), "\"cli\"");
        assert_eq!(Actor::system().kind, ActorKind::System);
        assert!(Actor::system().id.is_none());
    }

    #[test]
    fn the_builders_do_not_clobber_each_other() {
        // `with_assessment` and `with_decision` each rebuild the record; a typo
        // clearing the other's field would pass every other test here, because
        // the golden test constructs its record with a struct literal.
        let scope = Scope::new("t", "s", "n").unwrap();
        let rec = AuditRecord::new(
            scope,
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
        )
        .with_assessment(sample_assessment())
        .with_decision(sample_decision());
        // Equality, not `is_some()`: a builder that ignored its argument and
        // stored some other value would pass an is_some check.
        assert_eq!(rec.assessment.as_ref(), Some(&sample_assessment()));
        assert_eq!(rec.decision.as_ref(), Some(&sample_decision()));
        // And it must return the SAME record, not a fresh one — a builder that
        // rebuilt from scratch would lose these.
        assert_eq!(rec.event, AuditEvent::Admitted);
        assert_eq!(rec.actor, Actor::system());
        assert_eq!(rec.scope.subject.as_str(), "s");

        // And in the opposite order.
        let scope = Scope::new("t", "s", "n").unwrap();
        let rec = AuditRecord::new(
            scope,
            AuditEvent::Forgotten,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(1_000_000).unwrap(),
        )
        .with_decision(sample_decision())
        .with_assessment(sample_assessment());
        assert_eq!(rec.assessment.as_ref(), Some(&sample_assessment()));
        assert_eq!(rec.decision.as_ref(), Some(&sample_decision()));
        assert_eq!(
            rec.event,
            AuditEvent::Forgotten,
            "new() ignored its event argument"
        );
    }
}
