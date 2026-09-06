//! The `capacity` table: one row per `(subject, namespace)`, updated inside
//! the same write transaction as the item write it accounts for.
//!
//! **Why this is safe without a read-modify-write in Rust.** Unlike
//! `aggregates::increment` — which reads a row, decides in Rust whether to
//! `UPDATE` or `INSERT`, and therefore needs the caller's transaction to hold
//! SQLite's write lock before it runs (see `aggregates`' module doc) —
//! [`adjust`] never reads its row at all. `used_items = used_items + ?3` is
//! evaluated by SQLite as a single statement, so there is no window in this
//! crate's own code in which two writers could each compute a stale value.
//! What still has to be true is the same as everywhere else in this crate:
//! [`adjust`] must run inside the write transaction that also writes the
//! audit row, on the same terms `aggregates::increment` states, so that a
//! rolled-back write does not leave the count adjusted for an item that was
//! never actually admitted.
//!
//! The in-process serialiser is still `TenantManager::with_write`'s
//! per-tenant mutex — every mutating path goes through it — so
//! `concurrent_admits_do_not_double_count` is exercising that lock, not a
//! property of this module's SQL. See the task report for the mutation that
//! confirms this.

use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_core::{Budget, CapacityState, Scope, ScopeStats};
use rusqlite::{Connection, OptionalExtension, params};

/// Creates the scope's accounting row if it does not exist yet, leaving an
/// existing row untouched. Every other function here calls this first so a
/// scope's first write does not have to be special-cased.
pub fn ensure_row(conn: &Connection, scope: &Scope) -> Result<(), BackendError> {
    conn.execute(
        "INSERT OR IGNORE INTO capacity (subject, namespace, used_items, used_bytes)
         VALUES (?1, ?2, 0, 0)",
        params![scope.subject.as_str(), scope.namespace.as_str()],
    )
    .sql()?;
    Ok(())
}

