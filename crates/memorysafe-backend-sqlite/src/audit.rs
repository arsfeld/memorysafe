//! The `audit` detail table.
//!
//! Rows carry ids, BLAKE3 digests and feature numbers — never item bodies.
//! The `policy` column is a rendered display convenience and is deliberately
//! **not** a key: `PolicyId`'s `Display` is not injective, so anything that
//! grouped on it would merge distinct policies. The aggregate table keys on the
//! two parts instead; see [`crate::aggregates`].
//!
//! **Every insert here is one half of a pair.** The other half is
//! `aggregates::increment`, in the same transaction. The rule is stated once,
//! on `memorysafe_backend::aggregates`, and binds every audit-writing path.

use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{AuditEvent, AuditFilter, AuditId, AuditRecord, Scope};
use rusqlite::{Connection, params};

/// The inverse of `AuditEvent::as_str`. A storage error rather than a panic:
/// a row holding a string outside every variant's rendered name means the row
/// is corrupt or was written by something other than this crate, and this
/// runs inside `with_conn`/`with_write` closures where a panic poisons the
/// tenant's connection (see `tenant`'s module doc). Used by
/// `aggregates::query`, the read half of the aggregate table, which is the
/// only place this crate needs to go from a stored event name back to an
/// `AuditEvent`.
pub(crate) fn event_from_str(s: &str) -> Result<AuditEvent, BackendError> {
    serde_json::from_str(&format!("\"{s}\"")).map_err(|e| BackendError::Storage {
        message: format!("stored audit event {s:?} does not match any AuditEvent: {e}"),
        retryable: false,
    })
}

pub fn insert(conn: &Connection, record: &AuditRecord) -> Result<AuditId, BackendError> {
    let policy = record.decision.as_ref().map(|d| d.policy.to_string());
    conn.execute(
        "INSERT INTO audit (id, at, subject, namespace, event, items, assessment, decision,
             actor, policy)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            record.id.as_str(),
            record.at.unix_timestamp(),
            record.scope.subject.as_str(),
            record.scope.namespace.as_str(),
            // `as_str()`, not a re-derivation from serde. `AuditEvent::as_str`
            // is documented as the one string that is simultaneously the serde
            // form, the value a backend stores and the key aggregates sort by
            // — and `aggregates::increment` keys on exactly this call, in this
            // same transaction. Deriving the column two different ways would
            // let the detail row and its aggregate disagree about the event.
            record.event.as_str(),
            serde_json::to_string(&record.items).unwrap_or_else(|_| "[]".into()),
            record
                .assessment
                .as_ref()
                .map(|a| serde_json::to_string(a).unwrap_or_default()),
            record
                .decision
                .as_ref()
                .map(|d| serde_json::to_string(d).unwrap_or_default()),
            serde_json::to_string(&record.actor).unwrap_or_default(),
            policy,
        ],
    )
    .sql()?;
    Ok(record.id.clone())
}

/// A stored value the writer could not have produced. Returning this rather
/// than panicking matters more here than the wording suggests: a panic inside
/// a `with_conn` closure poisons that tenant's connection mutex — see
/// `TenantManager` — so one corrupt row takes the tenant down until the pool
/// heals, where an error returns to the caller and leaves it working.
fn unreadable(column: &str, detail: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::other(format!(
            "audit.{column} holds {detail}, which no writer in this crate emits"
        ))),
    )
}

