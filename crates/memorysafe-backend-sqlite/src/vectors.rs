//! The `vectors` table and exact brute-force nearest-neighbour search.
//!
//! No ANN index: per-scope corpora are small, results are exact, and there is
//! nothing to rebuild after every write. `scope_embedder` reports which model
//! a scope's vectors were produced by, so a probe from a different model is
//! rejected rather than silently compared — see `lib.rs`'s `neighbours`.

use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::retrieve::filter_sql;
use crate::tenant::SqlResultExt;
use memorysafe_backend::{BackendError, HardFilters};
use memorysafe_core::{ItemId, MemoryItem, Scope};
use memorysafe_embed::QuantizedVector;
use rusqlite::{Connection, Row, params};
use time::OffsetDateTime;

pub fn insert(
    conn: &Connection,
    id: &ItemId,
    scope: &Scope,
    q: &QuantizedVector,
) -> Result<(), BackendError> {
    conn.execute(
        "INSERT INTO vectors (item_id, subject, namespace, embedder, dim, scale, q)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(item_id) DO UPDATE SET
             embedder=excluded.embedder, dim=excluded.dim,
             scale=excluded.scale, q=excluded.q",
        params![
            id.as_str(),
            scope.subject.as_str(),
            scope.namespace.as_str(),
            q.embedder.to_string(),
            q.dim as i64,
            q.scale as f64,
            q.to_bytes(),
        ],
    )
    .sql()?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &ItemId) -> Result<(), BackendError> {
    conn.execute(
        "DELETE FROM vectors WHERE item_id = ?1",
        params![id.as_str()],
    )
    .sql()?;
    Ok(())
}

/// The number of `vectors` rows stored for `scope`, read from this table's
/// **own** `subject`/`namespace` columns — no join to `items`.
///
/// This is the low-level half of
/// [`crate::SqliteBackend::vector_row_count`], the public accessor built on
/// it; see that method's doc for why an unjoined, table-own count is the
/// thing worth exposing at all rather than a count derived from `search` or
/// `scope_embedder`.
pub fn count(conn: &Connection, scope: &Scope) -> Result<u64, BackendError> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM vectors WHERE subject=?1 AND namespace=?2",
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| r.get(0),
        )
        .sql()?;
    Ok(n as u64)
}

/// Which embedder this scope's vectors were produced by. `None` when the scope
/// holds no vectors yet. Comparing across models yields silently meaningless
/// similarities, so every search gates on this.
pub fn scope_embedder(
    conn: &Connection,
    scope: &Scope,
) -> Result<Option<(String, u16)>, BackendError> {
    let mut stmt = conn
        .prepare(
            "SELECT embedder, dim FROM vectors
             WHERE subject=?1 AND namespace=?2 LIMIT 1",
        )
        .sql()?;
    let mut rows = stmt
        .query_map(
            params![scope.subject.as_str(), scope.namespace.as_str()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u16)),
        )
        .sql()?;
    match rows.next() {
        Some(r) => Ok(Some(r.sql()?)),
        None => Ok(None),
    }
}

/// The two access columns, carried beside the item rather than on it.
///
/// They must NOT go on `MemoryItem`: it is exported and digested, so adding a
/// mutable counter to it would change an item's digest every time it is read.
///
/// **A row never recalled is `(None, 0)`, never `(Some(created_at), 0)`.**
/// `ScoredCandidate::last_accessed_at`'s doc explains why the `Option` is the
/// only thing separating the two states — `fx::item` pins `created_at` to
/// `UNIX_EPOCH`, so a wrongly-defaulted `Some(created_at)` carries the same
/// instant a real one would and no assertion on the timestamp can see it.
pub struct AccessStats {
    pub last_accessed_at: Option<OffsetDateTime>,
    pub access_count: u64,
}

/// Read `AccessStats` from a row that selected `last_access` and
/// `access_count` under those names. One implementation, because
/// `vectors::search` and `keyword::search` are the two arms of one fusion and
/// a difference between them would show up as retrieval-path-dependent access
/// statistics — the hardest kind of discrepancy to notice.
pub fn access_stats(row: &Row<'_>) -> rusqlite::Result<AccessStats> {
    let last: Option<i64> = row.get("last_access")?;
    Ok(AccessStats {
        last_accessed_at: last
            .map(OffsetDateTime::from_unix_timestamp)
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Integer,
                    Box::new(e),
                )
            })?,
        access_count: row.get::<_, i64>("access_count")? as u64,
    })
}

