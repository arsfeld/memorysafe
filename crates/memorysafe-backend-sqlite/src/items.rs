//! The `items` table: one row per stored memory, keyed by `ItemId` and
//! addressed by `(subject, namespace)` within the tenant's own file.
//!
//! **The tenant is not a column.** It is the database filename, so every query
//! here carries a `(subject, namespace)` predicate and nothing more — see the
//! crate doc. The `tenant` argument that `row_to_item` takes is used only to
//! rebuild the `Scope` a caller asked with; it is never compared against
//! anything stored, because nothing stores it.

use crate::tenant::SqlResultExt;
use memorysafe_backend::{BackendError, Page};
use memorysafe_core::{
    ItemId, MemoryItem, Protection, Scope, SensitivityLevel, Source, SourceKind,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use time::{Duration, OffsetDateTime};

fn source_kind_str(k: SourceKind) -> &'static str {
    match k {
        SourceKind::Agent => "agent",
        SourceKind::Session => "session",
        SourceKind::Tool => "tool",
        SourceKind::Human => "human",
    }
}

/// **Errors on an unrecognised string rather than falling back to a default,
/// and the fallback is what made three of these arms untestable.**
///
/// `fx::item` — the conformance suite's only fixture constructor — pins
/// `SourceKind::Agent`, which is exactly what the old `_ => SourceKind::Agent`
/// produced for a broken arm. So the fixture's chosen value and the catch-all's
/// fallback were *the same value*, and no test built on that fixture could ever
/// observe `"session"` or `"tool"` failing to parse, however many were added.
/// With no fallback value there is nothing for a fixture to coincide with.
///
/// Erroring is safe here because the writer is exhaustive: [`source_kind_str`]
/// matches every `SourceKind` variant with no catch-all, so the only strings
/// that can reach this function are ones it produced. Anything else is
/// corruption, and corruption becoming `Agent` silently is worse than a read
/// that fails.
fn source_kind_from(s: &str) -> rusqlite::Result<SourceKind> {
    match s {
        "agent" => Ok(SourceKind::Agent),
        "session" => Ok(SourceKind::Session),
        "tool" => Ok(SourceKind::Tool),
        "human" => Ok(SourceKind::Human),
        other => Err(unreadable_enum("source_kind", other)),
    }
}

fn protection_parts(p: Protection) -> (&'static str, Option<i64>) {
    match p {
        Protection::Normal => ("normal", None),
        Protection::Pinned => ("pinned", None),
        Protection::Protected { until } => ("protected", Some(until.unix_timestamp())),
    }
}

/// Errors on an unrecognised string, for the reason on [`source_kind_from`] —
/// and the stakes here are higher. `fx::item` pins `Protection::Normal`, which
/// is what the old catch-all returned, so a broken `"pinned"` arm turned a
/// **pinned item into an evictable one** with no error anywhere.
/// `Protection::Pinned`'s own doc says no policy may evict a pinned item and
/// the engine refuses any decision that tries; a silent downgrade in the reader
/// defeats that without ever reaching a policy.
///
/// The `"protected"` arm keeps its own fallback deliberately: a row whose
/// `protected_until` is absent or not a valid timestamp has no protection
/// window to honour, and `Normal` is the correct reading of it rather than a
/// default standing in for an unknown.
fn protection_from(kind: &str, until: Option<i64>) -> rusqlite::Result<Protection> {
    match kind {
        "normal" => Ok(Protection::Normal),
        "pinned" => Ok(Protection::Pinned),
        "protected" => Ok(
            match until.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) {
                Some(until) => Protection::Protected { until },
                None => Protection::Normal,
            },
        ),
        other => Err(unreadable_enum("protection", other)),
    }
}

/// A stored enum string the writer could not have produced.
fn unreadable_enum(column: &str, got: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::other(format!(
            "{column} holds {got:?}, which no writer in this crate emits"
        ))),
    )
}

/// The columns `row_to_item` reads, in one place so a `SELECT` and the row
/// decoder cannot drift.
pub const ITEM_COLUMNS: &str = "id, subject, namespace, body, kind, source_kind, source_id, \
     occurred_at, created_at, tags, attrs, sensitivity, ttl_seconds, protection, \
     protected_until, byte_size, pending_embedding";

