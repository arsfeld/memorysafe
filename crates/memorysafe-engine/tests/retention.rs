use memorysafe_backend_sqlite::SqliteBackend;
use memorysafe_core::{AuditFilter, Scope, SubjectId, TenantId};
use memorysafe_embed::DeterministicEmbedder;
use memorysafe_engine::{
    Engine, EngineConfig, PurgeCascade, RememberRequest, RetentionProfile, RetentionSpan,
};
use memorysafe_policy::BaselinePolicy;
use std::sync::Arc;

fn engine(profile: RetentionProfile) -> Engine {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cfg = EngineConfig::new(
        Arc::new(SqliteBackend::open(dir.keep())),
        Arc::new(DeterministicEmbedder::new(256)),
        Arc::new(BaselinePolicy::default()),
    );
    cfg.retention = profile;
    Engine::new(cfg)
}

fn scope() -> Scope {
    Scope::new("acme", "user-42", "agent").unwrap()
}

#[test]
fn the_four_profiles_match_the_documented_table() {
    assert_eq!(
        RetentionProfile::Balanced.retention().detail,
        RetentionSpan::UntilSubjectPurge
    );
    assert_eq!(
        RetentionProfile::Balanced.retention().purge_cascade,
        PurgeCascade::Cascade
    );

    assert_eq!(
        RetentionProfile::GdprStrict.retention().detail,
        RetentionSpan::Days(90)
    );
    assert_eq!(
        RetentionProfile::GdprStrict.retention().aggregate,
        RetentionSpan::Days(365)
    );

    assert_eq!(
        RetentionProfile::HipaaRetain.retention().detail,
        RetentionSpan::Days(2190)
    );
    assert_eq!(
        RetentionProfile::HipaaRetain.retention().purge_cascade,
        PurgeCascade::Preserve
    );

    assert_eq!(
        RetentionProfile::Forensic.retention().detail,
        RetentionSpan::Forever
    );
    assert_eq!(
        RetentionProfile::Forensic.retention().purge_cascade,
        PurgeCascade::Preserve
    );
}

/// Not in the brief's Step 1 text. `the_four_profiles_match_the_documented_table`
/// above is the mandated test, verbatim; it checks a hand-picked subset of the
/// 4x3 table (each profile's `detail`, plus one profile's `purge_cascade` or
/// `aggregate`), and mutation testing found two fields it never names in any
/// profile:
///
/// - `aggregate` is asserted only on `GdprStrict`. `Balanced.aggregate`,
///   `HipaaRetain.aggregate` and `Forensic.aggregate` were free to drift to any
///   value — a mutant setting `Forensic.aggregate` to `Days(1)` survived the
///   full suite, because `aggregate` has no production consumer yet (see this
///   task's report) and no test named it directly.
/// - `purge_cascade` is asserted on `Balanced`, `HipaaRetain` and `Forensic`,
///   but never on `GdprStrict`. A mutant swapping `GdprStrict.purge_cascade`
///   from `Cascade` to `Preserve` also survived: no test purges a `GdprStrict`
///   engine and inspects `PurgeOutcome`, only `the_item_is_always_removed_...`
///   loop, which checks item removal and nothing about the audit trail.
///
/// This test names every cell of the table once, closing both gaps without
/// touching the mandated test above.
#[test]
fn every_profile_field_matches_the_documented_table_exactly() {
    assert_eq!(
        RetentionProfile::Balanced.retention().aggregate,
        RetentionSpan::Forever
    );
    assert_eq!(
        RetentionProfile::GdprStrict.retention().purge_cascade,
        PurgeCascade::Cascade
    );
    assert_eq!(
        RetentionProfile::GdprStrict.retention().aggregate,
        RetentionSpan::Days(365)
    );
    assert_eq!(
        RetentionProfile::HipaaRetain.retention().aggregate,
        RetentionSpan::Forever
    );
    assert_eq!(
        RetentionProfile::Forensic.retention().aggregate,
        RetentionSpan::Forever
    );
}

