use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 1;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS items (
  id                TEXT PRIMARY KEY,
  subject           TEXT NOT NULL,
  namespace         TEXT NOT NULL,
  body              TEXT NOT NULL,
  kind              TEXT NOT NULL,
  source_kind       TEXT NOT NULL,
  source_id         TEXT,
  occurred_at       INTEGER,
  created_at        INTEGER NOT NULL,
  tags              TEXT NOT NULL,
  attrs             TEXT NOT NULL,
  sensitivity       INTEGER NOT NULL,
  ttl_seconds       INTEGER,
  protection        TEXT NOT NULL,
  protected_until   INTEGER,
  value_score       REAL NOT NULL DEFAULT 0.0,
  fragility_score   REAL NOT NULL DEFAULT 0.0,
  byte_size         INTEGER NOT NULL,
  last_access       INTEGER,
  access_count      INTEGER NOT NULL DEFAULT 0,
  pending_embedding INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_items_scope      ON items(subject, namespace);
CREATE INDEX IF NOT EXISTS idx_items_scope_sens ON items(subject, namespace, sensitivity);

CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  body, tags, content='items', content_rowid='rowid', tokenize='unicode61'
);
CREATE TRIGGER IF NOT EXISTS items_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;
CREATE TRIGGER IF NOT EXISTS items_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
END;
-- Guarded on the indexed columns. `record_recall` bumps `last_access` and
-- `access_count` for every item a recall returned, in the same transaction as
-- the audit row; an unguarded AFTER UPDATE trigger would make every recall a
-- full FTS delete-and-reinsert per item returned, none of whose indexed
-- columns changed — write amplification on the read path, inside the tenant
-- write lock. `IS NOT` rather than `<>` so NULLs compare correctly; neither
-- column is nullable today, and the operator must not depend on that.
CREATE TRIGGER IF NOT EXISTS items_au AFTER UPDATE ON items
WHEN old.body IS NOT new.body OR old.tags IS NOT new.tags
BEGIN
  INSERT INTO items_fts(items_fts, rowid, body, tags)
    VALUES('delete', old.rowid, old.body, old.tags);
  INSERT INTO items_fts(rowid, body, tags) VALUES (new.rowid, new.body, new.tags);
END;
-- `items_fts` is an external-content table keyed on `items.rowid`, and `items`
-- has no INTEGER PRIMARY KEY, so VACUUM may renumber those rowids and leave
-- the index pointing at the wrong rows. Do not VACUUM a tenant database
-- without rebuilding the index afterwards
-- (INSERT INTO items_fts(items_fts) VALUES('rebuild')).

