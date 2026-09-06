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

fn source_kind_from(s: &str) -> SourceKind {
    match s {
        "session" => SourceKind::Session,
        "tool" => SourceKind::Tool,
        "human" => SourceKind::Human,
        _ => SourceKind::Agent,
    }
}

fn protection_parts(p: Protection) -> (&'static str, Option<i64>) {
    match p {
        Protection::Normal => ("normal", None),
        Protection::Pinned => ("pinned", None),
        Protection::Protected { until } => ("protected", Some(until.unix_timestamp())),
    }
}

fn protection_from(kind: &str, until: Option<i64>) -> Protection {
    match kind {
        "pinned" => Protection::Pinned,
        "protected" => match until.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()) {
            Some(until) => Protection::Protected { until },
            None => Protection::Normal,
        },
        _ => Protection::Normal,
    }
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
        id: ItemId::parse(&row.get::<_, String>("id")?).expect("stored ids are valid ULIDs"),
        scope: Scope::new(tenant, &subject, &namespace).expect("stored scopes are valid"),
        body: row.get("body")?,
        kind: row.get("kind")?,
        source: Source {
            kind: source_kind_from(&row.get::<_, String>("source_kind")?),
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
        protection: protection_from(&protection, protected_until),
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
}