pub fn set_budget(conn: &Connection, scope: &Scope, budget: Budget) -> Result<(), BackendError> {
    ensure_row(conn, scope)?;
    conn.execute(
        "UPDATE capacity SET max_items = ?3, max_bytes = ?4
         WHERE subject = ?1 AND namespace = ?2",
        params![
            scope.subject.as_str(),
            scope.namespace.as_str(),
            budget.max_items.map(|v| v as i64),
            budget.max_bytes.map(|v| v as i64),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn state(conn: &Connection, scope: &Scope) -> Result<CapacityState, BackendError> {
    // `.optional()`, not `.ok()`: a scope that has never been written to (or
    // never had `set_budget` called on it) has no row yet, and that is the
    // *only* case this falls back on. `.ok()` on the bare `Result` would also
    // swallow a genuine failure — a busy database, a corrupt row — and report
    // it as an empty, unbounded scope, which looks like real data. Same shape
    // of defect `capacity::stats`'s median query below has to avoid, and the
    // one `items::delete`'s byte-size read already avoids.
    let row: Option<(Option<i64>, Option<i64>, i64, i64)> = conn
        .query_row(
            "SELECT max_items, max_bytes, used_items, used_bytes FROM capacity
             WHERE subject = ?1 AND namespace = ?2",
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .sql()?;

    Ok(match row {
        Some((mi, mb, ui, ub)) => CapacityState {
            budget: Budget {
                max_items: mi.map(|v| v as u64),
                max_bytes: mb.map(|v| v as u64),
            },
            used_items: ui.max(0) as u64,
            used_bytes: ub.max(0) as u64,
        },
        None => CapacityState {
            budget: Budget::UNBOUNDED,
            used_items: 0,
            used_bytes: 0,
        },
    })
}

/// Applied inside the write transaction. Deltas are signed; the row is
/// clamped at zero so a bookkeeping slip cannot go negative and wrap.
pub fn adjust(
    conn: &Connection,
    scope: &Scope,
    delta_items: i64,
    delta_bytes: i64,
) -> Result<(), BackendError> {
    ensure_row(conn, scope)?;
    conn.execute(
        "UPDATE capacity
         SET used_items = MAX(0, used_items + ?3),
             used_bytes = MAX(0, used_bytes + ?4)
         WHERE subject = ?1 AND namespace = ?2",
        params![
            scope.subject.as_str(),
            scope.namespace.as_str(),
            delta_items,
            delta_bytes
        ],
    )
    .sql()?;
    Ok(())
}

pub fn stats(conn: &Connection, scope: &Scope) -> Result<ScopeStats, BackendError> {
    let (count, total): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(byte_size), 0) FROM items
             WHERE subject = ?1 AND namespace = ?2",
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .sql()?;

    // Guarded by `count == 0`, so `QueryReturnedNoRows` is not the case this
    // protects against — a genuine failure (a busy database, a corrupt row)
    // is. `.unwrap_or(0)` directly on the `Result` would read a real error as
    // "median is zero", a `ScopeStats` that looks computed. `.optional()`
    // first means only the (here, unreachable so long as `count > 0`) "no row
    // at this offset" case falls back to 0; every other error still
    // propagates via `.sql()?`.
    let median: i64 = if count == 0 {
        0
    } else {
        conn.query_row(
            "SELECT byte_size FROM items WHERE subject = ?1 AND namespace = ?2
             ORDER BY byte_size LIMIT 1 OFFSET ?3",
            params![scope.subject.as_str(), scope.namespace.as_str(), count / 2],
            |r| r.get(0),
        )
        .optional()
        .sql()?
        .unwrap_or(0)
    };

    Ok(ScopeStats {
        item_count: count.max(0) as u64,
        total_bytes: total.max(0) as u64,
        // Computed by the engine from sampled neighbours; the backend has no
        // cheap way to produce it and a wrong value is worse than zero. See
        // `ScopeStats::mean_neighbour_similarity`'s own doc: any consumer
        // reading it directly must check `item_count` first.
        mean_neighbour_similarity: 0.0,
        median_item_bytes: median.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialise(&conn).unwrap();
        conn
    }

    fn scope(subject: &str, namespace: &str) -> Scope {
        Scope::new("t", subject, namespace).unwrap()
    }

    fn insert_item(conn: &Connection, id: &str, subject: &str, namespace: &str, bytes: i64) {
        conn.execute(
            "INSERT INTO items
               (id, subject, namespace, body, kind, source_kind, created_at,
                tags, attrs, sensitivity, protection, byte_size)
             VALUES (?1, ?2, ?3, 'b', 'note', 'user', 0, '[]', '{}', 0, 'normal', ?4)",
            params![id, subject, namespace, bytes],
        )
        .unwrap();
    }

    /// A scope with no row at all reads back unbounded and empty, never an
    /// error and never a row belonging to a different scope.
    #[test]
    fn state_of_an_unwritten_scope_is_unbounded_and_empty() {
        let conn = db();
        let s = state(&conn, &scope("s", "n")).unwrap();
        assert_eq!(s.budget, Budget::UNBOUNDED);
        assert_eq!(s.used_items, 0);
        assert_eq!(s.used_bytes, 0);
    }

    /// `adjust` creates the row on first use and accumulates thereafter, in
    /// both dimensions independently — the shape `capacity_accounting_tracks_items_and_bytes`
    /// checks through the trait; this pins the module's own arithmetic
    /// directly, including a negative delta (the eviction shape) bottoming
    /// out at zero rather than going negative.
    #[test]
    fn adjust_accumulates_and_clamps_at_zero() {
        let conn = db();
        let s = scope("s", "n");

        adjust(&conn, &s, 3, 300).unwrap();
        let got = state(&conn, &s).unwrap();
        assert_eq!(got.used_items, 3);
        assert_eq!(got.used_bytes, 300);

        adjust(&conn, &s, -1, -50).unwrap();
        let got = state(&conn, &s).unwrap();
        assert_eq!(got.used_items, 2);
        assert_eq!(got.used_bytes, 250);

        // A delta larger than what is on the books clamps at zero rather
        // than wrapping negative — `i64` would go negative silently and
        // `.max(0) as u64` in `state` cannot recover a value that was never
        // stored correctly in the first place.
        adjust(&conn, &s, -100, -100_000).unwrap();
        let got = state(&conn, &s).unwrap();
        assert_eq!(got.used_items, 0);
        assert_eq!(got.used_bytes, 0);
    }

    /// `set_budget` writes both dimensions and leaves `used_*` alone; a
    /// scope's usage must survive a budget change.
    #[test]
    fn set_budget_writes_the_budget_without_disturbing_usage() {
        let conn = db();
        let s = scope("s", "n");
        adjust(&conn, &s, 5, 500).unwrap();

        set_budget(
            &conn,
            &s,
            Budget {
                max_items: Some(10),
                max_bytes: Some(1000),
            },
        )
        .unwrap();

        let got = state(&conn, &s).unwrap();
        assert_eq!(got.budget.max_items, Some(10));
        assert_eq!(got.budget.max_bytes, Some(1000));
        assert_eq!(got.used_items, 5, "set_budget disturbed used_items");
        assert_eq!(got.used_bytes, 500, "set_budget disturbed used_bytes");
    }

    /// `adjust` and `state` are scoped by subject and namespace, exactly like
    /// `items::get`/`items::list` — nothing structural enforces either
    /// dimension here, since the tenant (the only structural boundary) is the
    /// file.
    #[test]
    fn capacity_is_scoped_by_subject_and_namespace() {
        let conn = db();
        let home = scope("s", "n");
        let other_subject = scope("other-s", "n");
        let other_namespace = scope("s", "other-n");

        adjust(&conn, &home, 1, 100).unwrap();

        assert_eq!(state(&conn, &other_subject).unwrap().used_items, 0);
        assert_eq!(state(&conn, &other_namespace).unwrap().used_items, 0);
        assert_eq!(state(&conn, &home).unwrap().used_items, 1);
    }

    /// `stats` computes `item_count`, `total_bytes` and `median_item_bytes`
    /// from the `items` table directly — not from `capacity`, which this
    /// function never reads. An odd count, so the median lands on a real row
    /// rather than needing interpolation, and the byte sizes are spaced apart
    /// so an off-by-one in the `OFFSET` reads back a *different* value rather
    /// than one that happens to coincide.
    #[test]
    fn stats_reports_exact_count_total_and_median() {
        let conn = db();
        let s = scope("s", "n");
        // Sizes 10, 20, 30, 40, 50 — sorted median (index 2, offset count/2=2)
        // is 30.
        for (i, bytes) in [10i64, 40, 20, 50, 30].into_iter().enumerate() {
            insert_item(&conn, &format!("i{i}"), "s", "n", bytes);
        }
        // A bystander in a different scope must not be counted.
        insert_item(&conn, "elsewhere", "other-s", "n", 999);

        let got = stats(&conn, &s).unwrap();
        assert_eq!(got.item_count, 5);
        assert_eq!(got.total_bytes, 150);
        assert_eq!(
            got.median_item_bytes, 30,
            "median must be the middle value of the sorted byte sizes, at \
             OFFSET count/2"
        );
    }

    /// An empty scope reports zero for every field, and in particular does
    /// not run the median query at all (`count == 0` guards it) — `stats`
    /// must not error or panic on `OFFSET 0` against an empty table.
    #[test]
    fn stats_of_an_empty_scope_is_all_zero() {
        let conn = db();
        let got = stats(&conn, &scope("s", "n")).unwrap();
        assert_eq!(got.item_count, 0);
        assert_eq!(got.total_bytes, 0);
        assert_eq!(got.median_item_bytes, 0);
    }
}
