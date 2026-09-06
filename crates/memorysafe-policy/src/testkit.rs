//! Fixture builders shared by this crate's unit tests.

use memorysafe_core::{
    Candidate, ItemId, MaintenanceCandidate, MemoryItem, Protection, Scope, Score, ScoredCandidate,
    SensitivityLevel, Source, SourceKind,
};
use time::OffsetDateTime;

pub fn scope() -> Scope {
    Scope::new("t", "s", "n").expect("valid test scope")
}

pub fn item(body: &str) -> MemoryItem {
    MemoryItem {
        id: ItemId::new(),
        scope: scope(),
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

pub fn candidate(body: &str, relevance: f32) -> ScoredCandidate {
    ScoredCandidate {
        item: item(body),
        relevance,
        vector_score: Some(relevance),
        keyword_score: None,
        value: Score::clamped(0.5),
        fragility: Score::clamped(0.5),
        estimated_tokens: 10,
        // Nothing in this crate reads either field yet — recency is realised
        // as decay during maintenance (Task 29), not by anything `admit`,
        // `value`, `fragility`, `eviction`, or `redundancy` compute today.
        // `None`/`0` (never-accessed) is the right default for every current
        // caller, not a placeholder standing in for a value some test needs.
        last_accessed_at: None,
        access_count: 0,
    }
}

/// A `MaintenanceCandidate` fixture, for `eviction` and `admit`'s capacity
/// path, which rank `Vec<MaintenanceCandidate>` rather than `ScoredCandidate`
/// (see the discrepancy note in the task report). Shares `candidate`'s
/// never-accessed default above, for the same reason: nothing here reads
/// `last_accessed_at` or `access_count` yet.
pub fn maintenance_candidate(body: &str, value: f32, fragility: f32) -> MaintenanceCandidate {
    MaintenanceCandidate {
        item: item(body),
        value: Score::clamped(value),
        fragility: Score::clamped(fragility),
        last_accessed_at: None,
        access_count: 0,
    }
}

/// A pre-admission write candidate, for `value` and `sensitivity` tests, which
/// operate on `Candidate` rather than the already-admitted `MemoryItem`.
pub fn candidate_from(body: &str, hint: Option<SensitivityLevel>) -> Candidate {
    Candidate {
        body: body.to_string(),
        kind: "fact".into(),
        tags: vec![],
        attrs: Default::default(),
        sensitivity_hint: hint,
        embedding: None,
        byte_size: body.len() as u64,
    }
}