/// Exact brute-force top-k. No ANN index: per-scope corpora are small, results
/// are exact, and there is nothing to rebuild after every write.
///
/// `filters` is applied in the `WHERE` clause via `filter_sql`, before this
/// function's own Rust-side truncation to `k`: a filter enforced only after
/// that truncation can let excluded rows crowd real matches out of the top-`k`
/// set entirely rather than merely leaking one through — see `filter_sql`'s
/// doc and `retrieve.rs`'s module doc.
pub fn search(
    conn: &Connection,
    scope: &Scope,
    probe: &QuantizedVector,
    filters: &HardFilters,
    k: usize,
) -> Result<Vec<(MemoryItem, AccessStats, f32)>, BackendError> {
    if k == 0 {
        return Ok(vec![]);
    }
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(scope.subject.as_str().to_string()),
        Box::new(scope.namespace.as_str().to_string()),
        Box::new(probe.embedder.to_string()),
        Box::new(probe.dim as i64),
    ];
    let filter_clause = filter_sql(filters, &mut args);
    let sql = format!(
        // `last_access`/`access_count` are named explicitly: `ITEM_COLUMNS`
        // contains neither, and `row_to_item` does not read them.
        "SELECT i.rowid, {cols}, i.last_access AS last_access,
                i.access_count AS access_count,
                v.embedder AS v_embedder, v.dim AS v_dim,
                v.scale AS v_scale, v.q AS v_q
         FROM items i JOIN vectors v ON v.item_id = i.id
         WHERE i.subject = ?1 AND i.namespace = ?2
           AND v.embedder = ?3 AND v.dim = ?4{filter_clause}",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), move |r| {
            let item = row_to_item(r, &tenant)?;
            let access = access_stats(r)?;
            let scale: f64 = r.get("v_scale")?;
            let bytes: Vec<u8> = r.get("v_q")?;
            Ok((item, access, scale as f32, bytes))
        })
        .sql()?;

    let mut scored: Vec<(MemoryItem, AccessStats, f32)> = Vec::new();
    for row in rows {
        let (item, access, scale, bytes) = row.sql()?;
        let q = QuantizedVector::from_bytes(probe.embedder.clone(), probe.dim, scale, &bytes)
            .map_err(|e| BackendError::Storage {
                message: e.to_string(),
                retryable: false,
            })?;
        // `Storage`, not `EmbedderMismatch`. `dot` fails on exactly one
        // condition — differing embedder or dim — and `q` was just built with
        // `probe`'s own embedder and dim, so this branch is unreachable by
        // construction. Cross-model exclusion is done by the `WHERE` above and
        // rejection by `neighbours`' `scope_embedder` guard. An
        // `EmbedderMismatch` whose `got` held a `Display`ed error would be a
        // lie in the one field a caller would read to diagnose the mismatch.
        let score = probe.dot(&q).map_err(|e| BackendError::Storage {
            message: e.to_string(),
            retryable: false,
        })?;
        scored.push((item, access, score));
    }

    // Tie-break by ascending `ItemId` before truncating at `k`: this
    // function is the over-fetch source for both `neighbours` (`k` is the
    // caller's `k`) and `retrieve_candidates` (`k` is `query.limit * 4`), so
    // an untied truncation here drops candidates nondeterministically
    // upstream of fusion — a downstream tie-break on the fused set cannot
    // recover a candidate this truncation already discarded.
    scored.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.id.cmp(&b.0.id)));
    scored.truncate(k);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use memorysafe_embed::{DeterministicEmbedder, Embedder};
    use rusqlite::Connection;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        schema::initialise(&c).unwrap();
        c
    }

    /// A `HardFilters` that excludes nothing, for tests whose subject is
    /// unrelated to filtering. `HardFilters::default()` fails closed
    /// (`sensitivity_ceiling: Internal`), which happens to admit every fixture
    /// item here since `fx::item` defaults to `Internal` too — but that
    /// coincidence is exactly the kind of thing a later fixture change could
    /// silently break, so these tests are explicit about wanting "nothing
    /// filtered" rather than relying on two defaults agreeing.
    fn unrestricted() -> HardFilters {
        HardFilters {
            sensitivity_ceiling: memorysafe_core::SensitivityLevel::Restricted,
            ..Default::default()
        }
    }

    #[test]
    fn top_k_is_bounded_and_sorted_descending() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let e = DeterministicEmbedder::new(256);

        for body in [
            "alpha one",
            "alpha two",
            "alpha three",
            "unrelated finance topic",
        ] {
            let item = {
                let mut i = memorysafe_backend::conformance::fx::item(&scope, body);
                i.body = body.to_string();
                i
            };
            crate::items::insert(&c, &item).unwrap();
            let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed(body).unwrap());
            insert(&c, &item.id, &scope, &q).unwrap();
        }

        let probe =
            memorysafe_embed::QuantizedVector::from_embedding(&e.embed("alpha one").unwrap());
        let hits = search(&c, &scope, &probe, &unrestricted(), 2).unwrap();

        assert_eq!(hits.len(), 2, "k was not honoured");
        // `.2` is the score. `search` returns `(MemoryItem, AccessStats, f32)`
        // — `.1` is `AccessStats`, which has no `PartialOrd`, so the brief's
        // literal `hits[0].1 >= hits[1].1` does not compile against the
        // corrected three-element tuple. See the task report for this
        // deviation.
        assert!(hits[0].2 >= hits[1].2, "results not sorted descending");
        assert_eq!(hits[0].0.body, "alpha one");
    }

    #[test]
    fn scope_embedder_reports_the_stored_model() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        assert_eq!(scope_embedder(&c, &scope).unwrap(), None);

        let e = DeterministicEmbedder::new(256);
        let item = memorysafe_backend::conformance::fx::item(&scope, "hello");
        crate::items::insert(&c, &item).unwrap();
        let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed("hello").unwrap());
        insert(&c, &item.id, &scope, &q).unwrap();

        assert_eq!(
            scope_embedder(&c, &scope).unwrap(),
            Some(("deterministic-256".to_string(), 256))
        );
    }

    /// `search` and `scope_embedder` must not cross a subject *or* a
    /// namespace boundary within one tenant. The tenant is the file — that
    /// isolation is structural — but subject and namespace are query
    /// predicates here and nothing structural enforces either.
    ///
    /// `isolation::retrieval_never_crosses_a_scope_boundary` owns this
    /// property through the trait, but it reads through *both*
    /// `retrieve_candidates` and `neighbours`, so it cannot bind until
    /// `retrieve_candidates` exists (Task 22) — a deferral recorded in
    /// `tests/conformance.rs`'s module doc. Until then, this crate-local test
    /// is the only thing that can fail if either predicate is dropped. It is
    /// modelled directly on that test — three properties it already gets
    /// right, restated here because a fix round having asked for "two
    /// subjects (or two namespaces)" is what let a namespace-only regression
    /// through the first time:
    ///
    /// **One dimension varied at a time.** An earlier version of this test
    /// varied only the subject (`home` vs. an `elsewhere` differing solely in
    /// subject), which certifies the subject predicate and says nothing about
    /// the namespace one — `WHERE i.subject = ?1 AND i.namespace = ?2` with
    /// the second half neutralised passed that version outright. Here `home`
    /// has two neighbours, `subject_neighbour` and `namespace_neighbour`,
    /// each differing from `home` in **exactly one** component, so a
    /// predicate that drops either half is caught by the neighbour that
    /// varies it.
    ///
    /// **A positive control, so a complete no-op cannot hide behind an
    /// all-empty assertion.** Every "the foreign item did not come back"
    /// check below is satisfied by a backend that returns nothing at all —
    /// including one whose `insert` silently wrote nothing. So `home`'s own
    /// item and model identity are asserted *present* first, and the two
    /// neighbour corpora are read back from their own scopes to prove their
    /// writes actually landed, before any absence is trusted.
    ///
    /// **`scope_embedder` needs its own, empty-of-content neighbours, and
    /// cannot reuse `subject_neighbour`/`namespace_neighbour` for this.**
    /// `search` returns a `Vec`, so "did the foreign item come back" is a
    /// membership question with no ambiguity. `scope_embedder` returns one
    /// `Option<(embedder, dim)>` picked by `LIMIT 1` with no `ORDER BY`, and
    /// every scope in this test shares one embedder and one dimension on
    /// purpose (round 1's finding: this test isolates the subject/namespace
    /// predicate from cross-model exclusion, which is a separate mechanism).
    /// That means a leaking query and a correctly-scoped one return the
    /// *identical value* whenever the leaked-from scope has any vector at
    /// all — asserting `scope_embedder(&c, &home)` equals the shared tuple
    /// cannot distinguish "read home's own row" from "read someone else's,
    /// which happens to look the same". The only way to observe a leak
    /// through an `Option` is presence vs. absence, which requires a probing
    /// scope with **no vectors of its own**: `subject_diag` shares `home`'s
    /// namespace but an unused subject (so a dropped subject predicate lets
    /// `home`'s row satisfy it), and `namespace_diag` shares `home`'s subject
    /// but an unused namespace (so a dropped namespace predicate lets
    /// `home`'s row satisfy it). Relying on `LIMIT 1`'s physical row order
    /// instead — giving each scope a distinct embedder and asserting which
    /// one comes back — was considered and rejected: SQLite does not
    /// document an order for an unindexed `LIMIT 1` with no `ORDER BY`, so a
    /// test built on it would be asserting on undefined behaviour.
    ///
    /// **`vectors.subject`/`vectors.namespace` are read back directly, with
    /// raw SQL, rather than only through `search` or `scope_embedder`.**
    /// Neither reader can see a corrupted write to those two columns:
    /// `search` joins `vectors` to `items` on `item_id` and filters on
    /// `items.subject`/`items.namespace` alone, never touching
    /// `vectors.subject`/`vectors.namespace`, and `scope_embedder`'s
    /// diagnostic scopes above probe coordinates that a hardcoded or swapped
    /// column in `insert` would not alias onto. A backend that writes the
    /// `items` row correctly and hardcodes (or swaps) the `vectors` row's own
    /// `subject`/`namespace` passes every assertion above unnoticed.
    #[test]
    fn search_and_scope_embedder_are_scoped_by_subject_and_namespace() {
        let c = conn();
        let e = DeterministicEmbedder::new(256);

        let home = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        // Content neighbours for `search`: each differs from `home` in
        // exactly one component and holds its own vector, so a predicate
        // that omits either half is caught by the neighbour that varies it.
        let subject_neighbour = memorysafe_core::Scope::new("t", "other-s", "n").unwrap();
        let namespace_neighbour = memorysafe_core::Scope::new("t", "s", "other-n").unwrap();
        // Empty diagnostics for `scope_embedder`: no vectors of their own, so
        // a leak surfaces as `Some` where the absence of any own row demands
        // `None` — see the doc above for why a value-level check cannot see
        // this leak when every real scope shares one embedder and dim.
        let subject_diag = memorysafe_core::Scope::new("t", "diag-s", "n").unwrap();
        let namespace_diag = memorysafe_core::Scope::new("t", "s", "diag-n").unwrap();

        // The home item is only a partial match for the probe and the
        // neighbours' bodies are exact matches, so a leak (if present) would
        // rank ahead of the home item rather than merely being one of
        // several ties — the same inversion
        // `retrieval_never_crosses_a_scope_boundary` uses, and for the same
        // reason: it removes any dependence on how ties or truncation are
        // broken.
        let probe_text = "the cat sat on the mat";
        let home_body = "the cat sat on a rug";

        let plant = |scope: &memorysafe_core::Scope, body: &str| {
            let item = memorysafe_backend::conformance::fx::item(scope, body);
            crate::items::insert(&c, &item).unwrap();
            let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed(body).unwrap());
            insert(&c, &item.id, scope, &q).unwrap();
            item.id
        };
        plant(&home, home_body);
        let subject_neighbour_id = plant(&subject_neighbour, probe_text);
        let namespace_neighbour_id = plant(&namespace_neighbour, probe_text);

        // `vectors.subject`/`vectors.namespace` are the table's *own*
        // columns, and every check above and below reads through `search`
        // (which joins `vectors` to `items` on `item_id` and filters on
        // `items.subject`/`items.namespace` — it never touches
        // `vectors.subject`/`vectors.namespace` at all) or through
        // `scope_embedder`, whose diagnostic scopes above probe different
        // coordinates than the ones a hardcoded or swapped column in
        // `insert` would alias onto. So a corrupted write here is invisible
        // to everything above: the `items` row stays correct and `search`
        // never notices. Read the stored columns back directly, with raw
        // SQL, for the two neighbours whose subject and namespace each
        // differ from the common `"s"`/`"n"` pair used everywhere else in
        // this file — so a literal hardcoded from the wrong scope cannot
        // coincidentally match either check.
        let stored_columns = |item_id: &ItemId| -> (String, String) {
            c.query_row(
                "SELECT subject, namespace FROM vectors WHERE item_id = ?1",
                params![item_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(
            stored_columns(&subject_neighbour_id),
            ("other-s".to_string(), "n".to_string()),
            "vectors.subject/vectors.namespace were not the coordinates insert \
             was given for subject_neighbour"
        );
        assert_eq!(
            stored_columns(&namespace_neighbour_id),
            ("s".to_string(), "other-n".to_string()),
            "vectors.subject/vectors.namespace were not the coordinates insert \
             was given for namespace_neighbour"
        );

        let probe =
            memorysafe_embed::QuantizedVector::from_embedding(&e.embed(probe_text).unwrap());

        // `search`: the positive control first — home's own item must come
        // back, or "the neighbours didn't leak" is true of a backend that
        // returns nothing for anyone.
        let home_hits = search(&c, &home, &probe, &unrestricted(), 10).unwrap();
        assert_eq!(
            home_hits.len(),
            1,
            "home holds exactly one item; a different count means either a \
             leak or a search that returned nothing at all"
        );
        assert_eq!(home_hits[0].0.body, home_body);
        for (scope, dimension) in [
            (&subject_neighbour, "subject"),
            (&namespace_neighbour, "namespace"),
        ] {
            assert!(
                !home_hits.iter().any(|h| h.0.scope == *scope),
                "search leaked across the {dimension} boundary into home: {:?}",
                home_hits
                    .iter()
                    .map(|h| h.0.body.clone())
                    .collect::<Vec<_>>()
            );
            // And the foreign corpus is independently readable from its own
            // scope, so the leak assertion above did not pass because the
            // write into it silently failed.
            let theirs = search(&c, scope, &probe, &unrestricted(), 10).unwrap();
            assert_eq!(
                theirs.len(),
                1,
                "the other-{dimension} corpus must exist, or 'nothing leaked \
                 from it' is true of a scope that never stored anything"
            );
            assert_eq!(theirs[0].0.body, probe_text);
        }

        // `scope_embedder`: the positive control first — home reports its
        // own stored model.
        assert_eq!(
            scope_embedder(&c, &home).unwrap(),
            Some(("deterministic-256".to_string(), 256)),
            "scope_embedder did not report home's own stored model"
        );
        // Then the two empty diagnostics, one per dimension, must each still
        // report None despite home holding a row that a broken predicate on
        // either half would let them see.
        assert_eq!(
            scope_embedder(&c, &subject_diag).unwrap(),
            None,
            "scope_embedder leaked across the subject boundary: a scope with \
             no vectors of its own, sharing home's namespace, reported a \
             model identity"
        );
        assert_eq!(
            scope_embedder(&c, &namespace_diag).unwrap(),
            None,
            "scope_embedder leaked across the namespace boundary: a scope \
             with no vectors of its own, sharing home's subject, reported a \
             model identity"
        );
    }

    #[test]
    fn deleting_an_item_cascades_to_its_vector() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let e = DeterministicEmbedder::new(256);
        let item = memorysafe_backend::conformance::fx::item(&scope, "transient");
        crate::items::insert(&c, &item).unwrap();
        let q = memorysafe_embed::QuantizedVector::from_embedding(&e.embed("transient").unwrap());
        insert(&c, &item.id, &scope, &q).unwrap();

        crate::items::delete(&c, &scope, &item.id).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "vector row survived its item");
    }
}