pub fn row_to_item(row: &Row<'_>, tenant: &str) -> rusqlite::Result<MemoryItem> {
    let tags_json: String = row.get("tags")?;
    let attrs_json: String = row.get("attrs")?;
    let subject: String = row.get("subject")?;
    let namespace: String = row.get("namespace")?;
    let sensitivity: i64 = row.get("sensitivity")?;
    let ttl: Option<i64> = row.get("ttl_seconds")?;
    let protection: String = row.get("protection")?;
    let protected_until: Option<i64> = row.get("protected_until")?;
    let occurred: Option<i64> = row.get("occurred_at")?;
    let created: i64 = row.get("created_at")?;
    let pending: i64 = row.get("pending_embedding")?;

    Ok(MemoryItem {
        id: ItemId::parse(&row.get::<_, String>("id")?)
            .map_err(|e| unreadable_enum("id", &e.to_string()))?,
        scope: Scope::new(tenant, &subject, &namespace)
            .map_err(|e| unreadable_enum("scope", &e.to_string()))?,
        body: row.get("body")?,
        kind: row.get("kind")?,
        source: Source {
            kind: source_kind_from(&row.get::<_, String>("source_kind")?)?,
            id: row.get("source_id")?,
        },
        occurred_at: occurred.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()),
        created_at: OffsetDateTime::from_unix_timestamp(created)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        attrs: serde_json::from_str(&attrs_json).unwrap_or_default(),
        sensitivity: SensitivityLevel::from_ordinal(sensitivity)
            .unwrap_or(SensitivityLevel::Restricted),
        ttl: ttl.map(Duration::seconds),
        protection: protection_from(&protection, protected_until)?,
        pending_embedding: pending != 0,
    })
}

pub fn insert(conn: &Connection, item: &MemoryItem) -> Result<(), BackendError> {
    let (protection, protected_until) = protection_parts(item.protection);
    conn.execute(
        "INSERT INTO items (id, subject, namespace, body, kind, source_kind, source_id,
             occurred_at, created_at, tags, attrs, sensitivity, ttl_seconds, protection,
             protected_until, byte_size, pending_embedding)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            item.id.as_str(),
            item.scope.subject.as_str(),
            item.scope.namespace.as_str(),
            item.body,
            item.kind,
            source_kind_str(item.source.kind),
            item.source.id,
            item.occurred_at.map(|t| t.unix_timestamp()),
            item.created_at.unix_timestamp(),
            serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&item.attrs).unwrap_or_else(|_| "{}".into()),
            item.sensitivity.ordinal(),
            item.ttl.map(|d| d.whole_seconds()),
            protection,
            protected_until,
            item.byte_size() as i64,
            i64::from(item.pending_embedding),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn get(
    conn: &Connection,
    scope: &Scope,
    id: &ItemId,
) -> Result<Option<MemoryItem>, BackendError> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items
         WHERE id = ?1 AND subject = ?2 AND namespace = ?3"
    );
    let mut stmt = conn.prepare(&sql).sql()?;
    let tenant = scope.tenant.as_str().to_string();
    let mut rows = stmt
        .query_map(
            params![
                id.as_str(),
                scope.subject.as_str(),
                scope.namespace.as_str()
            ],
            move |r| row_to_item(r, &tenant),
        )
        .sql()?;
    match rows.next() {
        Some(r) => Ok(Some(r.sql()?)),
        None => Ok(None),
    }
}

pub fn list(
    conn: &Connection,
    scope: &Scope,
    page: &Page,
) -> Result<Vec<MemoryItem>, BackendError> {
    // `Backend::list` mandates ascending `created_at` with ascending `id` as
    // the tie-break, and requires the pair to be a *total* order: a bulk
    // import leaves many rows sharing one `created_at`, and an unstable sort
    // under LIMIT/OFFSET can then repeat a row on two pages while dropping
    // another. `id` is a ULID, itself time-sortable, so the tie-break never
    // contradicts the primary key.
    //
    // `COLLATE BINARY` is stated rather than inherited, for the reason the
    // schema states it on every ordering index: the id is compared as text and
    // the collation an ordering runs under should be visible.
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM items
         WHERE subject = ?1 AND namespace = ?2
         ORDER BY created_at ASC, id COLLATE BINARY ASC LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql).sql()?;
    let tenant = scope.tenant.as_str().to_string();
    let rows = stmt
        .query_map(
            params![
                scope.subject.as_str(),
                scope.namespace.as_str(),
                page.effective_limit() as i64,
                page.offset as i64,
            ],
            move |r| row_to_item(r, &tenant),
        )
        .sql()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().sql()
}

