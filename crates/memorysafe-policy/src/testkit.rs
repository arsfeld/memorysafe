//! Fixture builders shared by this crate's unit tests.

use memorysafe_core::{
    ItemId, MemoryItem, Protection, Scope, Score, ScoredCandidate, SensitivityLevel, Source,
    SourceKind,
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
    }
}
