use memorysafe_core::{AuditRecord, MemoryItem, Namespace, SubjectId, TenantId};
use memorysafe_embed::QuantizedVector;
use serde::{Deserialize, Serialize};

/// Selects what to export. `None` on a level means "all of them".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSelector {
    pub tenant: TenantId,
    pub subject: Option<SubjectId>,
    pub namespace: Option<Namespace>,
    pub include_audit: bool,
}

/// One line of the export stream. Newline-delimited JSON.
///
/// Tagged `"record"`: every line names its record type under that key
/// (`"record":"header"`, `"record":"item"`, `"record":"audit"`), which later
/// tasks depend on for import. The `Audit` variant's payload field is named
/// `audit`, not `record` — serde's internally-tagged representation rejects a
/// tag key that collides with a field name in any variant, and reusing `kind`
/// instead would have made an exported item line read
/// `{"record":"item","item":{...,"kind":"fact",...}}` doubly, with two
/// unrelated meanings for the same key one nesting level apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum ExportRecord {
    Header {
        format_version: u32,
        exported_at: i64,
    },
    Item {
        item: Box<MemoryItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        vector: Option<ExportVector>,
    },
    Audit {
        audit: Box<AuditRecord>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportVector {
    pub embedder: String,
    pub dim: u16,
    pub scale: f32,
    /// Base64 of the int8 bytes.
    pub q_base64: String,
}

impl ExportVector {
    pub fn from_quantized(q: &QuantizedVector) -> Self {
        use base64::Engine as _;
        Self {
            embedder: q.embedder.to_string(),
            dim: q.dim,
            scale: q.scale,
            q_base64: base64::engine::general_purpose::STANDARD.encode(q.to_bytes()),
        }
    }
}

pub type ExportStream = Vec<ExportRecord>;
pub type ImportStream = Vec<ExportRecord>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImportReport {
    pub items_imported: u64,
    pub vectors_imported: u64,
    pub audit_imported: u64,
    pub items_skipped_existing: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::{Actor, AuditEvent, Scope};
    use time::OffsetDateTime;

    #[test]
    fn export_records_are_tagged_by_record_not_kind() {
        // Every exported line must name its record type under the "record"
        // key, not "kind" — a later task's import literals and its
        // `value.get("record")` assertion both depend on this exact key.
        // Reusing "kind" would also collide in meaning with `MemoryItem.kind`
        // one nesting level down inside an `Item` line.
        let header = ExportRecord::Header {
            format_version: 1,
            exported_at: 0,
        };
        let json = serde_json::to_string(&header).unwrap();
        assert_eq!(
            json,
            r#"{"record":"header","format_version":1,"exported_at":0}"#
        );

        let record = AuditRecord::new(
            Scope::new("t", "s", "n").unwrap(),
            AuditEvent::Admitted,
            vec![],
            Actor::system(),
            OffsetDateTime::UNIX_EPOCH,
        );
        let audit = ExportRecord::Audit {
            audit: Box::new(record.clone()),
        };
        let json = serde_json::to_string(&audit).unwrap();
        assert!(
            json.contains(r#""record":"audit""#),
            "audit line lost its record tag: {json}"
        );
        assert!(
            json.contains(r#""audit":{"#),
            "audit payload is not keyed \"audit\": {json}"
        );

        // Round-trip so the deserialize side is pinned too, not just the
        // serialize side.
        let back: ExportRecord = serde_json::from_str(&json).unwrap();
        match back {
            ExportRecord::Audit { audit: got } => assert_eq!(*got, record),
            other => panic!("expected ExportRecord::Audit, got {other:?}"),
        }
    }

    #[test]
    fn exported_vectors_round_trip_through_standard_padded_base64() {
        use base64::Engine as _;
        use memorysafe_embed::{DeterministicEmbedder, Embedder};

        let embedder = DeterministicEmbedder::new(16);
        let embedding = embedder.embed("round trip me").unwrap();
        let q = QuantizedVector::from_embedding(&embedding);

        let exported = ExportVector::from_quantized(&q);
        assert_eq!(exported.embedder, q.embedder.to_string());
        assert_eq!(exported.dim, q.dim);
        assert_eq!(exported.scale, q.scale);

        // Task 24's import decodes with a matching engine — pin that the
        // encoding is STANDARD (padded), not STANDARD_NO_PAD or URL-safe, so
        // a mismatch is caught here rather than surfacing only in import.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&exported.q_base64)
            .expect("STANDARD engine must decode what from_quantized produced");
        assert_eq!(decoded, q.to_bytes());
    }
}
