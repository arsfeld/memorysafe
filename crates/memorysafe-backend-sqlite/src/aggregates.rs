//! The `audit_aggregates` write half. The read half arrives with `purge`,
//! `export` and `import` in the portability task; both are listed there so
//! neither is forgotten again.
//!
//! # The write rule
//!
//! **Every audit row a backend writes increments exactly one aggregate row, in
//! the same transaction that writes the audit row.** The rule is stated once,
//! on `memorysafe_backend::aggregates`, and it binds every path — not only the
//! policy-driven ones. `AggregateKey::policy` is an `Option` precisely because
//! most event classes carry no decision at all, and aggregating only
//! policy-driven events would make `None` unreachable in stored data.
//!
//! # How concurrent increments are made atomic, and by what
//!
//! `memorysafe_backend::aggregates` requires each backend to choose between an
//! atomic upsert, a row lock and serialised writers, **and to say which**. This
//! backend **serialises the writers**: every mutating path goes through
//! `TenantManager::with_write`, whose per-tenant async mutex is the permanent
//! cross-handle serializer — permanent because two live connections to one
//! tenant file are a normal steady state produced by ordinary LRU eviction, not
//! only by a race. The read-modify-write below is correct under that lock and
//! under nothing weaker.
//!
//! **That lock is in-process, and it is worth being exact about what covers
//! the rest.** `with_write`'s mutex lives in one `TenantManager`, so two
//! `SqliteBackend` values over one root — or a second process — are outside
//! it. What covers them is that `audit::insert`'s `INSERT INTO audit` executes
//! before `increment` on **both** call sites (`Backend::apply` and
//! `Backend::record_recall`), so SQLite's single-writer WAL lock is already
//! held by the time the read-modify-write below runs. Reordering `increment`
//! ahead of that insert in either caller removes the second mechanism and
//! leaves only the in-process one.
//!
//! **What used to not be covered on the `apply` path, and is closed now —
//! stated because the reassuring version of this paragraph was wrong once
//! already, and a silently stale correction would be worse than the original
//! mistake.** Before capacity accounting existed, `apply`'s transaction could
//! open with a read: `items::delete` opens with a `SELECT byte_size` and
//! returns without writing at all when the row is absent, so on an eviction
//! the transaction's first statement was a read — and nothing in this crate
//! sets `TransactionBehavior`, so every transaction is `DEFERRED`. A DEFERRED
//! transaction that reads first takes a WAL snapshot and upgrades later; if
//! another connection committed in between, SQLite answers
//! `SQLITE_BUSY_SNAPSHOT`, for which the busy handler is **not** invoked, so
//! `busy_timeout` does not wait that case out.
//!
//! `apply`'s transaction now opens with `capacity::ensure_row` —
//! `INSERT OR IGNORE`, a write, even on the path where the row already
//! exists and the statement changes nothing — placed in `lib.rs` **before**
//! the eviction loop that used to run first. SQLite decides a DEFERRED
//! transaction's locking behaviour from its first executed statement, so a
//! write there takes the reserved lock immediately, the same as
//! `TransactionBehavior::Immediate` would, and the eviction loop's later
//! `SELECT` no longer determines how the transaction opened. This closes the
//! hazard for `apply` specifically. `record_recall`'s transaction was never
//! exposed to it in the first place — it already opened with `audit::insert`,
//! itself always a write.
//!
//! **The ordering was load-bearing once. It no longer is: `apply`'s
//! transaction now opens as `TransactionBehavior::Immediate` (below), which
//! takes the write lock the moment the transaction opens, before any
//! statement runs — so which statement comes first no longer decides how the
//! transaction opened, and the `SQLITE_BUSY_SNAPSHOT` hazard above is closed
//! by the transaction mode itself, independent of statement order.**
//!
//! `capacity::ensure_row(&tx, &txn.scope)` stays `apply`'s first statement
//! anyway. `capacity::adjust` also calls `ensure_row` internally, so the
//! standalone call was always redundant for its *own* correctness — its job
//! was, and remains, to go first. With `Immediate` in place that job is no
//! longer what closes the hazard, so the call is retained as defence in
//! depth: insurance against a future path that opens `apply`'s transaction as
//! `DEFERRED` again, at which point statement order would matter exactly as
//! it used to. Keep `capacity::ensure_row(&tx, &txn.scope)` as `apply`'s
//! first statement in `lib.rs`, ahead of the eviction loop; do not move it
//! down "because `adjust` calls it anyway" — not because today's ordering is
//! load-bearing, but because the call is cheap and a reader who deletes it on
//! that reasoning may not be the one who later reintroduces a `DEFERRED`
//! path and reopens the hazard.
//!
//! **Done.** Four production call sites now use
//! `transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)`: in
//! `portability.rs`, in `purge.rs`, and twice in `lib.rs` (`apply` and
//! `record_recall`). Regenerate the list rather than trusting this one — line
//! numbers and call sites both drift:
//!
//! ```text
//! git grep -n "TransactionBehavior::Immediate" -- crates/memorysafe-backend-sqlite/
//! ```
//!
//! `Immediate` takes the write lock up front regardless of what the first
//! statement turns out to be, and puts the wait back under `busy_timeout`.