#[test]
fn profiles_parse_from_their_documented_names() {
    assert_eq!(
        RetentionProfile::from_name("balanced"),
        Some(RetentionProfile::Balanced)
    );
    assert_eq!(
        RetentionProfile::from_name("gdpr_strict"),
        Some(RetentionProfile::GdprStrict)
    );
    assert_eq!(
        RetentionProfile::from_name("hipaa_retain"),
        Some(RetentionProfile::HipaaRetain)
    );
    assert_eq!(
        RetentionProfile::from_name("forensic"),
        Some(RetentionProfile::Forensic)
    );
    assert_eq!(RetentionProfile::from_name("nonsense"), None);
    assert_eq!(RetentionProfile::default(), RetentionProfile::Balanced);
}

#[tokio::test]
async fn balanced_cascades_audit_with_the_subject() {
    let e = engine(RetentionProfile::Balanced);
    e.remember(RememberRequest::new(
        scope(),
        "a memory that will be purged",
    ))
    .await
    .unwrap();

    let report = e
        .purge_subject(
            &TenantId::new("acme").unwrap(),
            &SubjectId::new("user-42").unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(report.items_removed, 1);
    assert!(report.audit_rows_removed >= 1);
    assert_eq!(report.audit_rows_preserved, 0);
    // `Backend::purge_subject` inserts the `SubjectPurged` record itself
    // after the cascading delete, in the same transaction, under both
    // `PurgeCascade` variants — see its doc comment ("audit is inserted
    // either way") and
    // `purge_subject_removes_everything_for_that_subject`'s own assertion
    // that "a cascading purge must leave exactly its own SubjectPurged
    // record". So a cascade never leaves the audit trail empty; it leaves
    // exactly that one record, which is what distinguishes "the pre-existing
    // detail is gone" from "no purge was ever recorded".
    let audit = e.audit(&scope(), &AuditFilter::default()).await.unwrap();
    assert_eq!(
        audit.len(),
        1,
        "a cascading purge must leave exactly its own SubjectPurged record"
    );
    assert_eq!(audit[0].event, memorysafe_core::AuditEvent::SubjectPurged);
}

#[tokio::test]
async fn hipaa_retain_preserves_audit_across_a_subject_purge() {
    let e = engine(RetentionProfile::HipaaRetain);
    e.remember(RememberRequest::new(scope(), "a clinical note"))
        .await
        .unwrap();

    let report = e
        .purge_subject(
            &TenantId::new("acme").unwrap(),
            &SubjectId::new("user-42").unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(report.items_removed, 1, "the item itself always goes");
    assert!(
        report.audit_rows_preserved >= 1,
        "hipaa_retain must keep the decision record"
    );
    assert_eq!(report.audit_rows_removed, 0);

    let audit = e.audit(&scope(), &AuditFilter::default()).await.unwrap();
    assert!(
        !audit.is_empty(),
        "the audit trail was destroyed under hipaa_retain"
    );
    // Even preserved, no body may survive.
    let json = serde_json::to_string(&audit).unwrap();
    assert!(
        !json.contains("clinical note"),
        "preserved audit leaked a body"
    );
}

#[tokio::test]
async fn the_item_is_always_removed_regardless_of_profile() {
    for profile in [
        RetentionProfile::Balanced,
        RetentionProfile::GdprStrict,
        RetentionProfile::HipaaRetain,
        RetentionProfile::Forensic,
    ] {
        let e = engine(profile);
        e.remember(RememberRequest::new(scope(), "the memory itself"))
            .await
            .unwrap();
        e.purge_subject(
            &TenantId::new("acme").unwrap(),
            &SubjectId::new("user-42").unwrap(),
        )
        .await
        .unwrap();
        assert!(
            e.review(&scope(), &Default::default())
                .await
                .unwrap()
                .is_empty(),
            "{profile:?} left the item behind"
        );
    }
}
