use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 1;

/// The version marker's own table, created and read **before** anything else
/// touches the file. Everything in `DDL` runs only once the version has been
/// checked; see `initialise`.
const META_DDL: &str =
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);";

const DDL: &str = r#"

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
-- has no INTEGER PRIMARY KEY, so SQLite's documented behaviour permits VACUUM
-- to renumber those rowids and leave the index naming the wrong rows. The
-- bundled SQLite does not actually renumber here, and
-- `tests::vacuum_does_not_desynchronise_the_fts_index` asserts *that* rather
-- than the precaution — so this stops being a remembered hazard and starts
-- being a tripwire. If it ever fires, rebuild after VACUUM with
-- `INSERT INTO items_fts(items_fts) VALUES('rebuild')`.

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
--
-- **To use this index, spell the mandated null placement `ASC NULLS FIRST` and
-- not `(policy_name IS NULL) DESC`.** Both satisfy the mandate; only the first
-- is servable, because the second orders by a computed value that is not an
-- index column. Measured with EXPLAIN QUERY PLAN, not recollected: the
-- expression form comes back `USE TEMP B-TREE FOR LAST 4 TERMS OF ORDER BY`
-- (`day` is still served by this index, the four key components after it are
-- sorted), while the `NULLS FIRST` form uses the index for all of them.
-- `tests::the_aggregate_ordering_index_serves_the_documented_order` asserts
-- both halves, so if a future SQLite makes the expression form servable the
-- test fails rather than this note going quietly stale.
CREATE INDEX IF NOT EXISTS idx_aggregates_order ON audit_aggregates(
  day,
  policy_name    COLLATE BINARY,
  policy_version COLLATE BINARY,
  event          COLLATE BINARY
);
"#;