/// Returns the byte size of what was removed, for capacity accounting.
pub fn delete(conn: &Connection, scope: &Scope, id: &ItemId) -> Result<u64, BackendError> {
    let size: Option<i64> = conn
        .query_row(
            "SELECT byte_size FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
            params![
                id.as_str(),
                scope.subject.as_str(),
                scope.namespace.as_str()
            ],
            |r| r.get(0),
        )
        .optional()
        .sql()?;
    let Some(size) = size else { return Ok(0) };
    conn.execute(
        "DELETE FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
        params![
            id.as_str(),
            scope.subject.as_str(),
            scope.namespace.as_str()
        ],
    )
    .sql()?;
    Ok(size.max(0) as u64)
}

/// Folds a new body and metadata into an existing item. Returns the byte-size
/// delta so capacity accounting stays exact.
///
/// **Scoped the same way `get` is, and for the same reason.** The lookup
/// below is `get(conn, scope, target)`, not a bare `SELECT ... WHERE id =
/// ?1`, so a merge naming an id that belongs to a different subject or
/// namespace fails with `MergeTargetMissing` exactly as if the id did not
/// exist at all — it does not reach across the scope boundary to rewrite
/// someone else's item. The `UPDATE` below repeats the same predicate rather
/// than trusting the id alone, so the write path and the read path cannot
/// drift apart on this even if a future edit changes one without the other.
pub fn merge(
    conn: &Connection,
    scope: &Scope,
    target: &ItemId,
    body: &str,
    tags: &[String],
    attrs: &std::collections::BTreeMap<String, serde_json::Value>,
    pending_embedding: bool,
) -> Result<i64, BackendError> {
    let Some(existing) = get(conn, scope, target)? else {
        return Err(BackendError::MergeTargetMissing(target.clone()));
    };
    let before = existing.byte_size() as i64;

    let mut merged_tags = existing.tags.clone();
    for t in tags {
        if !merged_tags.contains(t) {
            merged_tags.push(t.clone());
        }
    }
    let mut merged_attrs = existing.attrs.clone();
    for (k, v) in attrs {
        merged_attrs.insert(k.clone(), v.clone());
    }

    let mut updated = existing;
    updated.body = body.to_string();
    updated.tags = merged_tags;
    updated.attrs = merged_attrs;
    updated.pending_embedding = pending_embedding;
    let after = updated.byte_size() as i64;

    conn.execute(
        "UPDATE items SET body = ?4, tags = ?5, attrs = ?6, byte_size = ?7,
             pending_embedding = ?8
         WHERE id = ?1 AND subject = ?2 AND namespace = ?3",
        params![
            target.as_str(),
            scope.subject.as_str(),
            scope.namespace.as_str(),
            updated.body,
            serde_json::to_string(&updated.tags).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&updated.attrs).unwrap_or_else(|_| "{}".into()),
            after,
            i64::from(updated.pending_embedding),
        ],
    )
    .sql()?;
    Ok(after - before)
}

