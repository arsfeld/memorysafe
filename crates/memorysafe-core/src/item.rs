use crate::assessment::SensitivityLevel;
use crate::ids::{ItemId, Scope};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Agent,
    Session,
    Tool,
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub kind: SourceKind,
    pub id: Option<String>,
}

/// Engine-maintained. Set from `Action::Retain`, changed only by an explicit
/// `protect` call or by a `maintain` decision expiring a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Protection {
    Normal,
    Protected {
        #[serde(with = "time::serde::timestamp")]
        until: OffsetDateTime,
    },
    /// Absolute. No policy may evict a pinned item; the engine refuses any
    /// decision that tries.
    Pinned,
}

impl Protection {
    pub fn is_evictable(&self, now: OffsetDateTime) -> bool {
        match self {
            Protection::Normal => true,
            Protection::Pinned => false,
            Protection::Protected { until } => *until <= now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: ItemId,
    pub scope: Scope,
    pub body: String,
    pub kind: String,
    pub source: Source,
    #[serde(with = "time::serde::timestamp::option")]
    pub occurred_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::timestamp")]
    pub created_at: OffsetDateTime,
    pub tags: Vec<String>,
    pub attrs: BTreeMap<String, serde_json::Value>,
    /// Resolved by the policy; the caller's hint may only raise it.
    pub sensitivity: SensitivityLevel,
    pub ttl: Option<Duration>,
    pub protection: Protection,
    /// True when the embedder was unavailable at write time. Excluded from
    /// vector retrieval until backfilled; still visible to keyword and review.
    pub pending_embedding: bool,
}

impl MemoryItem {
    /// BLAKE3 of the content-identifying fields, hex encoded. Used in audit
    /// rows, which never store bodies.
    pub fn digest(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.body.as_bytes());
        h.update(b"\x1f");
        h.update(self.kind.as_bytes());
        h.update(b"\x1f");
        for tag in &self.tags {
            h.update(tag.as_bytes());
            // Unit separator, not a comma: `["a,b"]` and `["a", "b"]` would
            // otherwise hash identically, and this digest is an item's identity
            // in the audit trail.
            h.update(b"\x1f");
        }
        h.finalize().to_hex().to_string()
    }

    /// Charged against `Budget::max_bytes`.
    ///
    /// Counts every variable-length field that costs a byte on disk. `subject`
    /// and `namespace` are per-row columns in the SQLite backend, and
    /// `source.id` is an unbounded caller-supplied string — omitting either
    /// would let a caller consume real storage at zero charge and silently
    /// overrun the namespace budget. `tenant` is deliberately excluded: it is
    /// the database filename, not a column, so it costs nothing per row.
    pub fn byte_size(&self) -> u64 {
        let attrs = serde_json::to_string(&self.attrs)
            .map(|s| s.len())
            .unwrap_or(0);
        let tags: usize = self.tags.iter().map(|t| t.len()).sum();
        let source_id = self.source.id.as_ref().map(|s| s.len()).unwrap_or(0);
        let scope = self.scope.subject.as_str().len() + self.scope.namespace.as_str().len();
        // 64 approximates the fixed-width fields: ULID, two timestamps, ttl,
        // sensitivity ordinal, protection tag, and the pending flag.
        const FIXED_OVERHEAD: usize = 64;
        (self.body.len() + self.kind.len() + attrs + tags + source_id + scope + FIXED_OVERHEAD)
            as u64
    }

    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at + ttl <= now,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn item(body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: body.to_string(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: Some("agent-1".into()),
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
    fn digest_is_stable_and_content_addressed() {
        assert_eq!(item("hello").digest(), item("hello").digest());
        assert_ne!(item("hello").digest(), item("world").digest());
        assert_eq!(item("hello").digest().len(), 64); // blake3 hex

        // kind and tags are hashed too — the doc comment claims it and the
        // audit trail relies on it to tell two items apart.
        let mut other_kind = item("hello");
        other_kind.kind = "preference".into();
        assert_ne!(
            item("hello").digest(),
            other_kind.digest(),
            "kind is not hashed"
        );

        let mut tagged = item("hello");
        tagged.tags = vec!["work".into()];
        assert_ne!(
            item("hello").digest(),
            tagged.digest(),
            "tags are not hashed"
        );

        // Tag lists must not collide across different groupings.
        let mut joined = item("hello");
        joined.tags = vec!["a,b".into()];
        let mut split = item("hello");
        split.tags = vec!["a".into(), "b".into()];
        assert_ne!(
            joined.digest(),
            split.digest(),
            "tag delimiter is ambiguous"
        );
    }

    #[test]
    fn byte_size_counts_body_and_metadata() {
        let small = item("a");
        let large = item(&"a".repeat(1000));
        assert!(large.byte_size() > small.byte_size());
        assert!(small.byte_size() > 0);

        // Every variable-length field must move the charge, or a caller can
        // consume storage for free and overrun the budget.
        let base = item("a").byte_size();

        let mut tagged = item("a");
        tagged.tags = vec!["a-fairly-long-tag-value".into()];
        assert!(tagged.byte_size() > base, "tags are not charged");

        let mut attributed = item("a");
        attributed
            .attrs
            .insert("k".into(), serde_json::json!("a-long-attribute-value"));
        assert!(attributed.byte_size() > base, "attrs are not charged");

        let mut sourced = item("a");
        sourced.source.id = Some("a".repeat(500));
        assert!(sourced.byte_size() > base, "source.id is not charged");

        let mut deep = item("a");
        deep.scope = Scope::new("t", "a-long-subject-identifier", "a-long-namespace").unwrap();
        assert!(deep.byte_size() > base, "scope columns are not charged");

        let mut kinded = item("a");
        kinded.kind = "a-much-longer-kind-name".into();
        assert!(kinded.byte_size() > base, "kind is not charged");
    }

    #[test]
    fn pinned_items_are_never_evictable_at_any_instant() {
        // Pinning is absolute. A single-timestamp test would still pass for a
        // regression like `Pinned => t.unix_timestamp() < 0`.
        for ts in [i32::MIN as i64, -1, 0, 1, 1_700_000_000, i32::MAX as i64] {
            let t = OffsetDateTime::from_unix_timestamp(ts).unwrap();
            assert!(
                !Protection::Pinned.is_evictable(t),
                "pinned became evictable at {ts}"
            );
            assert!(Protection::Normal.is_evictable(t));
        }
    }

    #[test]
    fn protection_window_expires() {
        let now = OffsetDateTime::from_unix_timestamp(1000).unwrap();
        let future = OffsetDateTime::from_unix_timestamp(2000).unwrap();
        let past = OffsetDateTime::from_unix_timestamp(500).unwrap();
        assert!(!Protection::Protected { until: future }.is_evictable(now));
        assert!(Protection::Protected { until: past }.is_evictable(now));

        // The boundary is inclusive: a window ending exactly now has expired.
        assert!(Protection::Protected { until: now }.is_evictable(now));
        let plus_one = OffsetDateTime::from_unix_timestamp(1001).unwrap();
        assert!(!Protection::Protected { until: plus_one }.is_evictable(now));
    }

    #[test]
    fn serde_forms_are_a_stored_wire_format() {
        // Audit rows and the portable export archive both store these shapes.
        // Changing a tag key or the timestamp encoding is a data-compatibility
        // break, not a refactor.
        assert_eq!(
            serde_json::to_string(&Protection::Normal).unwrap(),
            r#"{"kind":"normal"}"#
        );
        assert_eq!(
            serde_json::to_string(&Protection::Pinned).unwrap(),
            r#"{"kind":"pinned"}"#
        );
        let until = OffsetDateTime::from_unix_timestamp(1000).unwrap();
        assert_eq!(
            serde_json::to_string(&Protection::Protected { until }).unwrap(),
            r#"{"kind":"protected","until":1000}"#
        );
        assert_eq!(
            serde_json::to_string(&SourceKind::Human).unwrap(),
            r#""human""#
        );
        // Timestamps are Unix seconds, not RFC3339.
        let i = item("wire format");
        let json = serde_json::to_string(&i).unwrap();
        assert!(
            json.contains(r#""created_at":0"#),
            "timestamp encoding changed: {json}"
        );
        let back: MemoryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, i);
    }

    #[test]
    fn a_full_memory_item_pins_its_stored_bytes() {
        // The portable export archive stores these as newline-delimited JSON.
        // A round trip alone proves only self-consistency: serialise and
        // deserialise share the same code, so ANY symmetric rename round-trips
        // perfectly while orphaning every row already on disk. Pin the literal
        // bytes, which is the property stored data actually depends on.
        let mut attrs = BTreeMap::new();
        attrs.insert("category".to_string(), serde_json::json!("memory"));
        attrs.insert("priority".to_string(), serde_json::json!(true));

        let item = MemoryItem {
            id: ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            scope: Scope::new("t", "s", "n").unwrap(),
            body: "This is the memory content".to_string(),
            kind: "note".to_string(),
            source: Source {
                kind: SourceKind::Human,
                id: Some("user-123".to_string()),
            },
            occurred_at: Some(OffsetDateTime::from_unix_timestamp(500).unwrap()),
            created_at: OffsetDateTime::from_unix_timestamp(1000).unwrap(),
            tags: vec!["important".to_string(), "review".to_string()],
            attrs,
            sensitivity: SensitivityLevel::Personal,
            ttl: Some(Duration::days(30)),
            protection: Protection::Protected {
                until: OffsetDateTime::from_unix_timestamp(2000).unwrap(),
            },
            pending_embedding: true,
        };

        let json = serde_json::to_string(&item).unwrap();
        let back: MemoryItem = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back, item,
            "a stored memory item did not survive the round trip"
        );

        let expected = concat!(
            r#"{"id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","scope":{"tenant":"t","subject":"s","#,
            r#""namespace":"n"},"body":"This is the memory content","kind":"note","#,
            r#""source":{"kind":"human","id":"user-123"},"occurred_at":500,"#,
            r#""created_at":1000,"tags":["important","review"],"attrs":{"category":"memory","#,
            r#""priority":true},"sensitivity":"personal","ttl":[2592000,0],"#,
            r#""protection":{"kind":"protected","until":2000},"pending_embedding":true}"#
        );
        assert_eq!(json, expected, "the stored memory item format changed");

        // And prove old bytes still parse — the actual compatibility question.
        let from_disk: MemoryItem = serde_json::from_str(expected).unwrap();
        assert_eq!(from_disk, item);
    }
}