use crate::audit::event_from_str;
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
use memorysafe_backend::aggregates::{
    SCORE_HISTOGRAM_BUCKETS, SCORE_HISTOGRAM_VERSION, day_bucket, score_bucket,
};
use memorysafe_backend::{AggregateKey, AuditAggregate, AuditAggregateFilter};
use memorysafe_core::{AuditRecord, PolicyId, TenantId};
use rusqlite::{Connection, OptionalExtension, params};

fn unreadable(field: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError::Storage {
        message: format!("aggregate {field} histogram is not readable: {e}"),
        retryable: false,
    }
}

/// Counts one audit row into its aggregate, creating the row on first sight.
///
/// The key comes from the record itself: the decision's policy **as two
/// fields**, the event's `as_str()`, and `day_bucket(record.at)`. Not from the
/// `audit` table's rendered `policy` column — that column is a display
/// convenience and `PolicyId`'s `Display` is not injective, so keying on it
/// merges distinct policies into one row.
///
/// `sum(histogram) <= count`, never `==`: only rows carrying an `Assessment`
/// land in a bucket, while `count` counts every row matching the key. See
/// `AuditAggregate`.
///
/// # Why an `UPDATE`, then an `INSERT` when it changed nothing
///
/// Not a targetless `ON CONFLICT`, and not one target either. The table's
/// uniqueness lives in **two partial unique indexes** — one predicated on
/// `policy_name IS NOT NULL` and one on `policy_name IS NULL` — because a
/// single index over the nullable tuple would treat NULLs as distinct and let
/// two policy-less rows for the same event and day both insert. An `ON
/// CONFLICT` naming one of those targets covers only half the key space, and
/// the policy-less half is the majority of it. Naming both would mean two
/// statements selected on `policy_name.is_some()`, whose policied branch no
/// test in this crate's write paths reaches.
///
/// `UPDATE` first and `INSERT` when `changes() == 0` has neither problem: one
/// pair of statements covering both halves, with both branches reached by the
/// same policy-less corpus — first increment inserts, second updates. It is
/// correct because writers are serialised; see the module doc.
pub fn increment(conn: &Connection, record: &AuditRecord) -> Result<(), BackendError> {
    let (name, version) = match record.decision.as_ref().map(|d| &d.policy) {
        Some(p) => (Some(p.name.as_str()), Some(p.version.as_str())),
        None => (None, None),
    };
    let event = record.event.as_str();
    let day = day_bucket(record.at);

    // `IS`, not `=`: the policy columns are NULL for the majority of event
    // classes, and `= NULL` is never true. This is the same NULL-distinctness
    // that forced two partial unique indexes in the schema.
    let existing: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT count, value_histogram, fragility_histogram FROM audit_aggregates
             WHERE policy_name IS ?1 AND policy_version IS ?2 AND event = ?3 AND day = ?4",
            params![name, version, event, day],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .sql()?;

    let mut value: Vec<u64> = vec![0; SCORE_HISTOGRAM_BUCKETS];
    let mut fragility: Vec<u64> = vec![0; SCORE_HISTOGRAM_BUCKETS];
    let mut count = 0i64;
    if let Some((c, v, f)) = existing {
        count = c;
        value = serde_json::from_str(&v).map_err(|e| unreadable("value", e))?;
        fragility = serde_json::from_str(&f).map_err(|e| unreadable("fragility", e))?;
        // A stored histogram of the wrong width would index out of bounds
        // below, and a panic inside a `with_write` closure poisons the
        // tenant's connection. A corrupt row is a storage error, not a panic.
        if value.len() != SCORE_HISTOGRAM_BUCKETS || fragility.len() != SCORE_HISTOGRAM_BUCKETS {
            return Err(BackendError::Storage {
                message: format!(
                    "aggregate row holds histograms of width {} and {}, not \
                     {SCORE_HISTOGRAM_BUCKETS}",
                    value.len(),
                    fragility.len()
                ),
                retryable: false,
            });
        }
    }
    count += 1;
    if let Some(a) = &record.assessment {
        value[score_bucket(a.value)] += 1;
        fragility[score_bucket(a.fragility)] += 1;
    }

    let value_json = serde_json::to_string(&value).expect("a Vec<u64> serialises");
    let fragility_json = serde_json::to_string(&fragility).expect("a Vec<u64> serialises");

    let updated = conn
        .execute(
            "UPDATE audit_aggregates
                SET count = ?5,
                    value_histogram = ?6,
                    fragility_histogram = ?7,
                    -- Refreshed, not left at whatever the row was created
                    -- with. `histogram_version` is not part of the key, so a
                    -- row created under older `SCORE_HISTOGRAM_EDGES` would
                    -- otherwise keep advertising them while accumulating
                    -- counts bucketed by this build's — the silent splicing
                    -- the version exists to make detectable. Writing the
                    -- current version makes the row honest about the edges its
                    -- most recent counts used; the retention task is where a
                    -- genuine bump gets its migration.
                    histogram_version = ?8
              WHERE policy_name IS ?1 AND policy_version IS ?2
                AND event = ?3 AND day = ?4",
            params![
                name,
                version,
                event,
                day,
                count,
                value_json,
                fragility_json,
                SCORE_HISTOGRAM_VERSION as i64,
            ],
        )
        .sql()?;

    if updated == 0 {
        conn.execute(
            "INSERT INTO audit_aggregates
                 (policy_name, policy_version, event, day, count,
                  value_histogram, fragility_histogram, histogram_version)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                name,
                version,
                event,
                day,
                count,
                value_json,
                fragility_json,
                SCORE_HISTOGRAM_VERSION as i64,
            ],
        )
        .sql()?;
    }
    Ok(())
}