pub fn exists(conn: &Connection, scope: &Scope, id: &ItemId) -> Result<bool, BackendError> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id=?1 AND subject=?2 AND namespace=?3",
            params![
                id.as_str(),
                scope.subject.as_str(),
                scope.namespace.as_str()
            ],
            |r| r.get(0),
        )
        .sql()?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_core::{ItemId, MemoryItem, Protection, Scope, SensitivityLevel, Source};
    use std::collections::BTreeMap;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialise(&conn).unwrap();
        conn
    }

    fn scope(subject: &str, namespace: &str) -> Scope {
        Scope::new("t", subject, namespace).unwrap()
    }

    fn item(scope: &Scope, body: &str) -> MemoryItem {
        MemoryItem {
            id: ItemId::new(),
            scope: scope.clone(),
            body: body.to_string(),
            kind: "fact".into(),
            source: Source {
                kind: SourceKind::Agent,
                id: Some("unit".into()),
            },
            occurred_at: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            tags: vec![],
            attrs: BTreeMap::new(),
            sensitivity: SensitivityLevel::Internal,
            ttl: None,
            protection: Protection::Normal,
            pending_embedding: false,
        }
    }

    /// Every field the item columns carry must survive a write and a read.
    ///
    /// **The values are deliberately all non-default**, because `row_to_item`
    /// falls back on a default for four of them — `tags`, `attrs`,
    /// `pending_embedding` and `protection` — and a fixture built from
    /// `MemoryItem`'s own quiet values cannot tell a fallback from a real
    /// decode. `Protection::Protected` in particular is the only variant that
    /// uses the second column, so a `protection_from` that ignored
    /// `protected_until` would be invisible against `Normal` or `Pinned`.
    /// The construction, not the coverage. Three arms of these two readers were
    /// untestable for a structural reason: `fx::item` pins `SourceKind::Agent`
    /// and `Protection::Normal`, which were **exactly** what the old catch-alls
    /// returned — so the fixture's value and the fallback value were the same
    /// value, and no test built on that fixture could ever see a broken arm.
    ///
    /// Adding tests could not fix that. Removing the fallback does: with an
    /// error in its place there is no value for a fixture to coincide with.
    /// This test pins the error, so the fallback cannot come back.
    #[test]
    fn an_unwritable_enum_string_is_an_error_rather_than_a_silent_default() {
        for (column, bad) in [("source_kind", "daemon"), ("protection", "sealed")] {
            let conn = db();
            let sk = if column == "source_kind" {
                bad
            } else {
                "agent"
            };
            let pr = if column == "protection" {
                bad
            } else {
                "normal"
            };
            conn.execute(
                &format!(
                    "INSERT INTO items (id, subject, namespace, body, kind, source_kind,
                         created_at, tags, attrs, sensitivity, protection, byte_size)
                     VALUES ('01M1VHV0H1QXXT4BPAT4Z8351R', 's', 'n', 'b', 'fact', '{sk}',
                             0, '[]', '{{}}', 1, '{pr}', 3)"
                ),
                [],
            )
            .unwrap();

            let err = list(&conn, &scope("s", "n"), &Page::default()).expect_err(&format!(
                "an unknown {column} must not be read as a default"
            ));
            let text = format!("{err:?}");
            assert!(
                text.contains(bad),
                "the error must name the unreadable value; got {text}"
            );
        }
    }

    #[test]
    fn every_column_survives_a_round_trip_including_the_ones_with_defaults() {
        let conn = db();
        let s = scope("s", "n");
        let mut written = item(&s, "the body");
        written.kind = "preference".into();
        written.source = Source {
            kind: SourceKind::Human,
            id: Some("dpo-7".into()),
        };
        written.occurred_at = Some(OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap());
        written.created_at = OffsetDateTime::from_unix_timestamp(1_700_000_001).unwrap();
        written.tags = vec!["work".into(), "urgent".into()];
        written
            .attrs
            .insert("k".into(), serde_json::Value::String("v".into()));
        written.sensitivity = SensitivityLevel::Sensitive;
        written.ttl = Some(Duration::seconds(3600));
        written.protection = Protection::Protected {
            until: OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
        };
        written.pending_embedding = true;

        insert(&conn, &written).unwrap();
        let read = get(&conn, &s, &written.id).unwrap().expect("stored item");
        assert_eq!(read, written, "an item did not survive the round trip");
    }

    /// The scope predicate, on both read paths, in both dimensions. This is
    /// the property `conformance::isolation` observes through the trait; it is
    /// asserted here too because it is a *query predicate* and nothing
    /// structural enforces it — unlike the tenant, which is the filename.
    #[test]
    fn get_and_list_are_scoped_by_subject_and_by_namespace() {
        let conn = db();
        let home = scope("s", "n");
        let other_subject = scope("other-s", "n");
        let other_namespace = scope("s", "other-n");

        let mine = item(&home, "mine");
        insert(&conn, &mine).unwrap();
        insert(&conn, &item(&other_subject, "theirs")).unwrap();
        insert(&conn, &item(&other_namespace, "elsewhere")).unwrap();

        assert_eq!(list(&conn, &home, &Page::default()).unwrap().len(), 1);
        assert_eq!(
            list(&conn, &other_subject, &Page::default()).unwrap().len(),
            1
        );
        assert_eq!(
            list(&conn, &other_namespace, &Page::default())
                .unwrap()
                .len(),
            1
        );

        assert!(get(&conn, &other_subject, &mine.id).unwrap().is_none());
        assert!(get(&conn, &other_namespace, &mine.id).unwrap().is_none());
        assert!(get(&conn, &home, &mine.id).unwrap().is_some());
    }

    /// `delete` reports the charge it removed, and reports `0` — rather than
    /// erroring or deleting something — for an id that is not in the scope it
    /// was given. The second half is what stops an eviction in one namespace
    /// reaching a row in another.
    #[test]
    fn delete_returns_the_removed_charge_and_is_scoped() {
        let conn = db();
        let home = scope("s", "n");
        let elsewhere = scope("s", "other-n");
        let mine = item(&home, "a body worth some bytes");
        insert(&conn, &mine).unwrap();

        assert_eq!(
            delete(&conn, &elsewhere, &mine.id).unwrap(),
            0,
            "delete crossed a namespace boundary"
        );
        assert!(exists(&conn, &home, &mine.id).unwrap());

        assert_eq!(delete(&conn, &home, &mine.id).unwrap(), mine.byte_size());
        assert!(!exists(&conn, &home, &mine.id).unwrap());

        // And deleting what is already gone is `0`, not an error: `apply`
        // pushes an eviction into `AppliedWrite::evicted` either way.
        assert_eq!(delete(&conn, &home, &mine.id).unwrap(), 0);
    }

    /// `list` orders ascending by `created_at` with `id` as the tie-break —
    /// the total order `Backend::list` mandates. Both halves are exercised:
    /// three rows sharing one timestamp, so only the tie-break can order them,
    /// and a fourth stamped earlier, so the primary key has to outrank it.
    ///
    /// Inserted in an order that is neither the id order nor the timestamp
    /// order, so storage order cannot be mistaken for either.
    #[test]
    fn list_orders_by_created_at_then_id() {
        let conn = db();
        let s = scope("s", "n");
        let tied: Vec<ItemId> = ["01ARZ3NDEKTSV4RRFFQ69G5FA1", "01ARZ3NDEKTSV4RRFFQ69G5FA2"]
            .iter()
            .map(|i| ItemId::parse(i).unwrap())
            .collect();
        let earlier = ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FA9").unwrap();

        // Largest id first, and the earliest row last.
        for (id, at) in [
            (tied[1].clone(), 100),
            (tied[0].clone(), 100),
            (earlier.clone(), 50),
        ] {
            let mut i = item(&s, "body");
            i.id = id;
            i.created_at = OffsetDateTime::from_unix_timestamp(at).unwrap();
            insert(&conn, &i).unwrap();
        }

        let ids: Vec<ItemId> = list(&conn, &s, &Page::default())
            .unwrap()
            .into_iter()
            .map(|i| i.id)
            .collect();
        assert_eq!(
            ids,
            vec![earlier, tied[0].clone(), tied[1].clone()],
            "list must order by created_at ascending, then by id ascending"
        );
    }

    /// Paging is over the same total order, so pages are disjoint and complete
    /// even when every row shares a timestamp — the bulk-import shape
    /// `Backend::list` names.
    #[test]
    fn pages_over_a_fully_tied_corpus_are_disjoint_and_complete() {
        let conn = db();
        let s = scope("s", "n");
        let ids: Vec<ItemId> = [
            "01ARZ3NDEKTSV4RRFFQ69G5FA0",
            "01ARZ3NDEKTSV4RRFFQ69G5FA1",
            "01ARZ3NDEKTSV4RRFFQ69G5FA2",
            "01ARZ3NDEKTSV4RRFFQ69G5FA3",
        ]
        .iter()
        .map(|i| ItemId::parse(i).unwrap())
        .collect();
        for id in ids.iter().rev() {
            let mut i = item(&s, "body");
            i.id = id.clone();
            insert(&conn, &i).unwrap();
        }

        let page = |offset| {
            list(&conn, &s, &Page { offset, limit: 2 })
                .unwrap()
                .into_iter()
                .map(|i| i.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(page(0), ids[0..2].to_vec());
        assert_eq!(page(2), ids[2..4].to_vec());
        assert!(page(4).is_empty());
    }

    /// `merge` folds the new tags and attrs into the existing ones rather
    /// than replacing either wholesale, and returns the byte-size *delta* —
    /// not the old size and not the new size — so capacity accounting can
    /// apply it as a signed adjustment.
    ///
    /// Existing and supplied tags/attrs are deliberately disjoint except for
    /// one shared tag, so folding is distinguishable from either "keep the
    /// old set" (would lose `"added"`/`k-new`) or "replace with the new set"
    /// (would lose `"kept"`/`k-old`).
    #[test]
    fn merge_folds_tags_and_attrs_and_returns_the_signed_byte_delta() {
        let conn = db();
        let s = scope("s", "n");
        let mut existing = item(&s, "a short body");
        existing.tags = vec!["kept".into(), "shared".into()];
        existing
            .attrs
            .insert("k-old".into(), serde_json::Value::String("old".into()));
        insert(&conn, &existing).unwrap();
        let before = existing.byte_size() as i64;

        let new_body = "a considerably longer replacement body than the original one";
        let new_tags = vec!["shared".to_string(), "added".to_string()];
        let mut new_attrs = BTreeMap::new();
        new_attrs.insert("k-new".to_string(), serde_json::Value::String("new".into()));

        let delta = merge(
            &conn,
            &s,
            &existing.id,
            new_body,
            &new_tags,
            &new_attrs,
            false,
        )
        .unwrap();

        let stored = get(&conn, &s, &existing.id)
            .unwrap()
            .expect("merge target survives");
        assert_eq!(stored.body, new_body);
        let mut tags = stored.tags.clone();
        tags.sort();
        assert_eq!(
            tags,
            vec![
                "added".to_string(),
                "kept".to_string(),
                "shared".to_string()
            ],
            "merge must fold tags rather than replacing or dropping either side"
        );
        assert_eq!(
            stored.attrs.get("k-old").and_then(|v| v.as_str()),
            Some("old"),
            "merge dropped an attr the target already had"
        );
        assert_eq!(
            stored.attrs.get("k-new").and_then(|v| v.as_str()),
            Some("new"),
            "merge dropped an attr it was supplied"
        );

        let after = stored.byte_size() as i64;
        assert_eq!(
            delta,
            after - before,
            "merge must return the signed byte-size delta, not the old or \
             new size alone"
        );
        assert!(delta > 0, "the premise: the replacement body is longer");

        // `stored.byte_size()` above is *recomputed* by `MemoryItem::byte_size`
        // from the row's body/tags/attrs — `MemoryItem` has no `byte_size`
        // field at all — so it cannot see what `merge`'s own `UPDATE` wrote
        // into the `items.byte_size` column. Read that column back directly:
        // `items::delete` charges an eviction from exactly this column, so a
        // merge that computes the right delta but persists the pre-merge
        // size here is a capacity leak that only shows up later, when the
        // item is evicted for less than it is actually costing the budget.
        let stored_byte_size: i64 = conn
            .query_row(
                "SELECT byte_size FROM items WHERE id = ?1",
                params![existing.id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            stored_byte_size, after,
            "merge must persist the item's new byte_size, not the pre-merge \
             one — a later eviction reads this column, not the delta merge \
             returned"
        );
    }

    /// `merge`'s own `UPDATE` did not touch `pending_embedding` until this
    /// test's premise: `insert` writes the column (see
    /// `every_column_survives_a_round_trip_including_the_ones_with_defaults`
    /// above) but `merge` skipped it entirely, so a merge whose re-embed
    /// failed had nowhere to record that fact — the flag stayed at whatever
    /// the target's *pre-merge* value happened to be, forever, no matter what
    /// `merge`'s caller passed. Both directions are exercised, mirroring
    /// `MergeWrite::pending_embedding`'s own doc: a merge can both set the
    /// flag (target was clean, re-embed failed) and clear a stale one
    /// (target was pending, re-embed succeeded) — either direction being
    /// silently ignored would still leave every other assertion in this
    /// module passing.
    #[test]
    fn merge_persists_the_pending_embedding_flag_in_both_directions() {
        let conn = db();
        let s = scope("s", "n");

        let mut clean = item(&s, "a clean target");
        clean.pending_embedding = false;
        insert(&conn, &clean).unwrap();
        merge(
            &conn,
            &s,
            &clean.id,
            "new body",
            &[],
            &BTreeMap::new(),
            true,
        )
        .unwrap();
        let after_set = get(&conn, &s, &clean.id).unwrap().unwrap();
        assert!(
            after_set.pending_embedding,
            "merge must be able to SET pending_embedding, not just leave it \
             at the target's pre-merge value"
        );

        let mut stale = item(&s, "a stale target");
        stale.pending_embedding = true;
        insert(&conn, &stale).unwrap();
        merge(
            &conn,
            &s,
            &stale.id,
            "new body",
            &[],
            &BTreeMap::new(),
            false,
        )
        .unwrap();
        let after_clear = get(&conn, &s, &stale.id).unwrap().unwrap();
        assert!(
            !after_clear.pending_embedding,
            "merge must be able to CLEAR a stale pending_embedding, not just \
             leave it at the target's pre-merge value"
        );
    }

    /// A merge whose supplied tag already exists on the target must not
    /// create a duplicate.
    #[test]
    fn merge_does_not_duplicate_a_tag_the_target_already_has() {
        let conn = db();
        let s = scope("s", "n");
        let mut existing = item(&s, "body");
        existing.tags = vec!["shared".into()];
        insert(&conn, &existing).unwrap();

        merge(
            &conn,
            &s,
            &existing.id,
            "body",
            &["shared".to_string()],
            &BTreeMap::new(),
            false,
        )
        .unwrap();

        let stored = get(&conn, &s, &existing.id).unwrap().unwrap();
        assert_eq!(stored.tags, vec!["shared".to_string()]);
    }

    /// A shrinking merge returns a *negative* delta — the case that tells
    /// apart "returns the delta" from "returns the new size", which would
    /// also be positive here and pass a test that only ever grows the body.
    #[test]
    fn merge_returns_a_negative_delta_when_the_body_shrinks() {
        let conn = db();
        let s = scope("s", "n");
        let existing = item(&s, "a body long enough to shrink meaningfully");
        insert(&conn, &existing).unwrap();
        let before = existing.byte_size() as i64;

        let delta = merge(
            &conn,
            &s,
            &existing.id,
            "short",
            &[],
            &BTreeMap::new(),
            false,
        )
        .unwrap();
        let stored = get(&conn, &s, &existing.id).unwrap().unwrap();
        let after = stored.byte_size() as i64;

        assert!(delta < 0, "expected a negative delta, got {delta}");
        assert_eq!(delta, after - before);
    }

    /// `merge` cannot reach a target in a different subject or namespace: it
    /// reports `MergeTargetMissing`, the same as if the id did not exist at
    /// all, because the lookup goes through the scoped `get` rather than a
    /// bare `SELECT ... WHERE id = ?1`.
    #[test]
    fn merge_target_lookup_is_scoped_by_subject_and_namespace() {
        let conn = db();
        let home = scope("s", "n");
        let elsewhere = scope("other-s", "n");
        let target = item(&elsewhere, "not home's item");
        insert(&conn, &target).unwrap();

        let err = merge(
            &conn,
            &home,
            &target.id,
            "stolen body",
            &[],
            &BTreeMap::new(),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(&err, BackendError::MergeTargetMissing(id) if *id == target.id),
            "expected MergeTargetMissing, got {err:?}"
        );

        let survivor = get(&conn, &elsewhere, &target.id)
            .unwrap()
            .expect("target untouched");
        assert_eq!(
            survivor.body, target.body,
            "a cross-scope merge rewrote the target"
        );
    }

    /// A target that genuinely does not exist reports the same error as one
    /// that exists in a different scope — a caller cannot distinguish "wrong
    /// scope" from "never existed" from the error alone, which is the point:
    /// neither should leak whether a foreign id is in use.
    #[test]
    fn merge_target_missing_entirely_reports_merge_target_missing() {
        let conn = db();
        let s = scope("s", "n");
        let ghost = ItemId::new();
        let err = merge(&conn, &s, &ghost, "body", &[], &BTreeMap::new(), false).unwrap_err();
        assert!(matches!(&err, BackendError::MergeTargetMissing(id) if *id == ghost));
    }
}