pub fn query(
    conn: &Connection,
    scope: &Scope,
    filter: &AuditFilter,
) -> Result<Vec<AuditRecord>, BackendError> {
    let mut sql = String::from(
        "SELECT id, at, subject, namespace, event, items, assessment, decision, actor
         FROM audit WHERE subject = ?1 AND namespace = ?2",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(scope.subject.as_str().to_string()),
        Box::new(scope.namespace.as_str().to_string()),
    ];

    if !filter.events.is_empty() {
        let names: Vec<&'static str> = filter.events.iter().map(|e| e.as_str()).collect();
        let placeholders = (0..names.len())
            .map(|i| format!("?{}", args.len() + i + 1))
            .collect::<Vec<_>>();
        sql.push_str(&format!(" AND event IN ({})", placeholders.join(",")));
        for n in names {
            args.push(Box::new(n.to_string()));
        }
    }
    // `filter.item`, when set, narrows to records whose `items` column (a
    // JSON array of `ItemRef { id, digest }` objects — see the module doc)
    // contains an element whose `id` matches. This must stay a SQL predicate
    // ANDed in here, before `ORDER BY ... LIMIT` is appended below, rather
    // than a Rust-side filter applied to the fetched rows: `limit` is
    // documented on `AuditFilter` as `min(limit, rows matching the WHOLE
    // filter)`, and a caller relies on `returned.len() < limit` as the only
    // signal the log is exhausted (there is deliberately no `truncated`
    // flag). Filtering after an item-blind `LIMIT` would satisfy a
    // `len()`-only test while quietly returning fewer than `min(limit,
    // matching)` rows.
    //
    // Matches the parsed `id` field specifically via `json_extract`, not a
    // `LIKE` over the raw column: an `ItemId` is a 26-character ULID and a
    // `digest` is BLAKE3 hex, so a substring match could in principle match
    // the wrong field, or a prefix of a longer id. Follows the
    // `json_each`-over-a-JSON-array precedent in `retrieve.rs`'s
    // `filter_sql`, adapted for an array of objects rather than of strings.
    if let Some(item_id) = &filter.item {
        args.push(Box::new(item_id.as_str().to_string()));
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each(items) \
               WHERE json_extract(json_each.value, '$.id') = ?{})",
            args.len()
        ));
    }
    if let Some(since) = filter.since {
        args.push(Box::new(since.unix_timestamp()));
        sql.push_str(&format!(" AND at >= ?{}", args.len()));
    }
    if let Some(until) = filter.until {
        args.push(Box::new(until.unix_timestamp()));
        sql.push_str(&format!(" AND at <= ?{}", args.len()));
    }
    // The cursor. Rows page **descending** by `id`, so `after` — the last id
    // of the previous page — excludes everything at or above it: `id <
    // after`, strictly. `<=` would re-serve the cursor row itself on the next
    // page, and `>` would walk the log backwards.
    if let Some(after) = &filter.after {
        args.push(Box::new(after.as_str().to_string()));
        sql.push_str(&format!(" AND id < ?{}", args.len()));
    }
    // Ordered by id, descending (newest first) — see `AuditFilter::after`'s
    // doc comment in memorysafe-core: `at` is whole seconds and cannot
    // separate rows written in the same second, so `id` alone is the total
    // order, not a tie-break on `at`.
    //
    // Spelled without an explicit collation so it matches the form
    // `schema::tests::the_audit_index_serves_the_id_ordering_the_contract_mandates`
    // proves `idx_audit_scope_id` serves without a sort. The index states
    // `COLLATE BINARY` itself, which is also the column's default.
    args.push(Box::new(filter.limit as i64));
    sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), move |r| {
            let subject: String = r.get("subject")?;
            let namespace: String = r.get("namespace")?;
            let event_str: String = r.get("event")?;
            let items_json: String = r.get("items")?;
            let assessment: Option<String> = r.get("assessment")?;
            let decision: Option<String> = r.get("decision")?;
            let actor_json: String = r.get("actor")?;
            let at: i64 = r.get("at")?;
            Ok(AuditRecord {
                id: AuditId::parse(&r.get::<_, String>("id")?)
                    .map_err(|e| unreadable("id", e.to_string()))?,
                at: time::OffsetDateTime::from_unix_timestamp(at)
                    .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
                scope: Scope::new(&tenant, &subject, &namespace)
                    .map_err(|e| unreadable("scope", e.to_string()))?,
                event: serde_json::from_str(&format!("\"{event_str}\""))
                    .map_err(|e| unreadable("event", e.to_string()))?,
                items: serde_json::from_str(&items_json).unwrap_or_default(),
                assessment: assessment.and_then(|s| serde_json::from_str(&s).ok()),
                decision: decision.and_then(|s| serde_json::from_str(&s).ok()),
                actor: serde_json::from_str(&actor_json)
                    .map_err(|e| unreadable("actor", e.to_string()))?,
            })
        })
        .sql()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().sql()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_core::{
        Actor, ActorKind, AuditEvent, AuditRecord, Decision, ItemId, ItemRef, MemoryItem, PolicyId,
        Protection, Reason, ReasonCode, Scope, SensitivityLevel, Source, SourceKind,
    };
    use time::OffsetDateTime;

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

    fn record(scope: &Scope, event: AuditEvent, at: i64) -> AuditRecord {
        AuditRecord::new(
            scope.clone(),
            event,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(at).unwrap(),
        )
    }

    /// The `policy` column has no reader anywhere in this crate — `query`'s
    /// `SELECT` omits it, and the export that reads it arrives with the
    /// portability task. Until then this is the only thing in the workspace
    /// that fails if `insert` binds `None` there or renders the wrong part of
    /// the `PolicyId`, so it is written against the exact string rather than
    /// against `Display` re-derived at assertion time.
    #[test]
    fn the_policy_column_renders_the_decision_and_is_null_without_one() {
        let conn = db();
        let sc = scope("s", "n");

        let mut policied = record(&sc, AuditEvent::Rejected, 1);
        policied.decision = Some(Decision::reject(
            PolicyId::new("retention", "3"),
            Reason::new(ReasonCode::LowValue, "", Default::default()),
        ));
        insert(&conn, &policied).unwrap();

        let plain = record(&sc, AuditEvent::Admitted, 2);
        insert(&conn, &plain).unwrap();

        let policy_of = |id: &AuditId| -> Option<String> {
            conn.query_row(
                "SELECT policy FROM audit WHERE id = ?1",
                params![id.as_str()],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            policy_of(&policied.id).as_deref(),
            Some("retention@3"),
            "the rendered policy is missing or mis-rendered"
        );
        assert_eq!(
            policy_of(&plain.id),
            None,
            "a record carrying no decision must leave the column NULL, not an \
             empty string — NULL is what the export will read as `no policy`"
        );
    }

    /// The echo rule at the storage layer: the row comes back under the id it
    /// was written with, carrying the event, actor and item refs it was given.
    ///
    /// The actor is deliberately **not** `Actor::system()`, which is what a
    /// backend inventing a row would most plausibly write, and the record
    /// carries an `ItemRef` so the `items` column is exercised with something
    /// other than an empty array.
    #[test]
    fn a_record_round_trips_under_the_id_it_was_given() {
        let conn = db();
        let s = scope("s", "n");
        let subject_item = item(&s, "a body that must never be stored here");
        let mut rec = record(&s, AuditEvent::Admitted, 1_700_000_000);
        rec.items = vec![ItemRef::from_item(&subject_item)];
        rec.actor = Actor {
            kind: ActorKind::Human,
            id: Some("dpo-7".into()),
        };

        let returned = insert(&conn, &rec).unwrap();
        assert_eq!(returned, rec.id, "insert did not echo the id it was given");

        let back = query(&conn, &s, &AuditFilter::default()).unwrap();
        assert_eq!(back, vec![rec.clone()], "the row did not survive intact");

        // And the body is not in the table. Audit rows carry ids, digests and
        // feature numbers — never item bodies.
        let dump: String = conn
            .query_row(
                "SELECT group_concat(id || at || event || items || actor) FROM audit",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !dump.contains("a body that must never be stored here"),
            "an item body reached the audit table: {dump}"
        );
    }

    /// The scope predicate. Subject and namespace are each varied on their
    /// own, so a query carrying one and not the other is caught.
    #[test]
    fn a_query_is_scoped_by_subject_and_by_namespace() {
        let conn = db();
        let home = scope("s", "n");
        insert(&conn, &record(&home, AuditEvent::Admitted, 10)).unwrap();
        insert(
            &conn,
            &record(&scope("other-s", "n"), AuditEvent::Admitted, 10),
        )
        .unwrap();
        insert(
            &conn,
            &record(&scope("s", "other-n"), AuditEvent::Admitted, 10),
        )
        .unwrap();

        assert_eq!(
            query(&conn, &home, &AuditFilter::default()).unwrap().len(),
            1
        );
        assert_eq!(
            query(&conn, &scope("other-s", "n"), &AuditFilter::default())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            query(&conn, &scope("s", "other-n"), &AuditFilter::default())
                .unwrap()
                .len(),
            1
        );
    }

    /// Rows come back newest-first by `AuditId`, and the id order is what
    /// decides — not `at`. The corpus makes the two disagree: the row with the
    /// largest id carries the *earliest* timestamp, so a backend ordering by
    /// `at` returns the reverse.
    #[test]
    fn rows_are_ordered_by_id_descending_and_not_by_at() {
        let conn = db();
        let s = scope("s", "n");
        let ids = [
            "01ARZ3NDEKTSV4RRFFQ69G5FB0",
            "01ARZ3NDEKTSV4RRFFQ69G5FB1",
            "01ARZ3NDEKTSV4RRFFQ69G5FB2",
        ];
        // Ascending id, descending `at`.
        for (i, id) in ids.iter().enumerate() {
            let mut rec = record(&s, AuditEvent::Admitted, 100 - i as i64);
            rec.id = AuditId::parse(id).unwrap();
            insert(&conn, &rec).unwrap();
        }

        let got: Vec<String> = query(&conn, &s, &AuditFilter::default())
            .unwrap()
            .into_iter()
            .map(|r| r.id.as_str().to_string())
            .collect();
        assert_eq!(
            got,
            vec![ids[2].to_string(), ids[1].to_string(), ids[0].to_string()],
            "rows must be ordered by AuditId descending; this corpus makes `at` \
             order disagree, so an `at`-ordered backend returns the reverse"
        );
    }

    /// The three filter clauses the query builder assembles, each narrowing
    /// and each leaving the others alone. The placeholder numbering is the
    /// thing at risk: the clauses are appended conditionally, so an off-by-one
    /// in the index arithmetic binds a value to the wrong slot.
    #[test]
    fn events_since_and_until_narrow_together_without_misbinding_a_placeholder() {
        let conn = db();
        let s = scope("s", "n");
        insert(&conn, &record(&s, AuditEvent::Admitted, 100)).unwrap();
        insert(&conn, &record(&s, AuditEvent::Forgotten, 200)).unwrap();
        insert(&conn, &record(&s, AuditEvent::Admitted, 300)).unwrap();
        insert(&conn, &record(&s, AuditEvent::Recalled, 400)).unwrap();

        let only_admits = AuditFilter {
            events: vec![AuditEvent::Admitted],
            ..Default::default()
        };
        assert_eq!(query(&conn, &s, &only_admits).unwrap().len(), 2);

        let two_events = AuditFilter {
            events: vec![AuditEvent::Admitted, AuditEvent::Recalled],
            ..Default::default()
        };
        assert_eq!(query(&conn, &s, &two_events).unwrap().len(), 3);

        // Bounds are inclusive on both edges, per `Backend::audit`.
        let window = AuditFilter {
            since: Some(OffsetDateTime::from_unix_timestamp(200).unwrap()),
            until: Some(OffsetDateTime::from_unix_timestamp(300).unwrap()),
            ..Default::default()
        };
        assert_eq!(query(&conn, &s, &window).unwrap().len(), 2);

        // All three at once: the events clause consumes placeholders ?3..?4,
        // so `since` must land on ?5 and `until` on ?6. A builder that
        // numbered them from a fixed base binds the timestamps to the event
        // slots and returns nothing.
        let all_three = AuditFilter {
            events: vec![AuditEvent::Admitted, AuditEvent::Forgotten],
            since: Some(OffsetDateTime::from_unix_timestamp(150).unwrap()),
            until: Some(OffsetDateTime::from_unix_timestamp(350).unwrap()),
            ..Default::default()
        };
        let got = query(&conn, &s, &all_three).unwrap();
        assert_eq!(
            got.len(),
            2,
            "expected the eviction at 200 and the admit at 300"
        );
        assert!(got.iter().all(|r| r.at.unix_timestamp() >= 150));

        // And `limit` truncates rather than being ignored.
        let capped = AuditFilter {
            limit: 1,
            ..Default::default()
        };
        assert_eq!(query(&conn, &s, &capped).unwrap().len(), 1);
    }

    /// `filter.item` narrows to records whose `items` column references that
    /// `ItemId`, and only those. Two records, each referencing a distinct
    /// item, plus a third record referencing no item at all (an eviction,
    /// the same shape `record`/`insert` produce when `items` is left `vec![]`)
    /// — asserted in both directions, on the exact id returned rather than a
    /// count, so a backend that ignores `filter.item` (returns all three) or
    /// one that treats `item: Some(_)` as merely "items is non-empty"
    /// (returns the item-referencing pair but not the phantom) both fail
    /// here.
    #[test]
    fn filter_item_narrows_to_records_referencing_that_item() {
        let conn = db();
        let s = scope("s", "n");
        let item_a = item(&s, "a body");
        let item_b = item(&s, "b body");

        let mut rec_a = record(&s, AuditEvent::Admitted, 100);
        rec_a.items = vec![ItemRef::from_item(&item_a)];
        insert(&conn, &rec_a).unwrap();

        let mut rec_b = record(&s, AuditEvent::Admitted, 200);
        rec_b.items = vec![ItemRef::from_item(&item_b)];
        insert(&conn, &rec_b).unwrap();

        // A phantom eviction: no item reference at all.
        let phantom = record(&s, AuditEvent::Forgotten, 300);
        insert(&conn, &phantom).unwrap();

        let for_a = query(
            &conn,
            &s,
            &AuditFilter {
                item: Some(item_a.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            for_a.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec![rec_a.id.clone()],
            "filtering by item A's id must return exactly A's record, not B's \
             or the phantom's"
        );

        let for_b = query(
            &conn,
            &s,
            &AuditFilter {
                item: Some(item_b.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            for_b.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec![rec_b.id.clone()],
            "filtering by item B's id must return exactly B's record, not A's \
             or the phantom's"
        );
    }

    /// The residual left open by the conformance suite: it only ever queries
    /// `item` against `AuditFilter::default()`, whose `limit` is 100, so it
    /// never combines `item` with a constraining `limit` and — since nothing
    /// implemented the field before this change — could not have. Per
    /// `Backend::audit`'s contract, `item` is applied BEFORE `limit`, so
    /// `limit` means `min(limit, rows matching the WHOLE filter)`.
    ///
    /// A backend that filters a fetched page in Rust *after* an
    /// item-blind `SQL LIMIT` would satisfy `filter_item_narrows_to_records_
    /// referencing_that_item` above (that test's `limit` is the default 100,
    /// comfortably wider than its three-row corpus) while still breaking the
    /// contract here: `AuditFilter::limit`'s doc makes `returned.len() <
    /// limit` the *only* signal the log is exhausted, so a caller fed a short
    /// page for the wrong reason wrongly concludes it reached the end.
    ///
    /// The corpus interleaves matching and non-matching rows by id
    /// (newest-first is the row order) so that the newest `limit` rows,
    /// taken WITHOUT the item predicate, are *not* the same set as the
    /// newest `limit` rows that reference item A: id5/id3/id1 (the three
    /// newest, unfiltered) reference item B, while item A is on id4/id2/id0.
    /// A page-then-filter backend intersects those two sets and returns only
    /// id4 (one row); the correct SQL-first ordering returns id4, id2, id0
    /// (three rows) — `min(limit=3, matching=3)`.
    #[test]
    fn filter_item_is_applied_before_limit_not_after() {
        let conn = db();
        let s = scope("s", "n");
        let item_a = item(&s, "a body");
        let item_b = item(&s, "b body");

        let ids = [
            "01ARZ3NDEKTSV4RRFFQ69G5FB0", // oldest
            "01ARZ3NDEKTSV4RRFFQ69G5FB1",
            "01ARZ3NDEKTSV4RRFFQ69G5FB2",
            "01ARZ3NDEKTSV4RRFFQ69G5FB3",
            "01ARZ3NDEKTSV4RRFFQ69G5FB4",
            "01ARZ3NDEKTSV4RRFFQ69G5FB5", // newest
        ];
        // Even indices (0, 2, 4) reference item A; odd indices (1, 3, 5)
        // reference item B. Three rows match `item: Some(item_a)`.
        for (i, id) in ids.iter().enumerate() {
            let mut rec = record(&s, AuditEvent::Admitted, 100 + i as i64);
            rec.id = AuditId::parse(id).unwrap();
            rec.items = vec![ItemRef::from_item(if i % 2 == 0 {
                &item_a
            } else {
                &item_b
            })];
            insert(&conn, &rec).unwrap();
        }

        let got: Vec<String> = query(
            &conn,
            &s,
            &AuditFilter {
                item: Some(item_a.id.clone()),
                limit: 3,
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|r| r.id.as_str().to_string())
        .collect();

        assert_eq!(
            got,
            vec![ids[4].to_string(), ids[2].to_string(), ids[0].to_string()],
            "limit=3 with item=A must return all three item-A rows \
             (min(limit, matching) = 3), newest first — a backend that cuts \
             the SQL LIMIT before filtering by item would instead intersect \
             the newest 3 rows overall ({}, {}, {}, only one of which is \
             item A) with the item filter and return just one row",
            ids[5],
            ids[4],
            ids[3],
        );
    }
}
