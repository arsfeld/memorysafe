use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::tenant::SqlResultExt;
use crate::{capacity, items, vectors};
use base64::Engine as _;
use memorysafe_backend::{
    BackendError, ExportRecord, ExportStream, ExportVector, FORMAT_VERSION, ImportReport,
    ImportStream, ScopeSelector,
};
use memorysafe_core::{AuditFilter, Scope, TenantId};
use memorysafe_embed::QuantizedVector;
use rusqlite::{Connection, params};

// `FORMAT_VERSION` is `memorysafe-backend`'s, not a private copy. "A supported
// format version" is a property of the format, not of whichever backend is
// reading the stream; two backends each declaring their own constant is one
// silent divergence away from a SQLite export Postgres refuses.

pub fn export(conn: &Connection, sel: &ScopeSelector) -> Result<ExportStream, BackendError> {
    let mut out: ExportStream = vec![ExportRecord::Header {
        format_version: FORMAT_VERSION,
        exported_at: time::OffsetDateTime::now_utc().unix_timestamp(),
    }];

    let sql = format!(
        "SELECT {cols}, v.embedder AS v_embedder, v.dim AS v_dim, v.scale AS v_scale,
                v.q AS v_q
         FROM items i LEFT JOIN vectors v ON v.item_id = i.id
         WHERE (?1 IS NULL OR i.subject = ?1) AND (?2 IS NULL OR i.namespace = ?2)
         ORDER BY i.id ASC",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = sel.tenant.as_str().to_string();
    let subject = sel.subject.as_ref().map(|s| s.as_str().to_string());
    let namespace = sel.namespace.as_ref().map(|n| n.as_str().to_string());

    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(params![subject, namespace], move |r| {
            let item = row_to_item(r, &tenant)?;
            let embedder: Option<String> = r.get("v_embedder")?;
            let vector = match embedder {
                Some(embedder) => {
                    let dim: i64 = r.get("v_dim")?;
                    let scale: f64 = r.get("v_scale")?;
                    let q: Vec<u8> = r.get("v_q")?;
                    Some(ExportVector {
                        embedder,
                        dim: dim as u16,
                        scale: scale as f32,
                        q_base64: base64::engine::general_purpose::STANDARD.encode(q),
                    })
                }
                None => None,
            };
            Ok((item, vector))
        })
        .sql()?;

    let mut scopes = Vec::new();
    for row in rows {
        let (item, vector) = row.sql()?;
        if !scopes.contains(&item.scope) {
            scopes.push(item.scope.clone());
        }
        out.push(ExportRecord::Item {
            item: Box::new(item),
            vector,
        });
    }

    if sel.include_audit {
        // Collect across every scope before sorting: `audit::query` returns
        // each scope's rows newest-first (descending `AuditId`), but
        // `Backend::export`'s contract is one global run ascending by
        // `AuditId` — appending each scope's descending run back to back
        // would satisfy neither order.
        //
        // `limit: 100_000` truncates silently for a tenant with more audit
        // rows than that — the same defect `AuditFilter::limit`'s doc
        // comment warns about (see `crates/memorysafe-core/src/audit.rs`).
        // Not resolved here.
        let mut audit_rows = Vec::new();
        for scope in scopes {
            let filter = AuditFilter {
                limit: 100_000,
                ..Default::default()
            };
            audit_rows.extend(crate::audit::query(conn, &scope, &filter)?);
        }
        audit_rows.sort_by(|a, b| a.id.cmp(&b.id));
        for record in audit_rows {
            out.push(ExportRecord::Audit {
                audit: Box::new(record),
            });
        }
    }

    Ok(out)
}

pub fn import(
    conn: &mut Connection,
    destination: &TenantId,
    stream: ImportStream,
) -> Result<ImportReport, BackendError> {
    let tx = conn
        .transaction()
        .map_err(|e| crate::tenant::storage_error(e, false))?;
    let mut report = ImportReport::default();
    // `Backend::import`'s doc: "at least one Header must be present, and
    // every Header present must carry FORMAT_VERSION" — and names this as a
    // gap in the pre-Task-24 draft, which checked the version only inside
    // the `Header` arm below and so accepted a stream that never matched it
    // at all, with no version check ever running. Checked after the loop,
    // not before it starts: nothing commits until `tx.commit()` below, so a
    // rejection here still leaves the destination untouched, exactly as a
    // mid-loop rejection does.
    let mut saw_header = false;

    for record in stream {
        match record {
            ExportRecord::Header { format_version, .. } => {
                saw_header = true;
                if format_version != FORMAT_VERSION {
                    return Err(BackendError::MalformedImport(format!(
                        "unsupported format version {format_version}"
                    )));
                }
            }
            ExportRecord::Item { item, vector } => {
                let scope: Scope = item.scope.clone();
                // Every record is compared against `destination` — never
                // against another record. No record's tenant is authority for
                // any other's, so a disagreement rejects the whole import
                // rather than being retargeted. `subject` and `namespace` are
                // preserved exactly as written; only the tenant is checked,
                // and it is checked rather than assigned.
                if scope.tenant != *destination {
                    return Err(BackendError::MalformedImport(format!(
                        "item {} names tenant {} but the destination is {destination}",
                        item.id, scope.tenant
                    )));
                }
                // Import is idempotent: an item already present is skipped
                // rather than duplicated or overwritten.
                if items::exists(&tx, &scope, &item.id)? {
                    report.items_skipped_existing += 1;
                    continue;
                }
                // `protection` and `sensitivity` are stored exactly as the
                // stream carries them — a deliberate deviation from an
                // earlier draft of this function, which reset `protection` to
                // `Normal` on the theory that a stream claiming `Pinned`
                // could starve a namespace's budget. `Backend::import`'s own
                // doc names no such rule, and
                // `conformance::lifecycle::export_import_round_trips_exactly`
                // is explicit: it pins one item to `Protection::Pinned`
                // before exporting and asserts the *whole* `MemoryItem`,
                // protection included, survives the round trip unchanged.
                // Trusting neither field at the backend layer is still the
                // right instinct — it belongs to whichever layer decides
                // whether an import is trusted input, which for this trait is
                // the caller of `Backend::import`, not the backend itself.
                capacity::ensure_row(&tx, &scope)?;
                items::insert(&tx, &item)?;
                capacity::adjust(&tx, &scope, 1, item.byte_size() as i64)?;
                report.items_imported += 1;

                if let Some(v) = vector {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(&v.q_base64)
                        .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    let q = QuantizedVector::from_bytes(
                        memorysafe_core::EmbedderId::new(&v.embedder),
                        v.dim,
                        v.scale,
                        &bytes,
                    )
                    .map_err(|e| BackendError::MalformedImport(e.to_string()))?;
                    vectors::insert(&tx, &item.id, &scope, &q)?;
                    report.vectors_imported += 1;
                }
            }
            ExportRecord::Audit { audit } => {
                // The same comparison, so audit rows need no rule of their
                // own: same tenant as the destination, preserve the row
                // byte-exact; different, reject. The scope is never rewritten
                // to make the row fit — a rewritten audit row is a forged one.
                if audit.scope.tenant != *destination {
                    return Err(BackendError::MalformedImport(format!(
                        "audit row {} names tenant {} but the destination is {destination}",
                        audit.id, audit.scope.tenant
                    )));
                }
                crate::audit::insert(&tx, &audit)?;
                // Imported audit rows increment too: an `ExportStream` carries
                // no aggregate records, so a migrated tenant would otherwise
                // hold detail with no summary and lose the history at its first
                // cascading purge. A re-import cannot double-count — audit rows
                // keep their own `AuditId`, which is the table's primary key,
                // so the second import conflicts and `import` is all-or-nothing.
                crate::aggregates::increment(&tx, &audit)?;
                report.audit_imported += 1;
            }
        }
    }

    if !saw_header {
        return Err(BackendError::MalformedImport(
            "import stream carries no Header record".into(),
        ));
    }

    tx.commit()
        .map_err(|e| crate::tenant::storage_error(e, false))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_core::{ItemId, MemoryItem, SensitivityLevel, Source, SourceKind};
    use std::collections::BTreeMap;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialise(&conn).unwrap();
        conn
    }

    fn scope() -> Scope {
        Scope::new("t", "s", "n").unwrap()
    }

    fn item_with(source_kind: SourceKind, protection: memorysafe_core::Protection) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: scope(),
            body: "round trip me".into(),
            kind: "fact".into(),
            source: Source {
                kind: source_kind,
                id: None,
            },
            occurred_at: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: BTreeMap::new(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection,
            pending_embedding: false,
        }
    }

    /// `row_to_item`'s two lossy deserialisers — `source_kind_from` and
    /// `protection_from` in `items.rs` — each carry a silent catch-all
    /// fallback (`SourceKind::Agent`, `Protection::Normal`). Neither
    /// fallback panics or errors, so a deleted match arm is invisible unless
    /// a test actually constructs the value that arm exists for. `fx::item`
    /// — the conformance suite's only fixture constructor — pins
    /// `SourceKind::Agent` and `Protection::Normal`, which are exactly what a
    /// broken arm falls back to, so no conformance test, and no test built on
    /// `fx::item`, can ever see these three arms fail. Constructed directly
    /// here for that reason, and round-tripped through `export`/`import`
    /// rather than through `items::insert`/`get` directly: this is the path
    /// a caller actually uses to move data between tenants, and the one this
    /// module owns. `import` writes with `protection_parts`/`source_kind_str`
    /// (exhaustive matches, no catch-all — not at risk of this defect
    /// shape); `export` reads back with `row_to_item`, which is where
    /// `source_kind_from`/`protection_from` live, so the mutation is
    /// reachable from either the export step or the post-import read.
    ///
    /// **`Protection::Pinned` is the sharp one.** `Protection::Pinned`'s own
    /// doc says no policy may ever evict a pinned item. A backend whose
    /// `protection_from` silently maps `"pinned"` down to `Protection::Normal`
    /// — what a deleted arm falls back to — makes a protected memory
    /// evictable with no error anywhere: the failure is a policy decision
    /// made against silently corrupted data, not a crash.
    ///
    /// Also carries a `Protection::Protected { until }` item, which no
    /// conformance test constructs at all (its parse arm is otherwise
    /// covered — `items.rs`'s own round-trip test builds one — but nothing
    /// round-trips the `until` timestamp specifically through the
    /// export/import path this module owns).
    ///
    /// Confirmed by mutation, not only by this test passing: deleting each of
    /// the `source_kind_from`/`protection_from` arms this test exists for was
    /// run against the source and each failed this exact test (see the task
    /// report).
    #[test]
    fn source_kind_and_pinned_protection_survive_export_and_import() {
        use memorysafe_core::Protection;
        use time::OffsetDateTime;

        let source_conn = db();
        let mut target_conn = db();
        let s = scope();

        let session_item = item_with(SourceKind::Session, Protection::Normal);
        let tool_item = item_with(SourceKind::Tool, Protection::Normal);
        let pinned_item = item_with(SourceKind::Agent, Protection::Pinned);
        let until = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let protected_item = item_with(SourceKind::Agent, Protection::Protected { until });

        for item in [&session_item, &tool_item, &pinned_item, &protected_item] {
            items::insert(&source_conn, item).unwrap();
        }

        let sel = ScopeSelector {
            tenant: s.tenant.clone(),
            subject: None,
            namespace: None,
            include_audit: false,
        };
        let stream = export(&source_conn, &sel).unwrap();
        assert_eq!(
            stream
                .iter()
                .filter(|r| matches!(r, ExportRecord::Item { .. }))
                .count(),
            4,
            "all four items must have been exported, or the assertions below \
             prove nothing"
        );

        import(&mut target_conn, &s.tenant, stream).unwrap();

        let get =
            |conn: &Connection, id: &ItemId| items::get(conn, &s, id).unwrap().expect("imported");

        assert_eq!(
            get(&target_conn, &session_item.id).source.kind,
            SourceKind::Session,
            "SourceKind::Session did not survive export/import — it fell back to Agent"
        );
        assert_eq!(
            get(&target_conn, &tool_item.id).source.kind,
            SourceKind::Tool,
            "SourceKind::Tool did not survive export/import — it fell back to Agent"
        );
        assert_eq!(
            get(&target_conn, &pinned_item.id).protection,
            Protection::Pinned,
            "Protection::Pinned did not survive export/import — it fell back to \
             Normal, which would make a protected memory evictable"
        );
        assert_eq!(
            get(&target_conn, &protected_item.id).protection,
            Protection::Protected { until },
            "Protection::Protected{{until}} did not survive export/import with \
             its exact timestamp"
        );
    }
}