/// The read path's `ORDER BY`, in the documented key order — day first, then
/// the policy's two parts, then the event name — with collation and null
/// placement stated rather than defaulted, per the mandate on
/// `Backend::audit_aggregates`. Lifted to a `const` so `query` and
/// `tests::ordering_sql_states_collation_and_null_placement` read the same
/// string rather than a literal copy that could drift from what actually
/// runs.
///
/// `NULLS FIRST`, not `(policy_name IS NULL) DESC`: both satisfy the mandate,
/// but only the first is servable by `idx_aggregates_order` — see Task 19's
/// measurement, recorded on `schema`'s DDL comment for that index.
pub(crate) const AGGREGATE_ORDER_SQL: &str = "day ASC,
                      policy_name    COLLATE BINARY ASC NULLS FIRST,
                      policy_version COLLATE BINARY ASC NULLS FIRST,
                      event          COLLATE BINARY ASC";

/// Parses a stored histogram column into the fixed-width array
/// `AuditAggregate` carries. Rejects a wrong length rather than padding it:
/// a histogram narrower or wider than `SCORE_HISTOGRAM_BUCKETS` means the row
/// was written under a different `SCORE_HISTOGRAM_EDGES` than this build
/// has — the case `histogram_version` exists to make detectable — and padding
/// it would silently fold that mismatch into a value that looks computed.
fn histogram_from_json(json: &str) -> Result<[u64; SCORE_HISTOGRAM_BUCKETS], BackendError> {
    let v: Vec<u64> = serde_json::from_str(json).map_err(|e| unreadable("histogram", e))?;
    let len = v.len();
    v.try_into().map_err(|_| BackendError::Storage {
        message: format!(
            "aggregate row holds a histogram of width {len}, not \
             {SCORE_HISTOGRAM_BUCKETS} — it was written under different \
             SCORE_HISTOGRAM_EDGES than this build has"
        ),
        retryable: false,
    })
}