/// Applies pragmas and DDL. Idempotent — safe on every open.
///
/// **The version is checked before anything durable is written.** Only
/// `busy_timeout` (per-connection, nothing on disk) and `meta`'s own
/// `CREATE TABLE IF NOT EXISTS` run ahead of it. That ordering is the whole
/// point: `journal_mode=WAL` is a *persistent* change to the file header and
/// the rest of `DDL` creates eleven more tables, so checking afterwards means
/// refusing to open a database you have already converted and extended. An
/// earlier version of this function did exactly that — probed against a
/// "version 2" file, it took the table count from 1 to 12 and `journal_mode`
/// from `delete` to `wal`, and only then declined.
///
/// **The pragmas must be applied outside a transaction.** `PRAGMA
/// foreign_keys` is a **no-op inside one** — SQLite ignores it silently, with
/// no error — so wrapping the open path in a transaction would turn the
/// `vectors` cascade off without breaking anything visible here, and the
/// symptom would surface tasks later as vectors left behind by a purge.
/// `tests::initialise_turns_foreign_keys_on_and_an_item_delete_cascades_to_its_vector`
/// is what breaks if that happens; it starts from a connection with foreign
/// keys forced off, because this build has them on by default and an
/// end-state assertion cannot see the difference.
pub fn initialise(conn: &Connection) -> rusqlite::Result<()> {
    // Per-connection only, and set first so the version read below waits
    // rather than failing under a concurrent writer.
    conn.pragma_update(None, "busy_timeout", 5000)?;

    // `INSERT OR IGNORE` and then check, never an UPSERT. An unconditional
    // UPSERT makes the marker write-only: v1 code opening a v2 file rewrites
    // it *down* to 1 and carries on against a schema it does not understand,
    // and v2 code opening a v1 file writes 2 over an unmigrated database. Both
    // destroy the only evidence of what the file actually is. Refusing to open
    // is the whole value of storing a version, and there is no migration path
    // to offer instead while `SCHEMA_VERSION` is 1.
    conn.execute_batch(META_DDL)?;
    conn.execute(
        "INSERT OR IGNORE INTO meta(key, value) VALUES('schema_version', ?1)",
        [SCHEMA_VERSION.to_string()],
    )?;
    let stored: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )?;
    if stored != SCHEMA_VERSION.to_string() {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!(
                "database is at schema version {stored} and this build expects \
                 {SCHEMA_VERSION}; there is no migration path, and this build \
                 will not convert or extend a file it cannot read"
            )),
        ));
    }

    // Past the gate: now the durable changes.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(DDL)?;
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

    /// `foreign_keys=ON` is set by `initialise` and nothing else in this crate
    /// visibly depends on it, so without this the cascade could be off for
    /// several tasks before anyone noticed.
    ///
    /// **This starts from `OFF` on purpose, and the reason is a measurement.**
    /// The bundled SQLite is compiled with `SQLITE_DEFAULT_FOREIGN_KEYS=1` — a
    /// fresh connection reads `1` before `initialise` touches it, which is
    /// *not* upstream SQLite's default. So a test that only asserted the end
    /// state would stay green with `initialise`'s pragma line deleted, and
    /// green with it wrapped in a transaction, where `PRAGMA foreign_keys` is a
    /// documented **silent no-op** (probed on 3.53.2: set to `ON` inside a
    /// transaction, it still reads `0`). Both mutants were run against the
    /// end-state version of this test and both survived. Forcing `OFF` first is
    /// what kills them, and it is also what the crate would face if `bundled`
    /// were ever swapped for a system libsqlite3.
    #[test]
    fn initialise_turns_foreign_keys_on_and_an_item_delete_cascades_to_its_vector() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        initialise(&conn).unwrap();

        let on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on, 1, "initialise did not turn foreign keys on");

        insert_item(&conn, "i1", "body", "tag");
        conn.execute(
            "INSERT INTO vectors(item_id, subject, namespace, embedder, dim, scale, q)
             VALUES ('i1', 's', 'n', 'test', 4, 1.0, x'00010203')",
            [],
        )
        .unwrap();

        conn.execute("DELETE FROM items WHERE id = 'i1'", [])
            .unwrap();
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "the vector outlived its item");
    }

    /// The `schema_version` marker is only worth storing if a mismatch refuses
    /// to open. An unconditional UPSERT made it write-only: v1 code opening a
    /// v2 file would rewrite the marker *down* and carry on against a schema it
    /// does not understand, destroying the only evidence of what the file is.
    #[test]
    fn a_database_from_another_schema_version_is_refused_rather_than_relabelled() {
        let conn = db();
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            [(SCHEMA_VERSION + 1).to_string()],
        )
        .unwrap();

        let err = initialise(&conn).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("schema version"),
            "expected a version refusal, got: {message}"
        );

        // And the marker was not overwritten on the way out.
        let stored: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, (SCHEMA_VERSION + 1).to_string());
    }

    /// The refusal must come **before** the file is converted and extended, not
    /// after. On a real file, not in memory: `journal_mode` is a persistent
    /// header change and an in-memory database cannot leave `memory`, so the
    /// interesting assertion would be vacuous there.
    #[test]
    fn a_foreign_schema_version_is_refused_before_anything_durable_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key,value) VALUES('schema_version','2');",
        )
        .unwrap();
        let journal_before: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            journal_before, "delete",
            "the fixture is not in rollback mode"
        );

        let err = initialise(&conn).unwrap_err();
        assert!(
            format!("{err}").contains("schema version"),
            "expected a version refusal, got: {err}"
        );

        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            tables, 1,
            "the refusal ran the DDL first: {tables} tables in a database this \
             build has just declined to understand"
        );
        let journal_after: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            journal_after, journal_before,
            "the refusal converted the file to WAL on its way out"
        );
    }

    /// `items_fts` is external-content over `items.rowid`, and `items` has no
    /// INTEGER PRIMARY KEY, so SQLite's documented behaviour permits VACUUM to
    /// renumber those rowids and leave the index naming the wrong rows —
    /// silently, with the symptom being a search that returns the wrong
    /// document. The bundled SQLite does not in fact renumber here, and this
    /// asserts *that* rather than the precaution: the day it changes, this test
    /// says so instead of a recall quietly going wrong.
    #[test]
    fn vacuum_does_not_desynchronise_the_fts_index() {
        let conn = db();
        insert_item(&conn, "i1", "alpha", "one");
        insert_item(&conn, "i2", "beta", "two");
        conn.execute("DELETE FROM items WHERE id = 'i1'", [])
            .unwrap();

        conn.execute_batch("VACUUM").unwrap();
        conn.execute_batch("INSERT INTO items_fts(items_fts) VALUES('integrity-check')")
            .unwrap();

        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM items_fts WHERE items_fts MATCH 'beta'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let id: String = conn
            .query_row("SELECT id FROM items WHERE rowid = ?1", [rowid], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            id, "i2",
            "VACUUM renumbered rowids and the FTS index now names the wrong row"
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
