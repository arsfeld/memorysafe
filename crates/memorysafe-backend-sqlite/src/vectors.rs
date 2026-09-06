//! The `vectors` table and exact brute-force nearest-neighbour search.
//!
//! No ANN index: per-scope corpora are small, results are exact, and there is
//! nothing to rebuild after every write. `scope_embedder` reports which model
//! a scope's vectors were produced by, so a probe from a different model is
//! rejected rather than silently compared — see `lib.rs`'s `neighbours`.

use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::tenant::SqlResultExt;
use memorysafe_backend::BackendError;
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
pub fn search(
    conn: &Connection,
    scope: &Scope,
    probe: &QuantizedVector,
    k: usize,
) -> Result<Vec<(MemoryItem, AccessStats, f32)>, BackendError> {
    if k == 0 {
        return Ok(vec![]);
    }
    let sql = format!(
        // `last_access`/`access_count` are named explicitly: `ITEM_COLUMNS`
        // contains neither, and `row_to_item` does not read them.
        "SELECT i.rowid, {cols}, i.last_access AS last_access,
                i.access_count AS access_count,
                v.embedder AS v_embedder, v.dim AS v_dim,
                v.scale AS v_scale, v.q AS v_q
         FROM items i JOIN vectors v ON v.item_id = i.id
         WHERE i.subject = ?1 AND i.namespace = ?2
           AND v.embedder = ?3 AND v.dim = ?4",
        cols = ITEM_COLUMNS
            .split(", ")
            .map(|c| format!("i.{c} AS {c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let tenant = scope.tenant.as_str().to_string();
    let mut stmt = conn.prepare(&sql).sql()?;
    let rows = stmt
        .query_map(
            params![
                scope.subject.as_str(),
                scope.namespace.as_str(),
                probe.embedder.to_string(),
                probe.dim as i64,
            ],
            move |r| {
                let item = row_to_item(r, &tenant)?;
                let access = access_stats(r)?;
                let scale: f64 = r.get("v_scale")?;
                let bytes: Vec<u8> = r.get("v_q")?;
                Ok((item, access, scale as f32, bytes))
            },
        )
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
        let hits = search(&c, &scope, &probe, 2).unwrap();

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

    /// `search` and `scope_embedder` must not cross a subject boundary within
    /// one tenant. The tenant is the file — that isolation is structural —
    /// but subject/namespace is a query predicate here and nothing structural
    /// enforces it.
    ///
    /// `isolation::retrieval_never_crosses_a_scope_boundary` owns this
    /// property through the trait, but it reads through *both*
    /// `retrieve_candidates` and `neighbours`, so it cannot bind until
    /// `retrieve_candidates` exists (Task 22) — a deferral recorded in
    /// `tests/conformance.rs`'s module doc. Until then, this crate-local test
    /// is the only thing that can fail if either predicate is dropped:
    /// neutralising `search`'s `WHERE i.subject = ?1 AND i.namespace = ?2`
    /// (or `scope_embedder`'s `WHERE subject=?1 AND namespace=?2`) left the
    /// whole workspace green before this test existed.
    ///
    /// Both scopes share the same embedder and dimension on purpose: if the
    /// subject/namespace predicate were the only thing keeping them apart,
    /// dropping it would let `elsewhere`'s probe match `home`'s row exactly
    /// (the probe text is identical to the stored body), and `scope_embedder`
    /// would report `home`'s model identity for a scope that holds no
    /// vectors of its own.
    #[test]
    fn search_and_scope_embedder_are_scoped_by_subject_and_namespace() {
        let c = conn();
        let home = memorysafe_core::Scope::new("t", "s", "n").unwrap();
        let elsewhere = memorysafe_core::Scope::new("t", "other-s", "n").unwrap();
        let e = DeterministicEmbedder::new(256);

        let item = memorysafe_backend::conformance::fx::item(&home, "alpha memory");
        crate::items::insert(&c, &item).unwrap();
        let q =
            memorysafe_embed::QuantizedVector::from_embedding(&e.embed("alpha memory").unwrap());
        insert(&c, &item.id, &home, &q).unwrap();

        // A probe from a different subject, same tenant, must not see
        // `home`'s vector — even though the probe matches it exactly.
        let probe =
            memorysafe_embed::QuantizedVector::from_embedding(&e.embed("alpha memory").unwrap());
        let hits = search(&c, &elsewhere, &probe, 5).unwrap();
        assert!(
            hits.is_empty(),
            "search leaked a vector across a subject boundary: {:?}",
            hits.iter().map(|h| h.0.body.clone()).collect::<Vec<_>>()
        );

        // A scope with no vectors of its own must not report another
        // subject's stored model identity.
        assert_eq!(
            scope_embedder(&c, &elsewhere).unwrap(),
            None,
            "scope_embedder leaked another subject's embedder identity"
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