/// One page of aggregates for `tenant`, matching `filter`.
///
/// **The ordering is the four-component key in `Backend::audit_aggregates`'s
/// order**: `day`, then the policy's `name`, then its `version`, then the
/// event's serialised name. Not the struct's field order, and not by
/// `PolicyId::to_string()`, which is not injective — see `AGGREGATE_ORDER_SQL`
/// and `AggregateKey::cmp`. `tenant` is the fifth component of
/// `AggregateKey`'s `Ord` and is omitted from the SQL deliberately: it is a
/// parameter of this query, so every row shares it.
///
/// **The cursor runs the opposite way from `Backend::audit`'s.** `audit` pages
/// **descending** and its `after` selects ids **strictly less** than the
/// cursor. This method pages **ascending** and `after` selects keys
/// **strictly greater**.
///
/// The comparison is a lexicographic row comparison over four components,
/// written out longhand rather than as SQL's `(a,b,c,d) > (w,x,y,z)`: the
/// row-value form evaluates to NULL when any component is NULL, and the
/// policy columns are NULL for the majority of event classes, so the row
/// would be dropped and the page would come back short — which
/// `AuditAggregateFilter::limit` defines as meaning the log is exhausted.
///
/// Returns exactly `min(filter.limit, rows still matching after the cursor)`,
/// the same rule `Backend::audit` carries.
pub fn query(
    conn: &Connection,
    tenant: &TenantId,
    filter: &AuditAggregateFilter,
) -> Result<Vec<AuditAggregate>, BackendError> {
    // The cursor, decomposed. `after_present` is bound separately so the
    // predicate can be switched off without a second SQL string.
    let (a_day, a_name, a_version, a_event) = match &filter.after {
        Some(k) => (
            Some(k.day),
            k.policy.as_ref().map(|p| p.name.clone()),
            k.policy.as_ref().map(|p| p.version.clone()),
            Some(k.event.as_str().to_string()),
        ),
        None => (None, None, None, None),
    };

    let sql = format!(
        "SELECT policy_name, policy_version, event, day, count,
                value_histogram, fragility_histogram, histogram_version
         FROM audit_aggregates
         WHERE (?1 IS NULL OR day >= ?1)
           AND (?2 IS NULL OR day <= ?2)
           AND (?3 IS NULL OR (policy_name IS ?3 AND policy_version IS ?4))
           AND (?5 IS NULL OR (
                 day > ?5
              OR (day = ?5 AND (policy_name IS NOT NULL) > (?6 IS NOT NULL))
              OR (day = ?5 AND (policy_name IS NULL) = (?6 IS NULL)
                  AND coalesce(policy_name, '') COLLATE BINARY
                    > coalesce(?6, '') COLLATE BINARY)
              OR (day = ?5 AND policy_name IS ?6
                  AND coalesce(policy_version, '') COLLATE BINARY
                    > coalesce(?7, '') COLLATE BINARY)
              OR (day = ?5 AND policy_name IS ?6 AND policy_version IS ?7
                  AND event COLLATE BINARY > ?8 COLLATE BINARY)))
         ORDER BY {AGGREGATE_ORDER_SQL}
         LIMIT ?9"
    );

    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(
            params![
                filter.since,
                filter.until,
                filter.policy.as_ref().map(|p| p.name.clone()),
                filter.policy.as_ref().map(|p| p.version.clone()),
                a_day,
                a_name,
                a_version,
                a_event,
                filter.limit as i64,
            ],
            |r| {
                let name: Option<String> = r.get(0)?;
                let version: Option<String> = r.get(1)?;
                let event: String = r.get(2)?;
                Ok((
                    name,
                    version,
                    event,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            },
        )
        .sql()?;

    let mut out = Vec::new();
    for row in rows {
        let (name, version, event, day, count, value, fragility, version_tag) = row.sql()?;
        out.push(AuditAggregate {
            key: AggregateKey {
                tenant: tenant.clone(),
                policy: match (name, version) {
                    (Some(n), Some(v)) => Some(PolicyId::new(&n, &v)),
                    _ => None,
                },
                event: event_from_str(&event)?,
                day,
            },
            count: count as u64,
            value_histogram: histogram_from_json(&value)?,
            fragility_histogram: histogram_from_json(&fragility)?,
            histogram_version: version_tag as u32,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_backend::aggregates::SCORE_HISTOGRAM_EDGES;
    use memorysafe_core::{
        Actor, Assessment, AssessorId, AuditEvent, AuditRecord, Decision, PolicyId, Reason,
        ReasonCode, RedundancyAssessment, Scope, Score, SensitivityAssessment, SensitivityLevel,
    };
    use time::OffsetDateTime;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::initialise(&conn).unwrap();
        conn
    }

    fn record(event: AuditEvent, at: i64) -> AuditRecord {
        AuditRecord::new(
            Scope::new("t", "s", "n").unwrap(),
            event,
            vec![],
            Actor::system(),
            OffsetDateTime::from_unix_timestamp(at).unwrap(),
        )
    }

    fn assessment(value: f32, fragility: f32) -> Assessment {
        Assessment {
            value: Score::clamped(value),
            fragility: Score::clamped(fragility),
            sensitivity: SensitivityAssessment {
                level: SensitivityLevel::Internal,
                categories: vec![],
                confidence: Score::ONE,
            },
            redundancy: RedundancyAssessment {
                score: Score::ZERO,
                near_duplicates: vec![],
            },
            features: Default::default(),
            assessor: AssessorId::new("test", "1"),
        }
    }

    fn decision(name: &str, version: &str) -> Decision {
        Decision::reject(
            PolicyId::new(name, version),
            Reason::new(ReasonCode::LowValue, "", Default::default()),
        )
    }

    /// `(policy_name, policy_version, event, day)` — the whole key, as stored.
    type Key = (Option<String>, Option<String>, String, i64);
    /// `(count, sum(value_histogram), sum(fragility_histogram),
    /// histogram_version)`. The histograms are summed rather than compared
    /// bucket by bucket because most assertions here are about *which key* a
    /// row landed under; the bucket placement has its own test.
    type Totals = (i64, u64, u64, i64);

    /// One row per key, in the order `Backend::audit_aggregates` documents.
    fn rows(conn: &Connection) -> Vec<(Key, Totals)> {
        let mut stmt = conn
            .prepare(
                "SELECT policy_name, policy_version, event, day, count,
                        value_histogram, fragility_histogram, histogram_version
                 FROM audit_aggregates
                 ORDER BY day, policy_name COLLATE BINARY ASC NULLS FIRST,
                          policy_version COLLATE BINARY ASC NULLS FIRST,
                          event COLLATE BINARY ASC",
            )
            .unwrap();
        stmt.query_map([], |r| {
            let value: Vec<u64> = serde_json::from_str(&r.get::<_, String>(5)?).unwrap();
            let fragility: Vec<u64> = serde_json::from_str(&r.get::<_, String>(6)?).unwrap();
            assert_eq!(value.len(), SCORE_HISTOGRAM_BUCKETS);
            assert_eq!(fragility.len(), SCORE_HISTOGRAM_BUCKETS);
            Ok((
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?),
                (
                    r.get(4)?,
                    value.iter().sum::<u64>(),
                    fragility.iter().sum::<u64>(),
                    r.get(7)?,
                ),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// Both branches, on the majority half of the key space. The first
    /// increment takes the `INSERT` path (the `UPDATE` matched nothing); the
    /// second takes the `UPDATE` path. A backend that only ever inserted would
    /// fail on the second call with a UNIQUE violation from
    /// `idx_aggregates_key_policy_less`, and one that only ever updated would
    /// silently store nothing.
    #[test]
    fn a_policy_less_key_inserts_once_and_updates_thereafter() {
        let conn = db();
        let rec = record(AuditEvent::Admitted, 0);
        increment(&conn, &rec).unwrap();
        assert_eq!(
            rows(&conn),
            vec![((None, None, "admitted".into(), 0), (1, 0, 0, 1))]
        );

        increment(&conn, &record(AuditEvent::Admitted, 86_399)).unwrap();
        assert_eq!(
            rows(&conn),
            vec![((None, None, "admitted".into(), 0), (2, 0, 0, 1))],
            "a second row for the same key was created instead of incrementing"
        );
    }

    /// The key discriminates on all four components. Each row below differs
    /// from the first in exactly one of them, so a key that dropped any one
    /// component would collapse two rows into one.
    #[test]
    fn each_key_component_separates_rows() {
        let conn = db();
        let base = record(AuditEvent::Admitted, 0);
        increment(&conn, &base).unwrap();

        // Different day.
        increment(&conn, &record(AuditEvent::Admitted, 86_400)).unwrap();
        // Different event.
        increment(&conn, &record(AuditEvent::Forgotten, 0)).unwrap();
        // Different policy name, and different version under the same name.
        let mut policied = record(AuditEvent::Admitted, 0);
        policied.decision = Some(decision("baseline", "1"));
        increment(&conn, &policied).unwrap();
        let mut other_version = record(AuditEvent::Admitted, 0);
        other_version.decision = Some(decision("baseline", "2"));
        increment(&conn, &other_version).unwrap();

        assert_eq!(rows(&conn).len(), 5, "a key component was dropped");
        assert!(
            rows(&conn).iter().all(|(_, (count, ..))| *count == 1),
            "two distinct keys were merged into one row"
        );
    }

    /// Two policies that render identically under `PolicyId`'s `Display` stay
    /// two rows. This is the case a single rendered `policy` column merges —
    /// `("a@b", "c")` and `("a", "b@c")` both render `"a@b@c"` — and it is why
    /// the key is two columns.
    #[test]
    fn two_policies_that_render_alike_are_two_aggregate_rows() {
        let conn = db();
        let mut left = record(AuditEvent::Admitted, 0);
        left.decision = Some(decision("a@b", "c"));
        let mut right = record(AuditEvent::Admitted, 0);
        right.decision = Some(decision("a", "b@c"));
        assert_eq!(
            left.decision.as_ref().unwrap().policy.to_string(),
            right.decision.as_ref().unwrap().policy.to_string(),
            "the premise: these two render identically"
        );

        increment(&conn, &left).unwrap();
        increment(&conn, &right).unwrap();
        assert_eq!(
            rows(&conn).len(),
            2,
            "two policies that render alike were merged into one aggregate row"
        );
    }

    /// `sum(histogram) <= count`, never `==`: only rows carrying an
    /// `Assessment` land in a bucket, while `count` counts every row matching
    /// the key. The mixed corpus below is what makes the inequality strict.
    #[test]
    fn only_assessed_rows_land_in_a_bucket_so_the_sum_stays_below_the_count() {
        let conn = db();
        let mut assessed = record(AuditEvent::Admitted, 0);
        assessed.assessment = Some(assessment(0.05, 0.95));
        increment(&conn, &assessed).unwrap();
        increment(&conn, &record(AuditEvent::Admitted, 0)).unwrap();

        let got = rows(&conn);
        assert_eq!(got.len(), 1);
        let (count, value_sum, fragility_sum, version) = got[0].1;
        assert_eq!(count, 2);
        assert_eq!(value_sum, 1, "the unassessed row was bucketed");
        assert_eq!(fragility_sum, 1);
        assert!(value_sum < count as u64);
        assert_eq!(version, SCORE_HISTOGRAM_VERSION as i64);
    }

    /// Scores land in the bucket `score_bucket` names, and the two histograms
    /// are kept apart. A single shared histogram, or one written into the
    /// other's column, would be invisible against equal scores — so the two
    /// scores here are deliberately at opposite ends.
    #[test]
    fn value_and_fragility_are_bucketed_separately_and_by_score_bucket() {
        let conn = db();
        let mut rec = record(AuditEvent::Admitted, 0);
        rec.assessment = Some(assessment(0.05, 0.95));
        increment(&conn, &rec).unwrap();

        let (value, fragility): (Vec<u64>, Vec<u64>) = conn
            .query_row(
                "SELECT value_histogram, fragility_histogram FROM audit_aggregates",
                [],
                |r| {
                    Ok((
                        serde_json::from_str(&r.get::<_, String>(0)?).unwrap(),
                        serde_json::from_str(&r.get::<_, String>(1)?).unwrap(),
                    ))
                },
            )
            .unwrap();
        assert_eq!(value[0], 1, "0.05 belongs to the first bucket");
        assert_eq!(value.iter().sum::<u64>(), 1);
        assert_eq!(
            fragility[SCORE_HISTOGRAM_BUCKETS - 1],
            1,
            "0.95 belongs to the last bucket; edges are {SCORE_HISTOGRAM_EDGES:?}"
        );
        assert_eq!(fragility.iter().sum::<u64>(), 1);
    }

    /// A histogram of the wrong width is a storage error, not a panic. A panic
    /// here would happen inside a `with_write` closure and poison the tenant's
    /// connection — a corrupt row taking the tenant out until the pool heals.
    #[test]
    fn a_histogram_of_the_wrong_width_is_an_error_rather_than_a_panic() {
        let conn = db();
        conn.execute(
            "INSERT INTO audit_aggregates
               (policy_name, policy_version, event, day, count,
                value_histogram, fragility_histogram, histogram_version)
             VALUES (NULL, NULL, 'admitted', 0, 1, '[0,0,0]', '[0,0,0]', 1)",
            [],
        )
        .unwrap();

        let mut rec = record(AuditEvent::Admitted, 0);
        rec.assessment = Some(assessment(0.5, 0.5));
        let err = increment(&conn, &rec).unwrap_err();
        assert!(
            matches!(err, BackendError::Storage { .. }),
            "expected a storage error, got {err:?}"
        );
    }

    /// The `UPDATE`'s `histogram_version = ?8` is the whole mechanism keeping a
    /// row honest about which `SCORE_HISTOGRAM_EDGES` its counts were bucketed
    /// with, and nothing else in the crate observes the column: it is not part
    /// of the key, and the read half does not arrive until the portability
    /// task. Without this test, deleting the refresh leaves every test in the
    /// workspace green.
    #[test]
    fn an_increment_refreshes_a_row_left_at_a_foreign_histogram_version() {
        let conn = db();
        let zeros = serde_json::to_string(&vec![0u64; SCORE_HISTOGRAM_BUCKETS]).unwrap();
        let foreign = SCORE_HISTOGRAM_VERSION as i64 + 41;
        conn.execute(
            "INSERT INTO audit_aggregates
               (policy_name, policy_version, event, day, count,
                value_histogram, fragility_histogram, histogram_version)
             VALUES (NULL, NULL, 'admitted', 0, 0, ?1, ?2, ?3)",
            params![zeros, zeros, foreign],
        )
        .unwrap();

        increment(&conn, &record(AuditEvent::Admitted, 0)).unwrap();

        let (count, version): (i64, i64) = conn
            .query_row(
                "SELECT count, histogram_version FROM audit_aggregates
                  WHERE policy_name IS NULL AND event = 'admitted' AND day = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "the increment must have taken the UPDATE branch, not inserted a \
             second row — otherwise the version assertion below proves nothing"
        );
        assert_eq!(
            version, SCORE_HISTOGRAM_VERSION as i64,
            "the row still advertises the edges it was created under while \
             holding a count bucketed by this build's"
        );
    }

    /// The mandate on `Backend::audit_aggregates`: every ordering over a text
    /// column states its collation explicitly, and every ordering over a
    /// nullable column states null placement explicitly, neither left to a
    /// dialect default.
    ///
    /// Asserted against `AGGREGATE_ORDER_SQL`, the exact string `query`
    /// builds its `ORDER BY` from — not a copied literal, which would drift
    /// from what actually runs the moment one changed and not the other.
    /// This checks *explicitness*, a different property from the
    /// conformance sweep's check of the resulting *order*: a backend relying
    /// on a default that happens to agree with the documented order produces
    /// the right rows and still fails this test.
    #[test]
    fn ordering_sql_states_collation_and_null_placement() {
        let sql = AGGREGATE_ORDER_SQL;
        // Every text column this key orders by states its collation
        // explicitly: day (not text, no collation needed), policy_name,
        // policy_version and event — three `COLLATE BINARY`s.
        assert_eq!(
            sql.matches("COLLATE BINARY").count(),
            3,
            "expected an explicit COLLATE BINARY on each of policy_name, \
             policy_version and event: {sql}"
        );
        // Every nullable column in the key states null placement explicitly
        // — policy_name and policy_version, the two that can be NULL.
        assert_eq!(
            sql.matches("NULLS FIRST").count(),
            2,
            "expected an explicit NULLS FIRST on each of policy_name and \
             policy_version: {sql}"
        );
        // And specifically on the nullable columns, not merely present
        // somewhere in the string.
        for nullable in ["policy_name", "policy_version"] {
            let idx = sql.find(nullable).unwrap_or_else(|| {
                panic!("column {nullable} does not appear in the ORDER BY: {sql}")
            });
            let clause = &sql[idx..];
            let end = clause.find(',').unwrap_or(clause.len());
            let clause = &clause[..end];
            assert!(
                clause.contains("COLLATE BINARY") && clause.contains("NULLS FIRST"),
                "column {nullable}'s own ordering clause does not state both \
                 collation and null placement: {clause}"
            );
        }
    }

    /// `histogram_from_json` rejects a histogram of the wrong width rather
    /// than padding or truncating it — the read-side counterpart of
    /// `increment`'s own width check.
    #[test]
    fn histogram_from_json_rejects_the_wrong_width() {
        let err = histogram_from_json("[0,0,0]").unwrap_err();
        assert!(matches!(err, BackendError::Storage { .. }));

        let ok = histogram_from_json(
            &serde_json::to_string(&vec![0u64; SCORE_HISTOGRAM_BUCKETS]).unwrap(),
        );
        assert!(ok.is_ok());
    }
}