CREATE TABLE IF NOT EXISTS vectors (
  item_id   TEXT PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  subject   TEXT NOT NULL,
  namespace TEXT NOT NULL,
  embedder  TEXT NOT NULL,
  dim       INTEGER NOT NULL,
  scale     REAL NOT NULL,
  q         BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_vectors_scope ON vectors(subject, namespace, embedder, dim);

CREATE TABLE IF NOT EXISTS capacity (
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  max_items  INTEGER,
  max_bytes  INTEGER,
  used_items INTEGER NOT NULL DEFAULT 0,
  used_bytes INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (subject, namespace)
);

CREATE TABLE IF NOT EXISTS audit (
  id         TEXT PRIMARY KEY,
  at         INTEGER NOT NULL,
  subject    TEXT NOT NULL,
  namespace  TEXT NOT NULL,
  event      TEXT NOT NULL,
  items      TEXT NOT NULL,
  assessment TEXT,
  decision   TEXT,
  actor      TEXT NOT NULL,
  policy     TEXT
);
-- Two indexes, because `Backend::audit` has two query shapes and they order by
-- different keys. `AuditRecord::new` mints `id` at construction while taking
-- `at` as a parameter, so the two orders genuinely diverge and neither index
-- substitutes for the other.
--
-- The contract orders by `AuditId` descending and pages on `id < after`
-- (strictly smaller), so the cursor page is
--   WHERE subject=? AND namespace=? [AND at BETWEEN ? AND ?] AND id < ?
--   ORDER BY id DESC LIMIT n
-- and within one scope `idx_audit_scope_at` is ordered by `at ASC, id DESC` —
-- it cannot serve that ORDER BY, so SQLite sorts the whole scope, on a table
-- that grows without bound and is queried for compliance.
-- `schema::tests::the_audit_index_serves_the_id_ordering_the_contract_mandates`
-- reads the query plan and fails if the sort comes back.
--
-- `COLLATE BINARY` is stated for the same reason it is stated on the aggregate
-- ordering index: `AuditId` is a ULID compared as text, and the collation an
-- ordering runs under should be visible rather than inherited.
CREATE INDEX IF NOT EXISTS idx_audit_scope_id
  ON audit(subject, namespace, id COLLATE BINARY DESC);
-- Kept for the time-window filters, which the id index cannot serve.
CREATE INDEX IF NOT EXISTS idx_audit_scope_at ON audit(subject, namespace, at, id DESC);

CREATE TABLE IF NOT EXISTS idempotency (
  key            TEXT PRIMARY KEY,
  subject        TEXT NOT NULL,
  namespace      TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  outcome        TEXT NOT NULL,
  at             INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_aggregates (
  -- The key, and nothing finer. No subject column and no namespace column:
  -- `memorysafe_backend::aggregates` carries the argument, and
  -- `lifecycle::audit_aggregates_survive_a_cascading_purge` fails if a purge
  -- can reach these rows.
  --
  -- The policy is two columns because `PolicyId`'s Display is not injective;
  -- a rendered key column merges distinct policies into one row. See the
  -- prose below.
  policy_name    TEXT,                        -- NULL together with policy_version
  policy_version TEXT,
  event          TEXT NOT NULL,               -- AuditEvent::as_str(), never an ordinal
  day            INTEGER NOT NULL,            -- whole UTC days, aggregates::day_bucket
  count          INTEGER NOT NULL,
  value_histogram     TEXT NOT NULL,          -- JSON [u64; 10]
  fragility_histogram TEXT NOT NULL,          -- JSON [u64; 10]
  histogram_version   INTEGER NOT NULL,
  -- The two policy columns are NULL together or set together. Without this a
  -- row like ('x', NULL, 'admitted', 1) is representable, falls inside the
  -- policied partial index — whose predicate tests only `policy_name` — and
  -- `aggregates::query`'s `(Some(n), Some(v)) => Some(..), _ => None` would
  -- silently relabel it as policy-less: a wrong aggregate that looks
  -- well-formed. The invariant was a comment; this makes it a constraint.
  CHECK ((policy_name IS NULL) = (policy_version IS NULL))
);
-- Uniqueness in two partial indexes rather than one PRIMARY KEY over the
-- nullable tuple. A unique index treats NULLs as distinct from each other, so
-- a single index over (policy_name, policy_version, event, day) would let two
-- policy-less rows with the same event and day both insert — and policy-less
-- rows are the majority of the key space, not a corner. Splitting on the
-- nullability makes the constraint hold in both cases without inventing a
-- sentinel policy string, which `AggregateKey::policy`'s doc rules out.
CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policied
  ON audit_aggregates(policy_name, policy_version, event, day)
  WHERE policy_name IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aggregates_key_policy_less
  ON audit_aggregates(event, day)
  WHERE policy_name IS NULL;
-- The read path's ordering index, in the documented key order: day first, then
-- the policy parts, then the event name. Collation is stated on every text
-- column rather than left to the engine's default, per the mandate on
-- `Backend::audit_aggregates`; `COLLATE BINARY` is SQLite's spelling of byte
-- order and Postgres's is `COLLATE "C"`.
CREATE INDEX IF NOT EXISTS idx_aggregates_order ON audit_aggregates(
  day,
  policy_name    COLLATE BINARY,
  policy_version COLLATE BINARY,
  event          COLLATE BINARY
);
"#;

/// Applies pragmas and DDL. Idempotent — safe on every open.
pub fn initialise(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.execute_batch(DDL)?;
    conn.execute(
        "INSERT INTO meta(key, value) VALUES('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialise(&conn).unwrap();
        conn
    }

    /// Inserts one aggregate row. `policy` is the `(name, version)` pair, or
    /// `None` for the policy-less rows that `AggregateKey::policy`'s doc calls
    /// the majority of the key space.
    fn insert_aggregate(
        conn: &Connection,
        policy: Option<(&str, &str)>,
        event: &str,
        day: i64,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO audit_aggregates
               (policy_name, policy_version, event, day, count,
                value_histogram, fragility_histogram, histogram_version)
             VALUES (?1, ?2, ?3, ?4, 1, '[0,0,0,0,0,0,0,0,0,0]', '[0,0,0,0,0,0,0,0,0,0]', 1)",
            rusqlite::params![policy.map(|p| p.0), policy.map(|p| p.1), event, day],
        )
    }

    fn insert_item(conn: &Connection, id: &str, body: &str, tags: &str) {
        conn.execute(
            "INSERT INTO items
               (id, subject, namespace, body, kind, source_kind, created_at,
                tags, attrs, sensitivity, protection, byte_size)
             VALUES (?1, 's', 'n', ?2, 'note', 'user', 0, ?3, '{}', 0, 'none', 10)",
            rusqlite::params![id, body, tags],
        )
        .unwrap();
    }

    fn plan(conn: &Connection, sql: &str) -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.join("\n")
    }

    /// Family A, tested against `policy: None` specifically rather than against
    /// a representative row — a representative row has a policy and proves
    /// nothing about most of the table. A single unique index over the nullable
    /// tuple treats every NULL as distinct from every other NULL, so both of
    /// these rows would insert. The partial index split on nullability is what
    /// stops it, without a sentinel policy string.
    #[test]
    fn two_policy_less_rows_with_the_same_event_and_day_cannot_both_insert() {
        let conn = db();
        insert_aggregate(&conn, None, "admitted", 20_000).unwrap();
        let err = insert_aggregate(&conn, None, "admitted", 20_000).unwrap_err();
        assert!(
            format!("{err}").contains("UNIQUE"),
            "a second policy-less row for the same event and day inserted: {err}"
        );

        // The same key one day over is a different row, so the index is not
        // merely rejecting everything.
        insert_aggregate(&conn, None, "admitted", 20_001).unwrap();
    }

    /// The policied half of the same constraint, plus the reason the policy is
    /// two columns and not one rendered `{name}@{version}`: `("a@b", "c")` and
    /// `("a", "b@c")` both render `"a@b@c"`, so a single rendered key column
    /// would merge two distinct policies' counts into one row.
    #[test]
    fn two_policies_that_render_alike_stay_two_rows() {
        let conn = db();
        insert_aggregate(&conn, Some(("a@b", "c")), "admitted", 20_000).unwrap();
        insert_aggregate(&conn, Some(("a", "b@c")), "admitted", 20_000).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_aggregates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 2,
            "two policies that render alike were merged into one row"
        );

        let err = insert_aggregate(&conn, Some(("a@b", "c")), "admitted", 20_000).unwrap_err();
        assert!(format!("{err}").contains("UNIQUE"), "{err}");
    }

    /// `CHECK` is a Family-A construct: a predicate that evaluates to NULL is
    /// read by SQL as *not violated*, so a `CHECK` written over a nullable
    /// column can silently fail to constrain the majority of the table. This
    /// one is written as `(a IS NULL) = (b IS NULL)`, and `IS NULL` yields 0 or
    /// 1 and never NULL — so it binds policy-less rows too. Both directions of
    /// the half-set pair are asserted, and a fully policy-less row is asserted
    /// to still be accepted.
    #[test]
    fn the_policy_pair_check_binds_policy_less_rows_rather_than_evaluating_to_null() {
        let conn = db();

        let err = conn
            .execute(
                "INSERT INTO audit_aggregates
                   (policy_name, policy_version, event, day, count,
                    value_histogram, fragility_histogram, histogram_version)
                 VALUES ('x', NULL, 'admitted', 1, 1, '[]', '[]', 1)",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "half-set (name, NULL): {err}"
        );

        let err = conn
            .execute(
                "INSERT INTO audit_aggregates
                   (policy_name, policy_version, event, day, count,
                    value_histogram, fragility_histogram, histogram_version)
                 VALUES (NULL, 'x', 'admitted', 1, 1, '[]', '[]', 1)",
                [],
            )
            .unwrap_err();
        assert!(
            format!("{err}").contains("CHECK"),
            "half-set (NULL, version): {err}"
        );

        insert_aggregate(&conn, None, "admitted", 1).unwrap();
        insert_aggregate(&conn, Some(("p", "1")), "admitted", 1).unwrap();
    }

    /// `Backend::audit_aggregates` mandates that null placement be stated
    /// rather than defaulted, and the brief records the SQLite behaviour as a
    /// recollection. This pins it: SQLite sorts NULL first under `ASC`, so
    /// `idx_aggregates_order`'s own column order already matches the contract's
    /// "`None` before every `Some`" — the explicit clause agrees with the index
    /// rather than fighting it.
    #[test]
    fn nulls_sort_first_so_the_ordering_index_matches_the_documented_order() {
        let conn = db();
        insert_aggregate(&conn, Some(("a", "1")), "admitted", 7).unwrap();
        insert_aggregate(&conn, None, "admitted", 7).unwrap();

        let implicit: Vec<Option<String>> = conn
            .prepare(
                "SELECT policy_name FROM audit_aggregates ORDER BY day, policy_name COLLATE BINARY",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(implicit, vec![None, Some("a".to_string())]);

        let explicit: Vec<Option<String>> = conn
            .prepare(
                "SELECT policy_name FROM audit_aggregates
                 ORDER BY day, policy_name COLLATE BINARY ASC NULLS FIRST",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            explicit, implicit,
            "the mandated explicit null placement disagrees with the index order"
        );
    }

    /// The documented aggregate ordering must be servable by
    /// `idx_aggregates_order` without a sort — that is what the index is for.
    /// Spelling the mandated null placement as `NULLS FIRST` keeps it servable;
    /// spelling it as `(policy_name IS NULL) DESC` does not, because that
    /// expression is not an index column. Both halves are asserted so the
    /// choice is visible to whoever writes the query builder.
    #[test]
    fn the_aggregate_ordering_index_serves_the_documented_order() {
        let conn = db();
        let servable = plan(
            &conn,
            "SELECT * FROM audit_aggregates ORDER BY day ASC,
               policy_name COLLATE BINARY ASC NULLS FIRST,
               policy_version COLLATE BINARY ASC NULLS FIRST,
               event COLLATE BINARY ASC",
        );
        assert!(
            servable.contains("idx_aggregates_order") && !servable.contains("TEMP B-TREE"),
            "the ordering index does not serve the documented order: {servable}"
        );

        let sorted = plan(
            &conn,
            "SELECT * FROM audit_aggregates ORDER BY day ASC,
               (policy_name IS NULL) DESC,
               policy_name COLLATE BINARY ASC,
               policy_version COLLATE BINARY ASC,
               event COLLATE BINARY ASC",
        );
        assert!(
            sorted.contains("TEMP B-TREE"),
            "the `(x IS NULL) DESC` spelling became index-servable; the note on \
             `idx_aggregates_order` is now stale: {sorted}"
        );
    }

    /// `Backend::audit` orders by `AuditId` descending and pages on `id <
    /// after`; `at` is a separate, genuinely divergent order because
    /// `AuditRecord::new` mints `id` at construction while taking `at` as a
    /// parameter. An index whose third column is `at` cannot serve
    /// `ORDER BY id DESC`, so the cursor path would sort the whole scope on a
    /// table that grows without bound (preflight F4).
    #[test]
    fn the_audit_index_serves_the_id_ordering_the_contract_mandates() {
        let conn = db();
        let cursor = plan(
            &conn,
            "SELECT * FROM audit
             WHERE subject = 's' AND namespace = 'n' AND id < 'x'
             ORDER BY id DESC LIMIT 2",
        );
        assert!(
            !cursor.contains("TEMP B-TREE"),
            "the id-ordered cursor page is sorted rather than indexed: {cursor}"
        );

        // The time-window filter still has an index of its own: the fix is an
        // additional index, not a reordering of the existing one.
        let window = plan(
            &conn,
            "SELECT * FROM audit
             WHERE subject = 's' AND namespace = 'n' AND at BETWEEN 1 AND 2",
        );
        assert!(
            window.contains("idx_audit_scope_at"),
            "the time-window index was dropped rather than kept: {window}"
        );
    }

    /// `record_recall` bumps `items.last_access` and `items.access_count` for
    /// every item it returned, in the same transaction as the audit row. An
    /// unguarded `AFTER UPDATE` trigger makes every recall a full FTS
    /// delete-and-reinsert for each item returned, none of whose indexed
    /// columns changed — write amplification on the read path, inside the
    /// tenant write lock (preflight F3).
    ///
    /// `total_changes()` counts rows changed by trigger programs too, so it
    /// observes the trigger firing directly rather than by inference.
    #[test]
    fn an_access_statistics_bump_does_not_reindex_fts() {
        let conn = db();
        insert_item(&conn, "i1", "the body", "a b");

        let changes = |c: &Connection| -> i64 {
            c.query_row("SELECT total_changes()", [], |r| r.get(0))
                .unwrap()
        };

        let before = changes(&conn);
        conn.execute(
            "UPDATE items SET access_count = access_count + 1, last_access = 99 WHERE id = 'i1'",
            [],
        )
        .unwrap();
        let bump = changes(&conn) - before;

        let before = changes(&conn);
        conn.execute(
            "UPDATE items SET body = 'a different body' WHERE id = 'i1'",
            [],
        )
        .unwrap();
        let reindex = changes(&conn) - before;

        assert_eq!(bump, 1, "an access bump touched more than the items row");
        assert!(
            bump < reindex,
            "the access bump cost as much as a body change ({bump} vs {reindex}), \
             so the FTS trigger is firing on every recall"
        );

        // And the guard must not cost correctness: a body change is still
        // indexed, and the stale term is gone.
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'different'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "the new body was not indexed");
        let stale: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH 'the'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "the replaced body is still indexed");
    }
}
