//! Hybrid retrieval: fuses `vectors::search` and `keyword::search` into one
//! ranked candidate list, and hosts the hard-filter SQL both arms share.
//!
//! # Hard filters are enforced in SQL, never after fetching
//!
//! [`filter_sql`] builds the `WHERE` predicates for `sensitivity_ceiling`,
//! `kinds`, `tags_any`, `occurred_after`/`occurred_before` and
//! `exclude_pending_embedding`, and both `vectors::search` and
//! `keyword::search` append it to their own scope predicate. That placement
//! matters, not just its presence: both arms cap how many rows they fetch —
//! `keyword::search` with a SQL `LIMIT`, `vectors::search` by truncating a
//! full scope scan to the caller's `k` in Rust — so a filter applied only
//! *after* fetching can let excluded rows crowd real matches out of the
//! result before the filter ever runs, rather than merely leaking an excluded
//! row through. `sensitivity_ceiling_is_enforced_in_the_query` (the
//! conformance suite) and `filter_sql_narrows_every_dimension_under_limit_pressure`
//! (below) are both built in that shape for that reason.
//!
//! [`passes`] re-checks the same fields in Rust, over the fused set, as a
//! second line of defence — never the enforcement point.

use crate::vectors::AccessStats;
use crate::{estimate_tokens, keyword, vectors};
use memorysafe_backend::{BackendError, CandidateQuery, HardFilters};
use memorysafe_core::{MemoryItem, Scope, Score, ScoredCandidate};
use rusqlite::Connection;
use std::collections::HashMap;

/// Weight on the vector signal when both are present. Keyword carries the rest.
const VECTOR_WEIGHT: f32 = 0.7;

/// Applied in Rust only over rows the SQL already narrowed, as a second line of
/// defence. The SQL predicates in [`filter_sql`] are the enforcement point.
fn passes(item: &MemoryItem, f: &HardFilters) -> bool {
    if item.sensitivity > f.sensitivity_ceiling {
        return false;
    }
    if f.exclude_pending_embedding && item.pending_embedding {
        return false;
    }
    if !f.kinds.is_empty() && !f.kinds.contains(&item.kind) {
        return false;
    }
    if !f.tags_any.is_empty() && !item.tags.iter().any(|t| f.tags_any.contains(t)) {
        return false;
    }
    if let Some(after) = f.occurred_after
        && item.occurred_at.is_none_or(|t| t < after)
    {
        return false;
    }
    if let Some(before) = f.occurred_before
        && item.occurred_at.is_none_or(|t| t > before)
    {
        return false;
    }
    true
}

/// Builds the `WHERE`-clause continuation for every `HardFilters` field,
/// appending its bind values to `args` and returning the SQL fragment
/// (leading `" AND "`, so a caller appends it directly after its own scope
/// predicate). Shared by `vectors::search` and `keyword::search` so the two
/// retrieval arms cannot drift on what a hard filter means.
///
/// **An item whose `occurred_at` is `NULL` matches neither time bound.**
/// `occurred_at >= ?` and `occurred_at <= ?` are both `NULL` (never true) when
/// the column is `NULL`, which is SQL's ordinary three-valued logic and is
/// also the documented behaviour on `HardFilters::occurred_after`/
/// `occurred_before` — stated here because it would otherwise be an accident
/// of the predicate rather than a choice.
pub fn filter_sql(f: &HardFilters, args: &mut Vec<Box<dyn rusqlite::ToSql>>) -> String {
    let mut sql = String::new();
    args.push(Box::new(f.sensitivity_ceiling.ordinal()));
    sql.push_str(&format!(" AND i.sensitivity <= ?{}", args.len()));
    if f.exclude_pending_embedding {
        sql.push_str(" AND i.pending_embedding = 0");
    }
    if !f.kinds.is_empty() {
        let ph: Vec<String> = (0..f.kinds.len())
            .map(|i| format!("?{}", args.len() + i + 1))
            .collect();
        sql.push_str(&format!(" AND i.kind IN ({})", ph.join(",")));
        for k in &f.kinds {
            args.push(Box::new(k.clone()));
        }
    }
    if !f.tags_any.is_empty() {
        let ph: Vec<String> = (0..f.tags_any.len())
            .map(|i| format!("?{}", args.len() + i + 1))
            .collect();
        sql.push_str(&format!(
            " AND EXISTS (SELECT 1 FROM json_each(i.tags) WHERE json_each.value IN ({}))",
            ph.join(",")
        ));
        for t in &f.tags_any {
            args.push(Box::new(t.clone()));
        }
    }
    if let Some(after) = f.occurred_after {
        args.push(Box::new(after.unix_timestamp()));
        sql.push_str(&format!(" AND i.occurred_at >= ?{}", args.len()));
    }
    if let Some(before) = f.occurred_before {
        args.push(Box::new(before.unix_timestamp()));
        sql.push_str(&format!(" AND i.occurred_at <= ?{}", args.len()));
    }
    sql
}

/// Fuses `vectors::search` and `keyword::search` into one ranked list.
///
/// Each arm is over-fetched (`query.limit * 4`, floored at `query.limit`) so
/// fusion and the hard filters have room to narrow afterwards without
/// starving the final page — see the module doc for why the *SQL* predicates,
/// not this over-fetch margin, are what actually keeps a policy from being
/// able to widen a candidate set.
///
/// The merge key is `item.id`, not `item.body`: two distinct items sharing a
/// body must remain two candidates (`fusion_does_not_merge_distinct_items_sharing_a_body`
/// pins this).
pub fn candidates(
    conn: &Connection,
    scope: &Scope,
    query: &CandidateQuery,
) -> Result<Vec<ScoredCandidate>, BackendError> {
    if !query.is_valid() {
        return Err(BackendError::InvalidQuery(
            "a query must carry an embedding, text, or both".into(),
        ));
    }

    // Over-fetch from each source; fusion and the policy narrow afterwards.
    let fetch = query.limit.saturating_mul(4).max(query.limit);

    // `(item, access, vector_score, keyword_score)`. `access` is in the tuple
    // rather than re-read later because neither arm's item row is still
    // available by the time `ScoredCandidate` is built.
    let mut merged: HashMap<String, (MemoryItem, AccessStats, Option<f32>, Option<f32>)> =
        HashMap::new();

    if let Some(embedding) = &query.embedding
        && let Some((stored, dim)) = vectors::scope_embedder(conn, scope)?
        && stored == embedding.embedder.to_string()
        && dim == embedding.dim
    {
        let probe = memorysafe_embed::QuantizedVector::from_embedding(embedding);
        for (item, access, score) in vectors::search(conn, scope, &probe, &query.filters, fetch)? {
            merged
                .entry(item.id.as_str().to_string())
                .or_insert((item, access, None, None))
                .2 = Some(score);
        }
    }

    if let Some(text) = &query.text {
        for (item, access, score) in keyword::search(conn, scope, text, &query.filters, fetch)? {
            let e = merged
                .entry(item.id.as_str().to_string())
                .or_insert((item, access, None, None));
            e.3 = Some(score);
        }
    }

    let mut out: Vec<ScoredCandidate> = merged
        .into_values()
        .filter(|(item, _, _, _)| passes(item, &query.filters))
        .map(|(item, access, vector_score, keyword_score)| {
            let relevance = match (vector_score, keyword_score) {
                (Some(v), Some(k)) => VECTOR_WEIGHT * v + (1.0 - VECTOR_WEIGHT) * k,
                (Some(v), None) => v,
                (None, Some(k)) => k,
                (None, None) => 0.0,
            };
            ScoredCandidate {
                estimated_tokens: estimate_tokens(&item.body),
                item,
                relevance,
                vector_score,
                keyword_score,
                value: Score::ZERO,
                fragility: Score::ZERO,
                // From the item row's `last_access`/`access_count` columns,
                // carried through the merge map beside the item. `(None, 0)`
                // for a row never recalled.
                last_accessed_at: access.last_accessed_at,
                access_count: access.access_count,
            }
        })
        .collect();

    // Ties broken by id so ordering is total and pagination is reproducible.
    out.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| a.item.id.cmp(&b.item.id))
    });
    out.truncate(query.limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_backend::conformance::fx;
    use memorysafe_core::{ItemId, SensitivityLevel};
    use memorysafe_embed::{DeterministicEmbedder, Embedder, QuantizedVector};
    use time::{Duration, OffsetDateTime};

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::schema::initialise(&c).unwrap();
        c
    }

    fn embedder() -> DeterministicEmbedder {
        DeterministicEmbedder::new(256)
    }

    fn embed_item(c: &Connection, scope: &Scope, item: &memorysafe_core::MemoryItem) {
        crate::items::insert(c, item).unwrap();
        let v = QuantizedVector::from_embedding(&embedder().embed(&item.body).unwrap());
        vectors::insert(c, &item.id, scope, &v).unwrap();
    }

    /// `filter_sql` is one function serving all five `HardFilters` kinds, and
    /// only `sensitivity_ceiling` has a conformance test built to catch a
    /// predicate silently dropped from the SQL rather than merely absent from
    /// the result — see the module doc. This is that same shape
    /// (`sensitivity_ceiling_is_enforced_in_the_query`'s), run once over the
    /// other four: `kinds`, `tags_any`, `exclude_pending_embedding`, and both
    /// `occurred_after`/`occurred_before` — including a `None`-`occurred_at`
    /// item, because `HardFilters::occurred_after`'s own doc warns that a
    /// time-filter test that never seeds an explicit `occurred_at` passes
    /// vacuously against the pinned-`UNIX_EPOCH` fixture corpus.
    ///
    /// One noise group per dimension, each verbatim-identical to the query
    /// text (so it ranks at the very top under both scorers) and violating
    /// exactly *one* filter while satisfying every other — so a single
    /// dropped predicate lets exactly its own group flood the over-fetch
    /// margin (`limit * 4`) and crowd the real match out entirely, which is
    /// the "fewer than expected" failure `filter_sql_narrows_...` names.
    /// Each group is sized to 20, comfortably above `fetch = 3 * 4 = 12`, so
    /// no plausible over-fetch factor hides the leak.
    ///
    /// The query carries both an embedding and text, so a predicate dropped
    /// from either `vectors::search` or `keyword::search` is caught: whichever
    /// arm leaks floods the fused set the same way.
    #[test]
    /// **What this test reaches, and what it cannot.** It discriminates each
    /// SQL predicate by *count under limit pressure*: drop one and the
    /// over-fetch fills with rows `passes` then rejects, so fewer than `limit`
    /// come back while matching items existed — a completeness failure the
    /// Rust re-check cannot repair.
    ///
    /// That mechanism catches an **absent** predicate and is structurally
    /// incapable of catching an **off-by-one** one, because a boundary error
    /// returns approximately the right count: one row different, still at or
    /// above the limit under pressure. So "narrows every dimension" means
    /// every predicate is *present*, not that any is *exact*.
    ///
    /// Exactness is a separate test per dimension, and only `sensitivity` has
    /// one — `filter_sql_sensitivity_boundary_is_exact_on_both_arms`. Verified
    /// by mutation: `occurred_at >=` to `>` and `<=` to `<` both survive the
    /// whole suite. The other five need that shape.
    fn filter_sql_narrows_every_dimension_under_limit_pressure() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let query_text = "special filter pressure probe zebra";
        // Shares 4 of 5 tokens with the noise bodies, so it ranks just under
        // them rather than tying — the same device
        // `sensitivity_ceiling_is_enforced_in_the_query` uses.
        let real_body = "unique filter pressure probe zebra";
        let noise_body = query_text;

        let window_start = OffsetDateTime::UNIX_EPOCH + Duration::seconds(1_000);
        let window_end = OffsetDateTime::UNIX_EPOCH + Duration::seconds(2_000);
        let inside = window_start + Duration::seconds(500);
        let before_window = window_start - Duration::seconds(500);
        let after_window = window_end + Duration::seconds(500);

        let mut real = fx::item_with(
            &scope,
            real_body,
            "fact",
            &["home"],
            SensitivityLevel::Personal,
        );
        real.occurred_at = Some(inside);
        embed_item(&c, &scope, &real);

        const NOISE_PER_GROUP: usize = 20;

        // Wrong kind.
        for _ in 0..NOISE_PER_GROUP {
            let mut item = fx::item_with(
                &scope,
                noise_body,
                "other-kind",
                &["home"],
                SensitivityLevel::Personal,
            );
            item.occurred_at = Some(inside);
            embed_item(&c, &scope, &item);
        }
        // Wrong tag.
        for _ in 0..NOISE_PER_GROUP {
            let mut item = fx::item_with(
                &scope,
                noise_body,
                "fact",
                &["other-tag"],
                SensitivityLevel::Personal,
            );
            item.occurred_at = Some(inside);
            embed_item(&c, &scope, &item);
        }
        // Pending embedding.
        for _ in 0..NOISE_PER_GROUP {
            let mut item = fx::item_with(
                &scope,
                noise_body,
                "fact",
                &["home"],
                SensitivityLevel::Personal,
            );
            item.occurred_at = Some(inside);
            item.pending_embedding = true;
            embed_item(&c, &scope, &item);
        }
        // Before the time window — violates `occurred_after` only.
        for _ in 0..NOISE_PER_GROUP {
            let mut item = fx::item_with(
                &scope,
                noise_body,
                "fact",
                &["home"],
                SensitivityLevel::Personal,
            );
            item.occurred_at = Some(before_window);
            embed_item(&c, &scope, &item);
        }
        // After the time window — violates `occurred_before` only. A
        // separate group from the one above: `occurred_after` and
        // `occurred_before` are two independent predicates in `filter_sql`,
        // and a noise group violating only the first cannot catch the second
        // being dropped (confirmed by mutation: deleting the
        // `occurred_before` clause alone left every other group correctly
        // excluded and this test green until this group was added).
        for _ in 0..NOISE_PER_GROUP {
            let mut item = fx::item_with(
                &scope,
                noise_body,
                "fact",
                &["home"],
                SensitivityLevel::Personal,
            );
            item.occurred_at = Some(after_window);
            embed_item(&c, &scope, &item);
        }
        // No occurred_at at all — must be excluded once a bound is set.
        for _ in 0..NOISE_PER_GROUP {
            let item = fx::item_with(
                &scope,
                noise_body,
                "fact",
                &["home"],
                SensitivityLevel::Personal,
            );
            // `fx::item_with` leaves `occurred_at: None`, matching `fx::item`'s
            // pin — left as-is rather than set, which is the point of this
            // group.
            embed_item(&c, &scope, &item);
        }

        let filters = HardFilters {
            kinds: vec!["fact".into()],
            tags_any: vec!["home".into()],
            exclude_pending_embedding: true,
            occurred_after: Some(window_start),
            occurred_before: Some(window_end),
            sensitivity_ceiling: SensitivityLevel::Restricted,
        };
        let query = CandidateQuery {
            embedding: Some(embedder().embed(query_text).unwrap()),
            text: Some(query_text.to_string()),
            filters,
            limit: 3,
        };

        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly the one item that satisfies every filter; fewer \
             (with a small limit and a large noise pool per dimension) means a \
             predicate was dropped from filter_sql and applied after fetching \
             instead of inside the query, not merely absent from the result. \
             Got: {:?}",
            hits.iter().map(|h| &h.item.body).collect::<Vec<_>>()
        );
        assert_eq!(hits[0].item.id, real.id);
    }

    /// Fix-round finding: `ScoredCandidate::estimated_tokens` must come from
    /// `item.body`, not another field of the same item — swapping in
    /// `item.kind` (a short, near-constant string) survived the full
    /// workspace suite before this test existed, since nothing compared the
    /// reported estimate against the body's own length. The body and kind
    /// here are picked with drastically different lengths so no coincidental
    /// value collision could hide the swap.
    #[test]
    fn candidates_reports_estimated_tokens_from_the_body_not_another_field() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let long_body = "word ".repeat(100);
        let mut item = fx::item(&scope, long_body.trim());
        item.kind = "a".into();
        embed_item(&c, &scope, &item);

        let query = CandidateQuery {
            embedding: Some(embedder().embed(&item.body).unwrap()),
            text: None,
            filters: HardFilters::default(),
            limit: 1,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].estimated_tokens,
            // A literal, NOT `estimate_tokens(&item.body)`: calling the
            // function under test on both sides of the assertion is
            // tautological — any change to the formula moves both sides
            // together, and a divisor of 3.0 instead of 4.0 passed this test.
            // 499 bytes (`"word "` x100, trimmed) at 4 bytes per token.
            125,
            "estimated_tokens did not match ceil(499/4) for the item's body"
        );
        assert!(
            hits[0].estimated_tokens > 50,
            "a ~500-byte body must not report a tiny token estimate (the \
             1-byte kind would): got {}",
            hits[0].estimated_tokens
        );
    }

    /// Fix-round: the `(Some(v), None)` and `(None, Some(k))` arms of the
    /// relevance `match` — a text-only or vector-only query — were unpinned:
    /// nothing asserted `relevance` actually equals the sole available score
    /// rather than, say, always `0.0` or a weighted value that silently
    /// assumes the missing side is `0.0` instead of absent. Both ordinary
    /// query shapes, both checked.
    #[test]
    fn relevance_equals_the_sole_score_on_a_single_signal_query() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let kw_item = fx::item(&scope, "a text-only match about narwhals");
        crate::items::insert(&c, &kw_item).unwrap();
        let kw_query = CandidateQuery {
            embedding: None,
            text: Some("narwhals".to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        let kw_hits = candidates(&c, &scope, &kw_query).unwrap();
        assert_eq!(kw_hits.len(), 1);
        assert!(
            kw_hits[0].vector_score.is_none(),
            "a text-only query must not carry a vector score"
        );
        let k = kw_hits[0].keyword_score.expect("keyword score missing");
        assert_eq!(
            kw_hits[0].relevance, k,
            "relevance on a text-only query must equal the sole keyword \
             score exactly"
        );

        let vec_item = fx::item(&scope, "a vector-only match about narwhals");
        embed_item(&c, &scope, &vec_item);
        let vec_query = CandidateQuery {
            embedding: Some(embedder().embed(&vec_item.body).unwrap()),
            text: None,
            filters: HardFilters::default(),
            limit: 10,
        };
        let vec_hits = candidates(&c, &scope, &vec_query).unwrap();
        assert_eq!(vec_hits.len(), 1);
        assert!(
            vec_hits[0].keyword_score.is_none(),
            "a vector-only query must not carry a keyword score"
        );
        let v = vec_hits[0].vector_score.expect("vector score missing");
        assert_eq!(
            vec_hits[0].relevance, v,
            "relevance on a vector-only query must equal the sole vector \
             score exactly"
        );
    }

    /// Fix-round finding I2: `keyword::search`'s `AccessStats` are dead in
    /// every test that reaches `candidates` through the conformance suite,
    /// because every one of those queries carries both an embedding and
    /// text, so the vector arm always populates `merged`'s entry first and
    /// the keyword arm's own `access` is discarded by `or_insert` finding the
    /// entry already there. Confirmed by mutation: zeroing the keyword arm's
    /// `access` before `or_insert` survives the full workspace suite. The
    /// path is real — a text-only query (no `embedding`), or an embedder
    /// mismatch, skips the vector arm entirely and the keyword arm is the
    /// only source of access statistics.
    ///
    /// This test uses a **text-only** `CandidateQuery` (`embedding: None`) so
    /// the vector arm never runs at all, and manually stamps `items.last_access`
    /// / `items.access_count` (the same columns `record_recall` writes,
    /// updated directly here since this is a raw-connection crate-local test)
    /// before calling `candidates`, so the expected values are known and
    /// distinct from the zero/`None` a dropped `access` would produce.
    #[test]
    fn candidates_reports_access_statistics_on_a_text_only_query() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let item = fx::item(&scope, "a text-only recall target about cats");
        crate::items::insert(&c, &item).unwrap();
        c.execute(
            "UPDATE items SET access_count = 7, last_access = 555 WHERE id = ?1",
            rusqlite::params![item.id.as_str()],
        )
        .unwrap();

        let query = CandidateQuery {
            embedding: None,
            text: Some("cats".to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].access_count, 7,
            "the keyword arm's own access_count was not carried into the \
             fused candidate"
        );
        assert_eq!(
            hits[0].last_accessed_at,
            Some(OffsetDateTime::from_unix_timestamp(555).unwrap()),
            "the keyword arm's own last_accessed_at was not carried into the \
             fused candidate"
        );
    }

    /// Fix-round finding I1: `passes` (the Rust-side second check) sits
    /// between `filter_sql` (the enforcement point) and every test that
    /// reaches `candidates`, so a *widening* bug in `filter_sql`'s sensitivity
    /// predicate — `i.sensitivity <= ?N` mutated to `<= ?N + 1`, admitting one
    /// level above the ceiling — is caught by `passes` before any assertion
    /// on `candidates`'s output can see it. Confirmed by mutation: with that
    /// widening applied and `passes` intact, the full workspace suite stayed
    /// green; only widening it *and* neutering `passes` together made
    /// `sensitivity_ceiling_is_enforced_in_the_query` fail. `passes` is the
    /// right design (defence-in-depth), but its presence means no test that
    /// goes through `candidates` can tell "the SQL predicate is correct" from
    /// "the SQL predicate is wrong but `passes` is covering for it".
    ///
    /// This test calls `keyword::search` and `vectors::search` **directly**,
    /// below `passes`, so it observes `filter_sql`'s own output with nothing
    /// in front of it. It checks the boundary specifically, both directions:
    /// an item exactly *at* the ceiling must come back (rules out `<`, which
    /// would incorrectly exclude it) and an item exactly *one level above*
    /// must not (rules out `<= +1`, which would incorrectly admit it, and
    /// rules out the predicate being dropped entirely).
    #[test]
    fn filter_sql_sensitivity_boundary_is_exact_on_both_arms() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let query_text = "boundary probe about cats";
        let at_ceiling = fx::item_with(&scope, query_text, "fact", &[], SensitivityLevel::Personal);
        let above_ceiling =
            fx::item_with(&scope, query_text, "fact", &[], SensitivityLevel::Sensitive);
        embed_item(&c, &scope, &at_ceiling);
        embed_item(&c, &scope, &above_ceiling);

        let filters = HardFilters {
            sensitivity_ceiling: SensitivityLevel::Personal,
            ..HardFilters::default()
        };

        let kw_hits = keyword::search(&c, &scope, query_text, &filters, 10).unwrap();
        let kw_ids: Vec<_> = kw_hits.iter().map(|h| h.0.id.clone()).collect();
        assert!(
            kw_ids.contains(&at_ceiling.id),
            "keyword::search excluded an item exactly at the ceiling — the \
             predicate narrowed to `<` instead of `<=`"
        );
        assert!(
            !kw_ids.contains(&above_ceiling.id),
            "keyword::search admitted an item one level above the ceiling — \
             the predicate widened past `<=`, or was dropped"
        );

        let probe = embedder().embed(query_text).unwrap();
        let probe = QuantizedVector::from_embedding(&probe);
        let vec_hits = vectors::search(&c, &scope, &probe, &filters, 10).unwrap();
        let vec_ids: Vec<_> = vec_hits.iter().map(|h| h.0.id.clone()).collect();
        assert!(
            vec_ids.contains(&at_ceiling.id),
            "vectors::search excluded an item exactly at the ceiling — the \
             predicate narrowed to `<` instead of `<=`"
        );
        assert!(
            !vec_ids.contains(&above_ceiling.id),
            "vectors::search admitted an item one level above the ceiling — \
             the predicate widened past `<=`, or was dropped"
        );
    }

    /// The mirror image of `filter_sql_sensitivity_boundary_is_exact_on_both_arms`,
    /// and it exists for the mirror-image reason.
    ///
    /// That test reaches *below* `passes` because `passes` was covering for a
    /// widened SQL predicate. Measured during the correctness-invariants task,
    /// the asymmetry runs the other way too: neutering `passes`'s ceiling arm
    /// — `if item.sensitivity > f.sensitivity_ceiling` mutated to `if false` —
    /// left the **entire workspace suite green**, all 543 tests, because
    /// `filter_sql` was covering for `passes` in exactly the way `passes` had
    /// been covering for `filter_sql`. Defence in depth is the right design
    /// and neither layer should go; the consequence is that each needs a test
    /// that reaches it with the other one out of the way, and only one of the
    /// two existed.
    ///
    /// So this calls `passes` directly. It checks the same boundary in the
    /// same two directions: an item exactly *at* the ceiling passes (ruling
    /// out `>=`, which would wrongly reject it) and an item exactly one level
    /// *above* does not (ruling out the check being dropped, or relaxed to a
    /// higher level).
    #[test]
    fn the_rust_side_ceiling_check_is_exact_on_both_arms() {
        let scope = Scope::new("t", "s", "n").unwrap();
        let filters = HardFilters {
            sensitivity_ceiling: SensitivityLevel::Personal,
            ..HardFilters::default()
        };

        let at_ceiling = fx::item_with(&scope, "body", "fact", &[], SensitivityLevel::Personal);
        let above_ceiling = fx::item_with(&scope, "body", "fact", &[], SensitivityLevel::Sensitive);
        let below_ceiling = fx::item_with(&scope, "body", "fact", &[], SensitivityLevel::Public);

        assert!(
            passes(&at_ceiling, &filters),
            "`passes` rejected an item exactly at the ceiling — the check \
             narrowed from `>` to `>=`"
        );
        assert!(
            passes(&below_ceiling, &filters),
            "`passes` rejected an item below the ceiling"
        );
        assert!(
            !passes(&above_ceiling, &filters),
            "`passes` admitted an item one level above the ceiling — the \
             second line of defence is not defending"
        );
    }

    /// Mutant #10 in the task-22 dispatch notes: swapping which fusion slot a
    /// vector score and a keyword score land in. `hybrid_returns_both_signal_sources`
    /// (conformance) only checks both are `Some`, which a swap leaves true, so
    /// it cannot catch this — this test checks *which* score is which by
    /// deliberately decoupling them: the item's indexed body is an exact
    /// keyword match for the query (high `keyword_score`) while its stored
    /// vector is embedded from unrelated text (low `vector_score`). A swap
    /// reverses the inequality below.
    #[test]
    fn vector_and_keyword_scores_are_not_swapped_in_the_fusion() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let query_text = "distinctive keyword phrase for fusion";
        let item = fx::item(&scope, query_text);
        crate::items::insert(&c, &item).unwrap();
        // A vector embedded from something with no token overlap with the
        // query, planted directly rather than via `embed_item` — decoupling
        // the keyword-index body from the vector's source text is the whole
        // point.
        let unrelated_vector = QuantizedVector::from_embedding(
            &embedder().embed("wholly unconnected content").unwrap(),
        );
        vectors::insert(&c, &item.id, &scope, &unrelated_vector).unwrap();

        let query = CandidateQuery {
            embedding: Some(embedder().embed(query_text).unwrap()),
            text: Some(query_text.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        let vector_score = hit.vector_score.expect("vector score missing");
        let keyword_score = hit.keyword_score.expect("keyword score missing");
        assert!(
            keyword_score > vector_score,
            "an exact keyword match with a deliberately unrelated vector must \
             score higher on keyword_score than on vector_score; got \
             keyword_score={keyword_score} vector_score={vector_score} — a \
             fusion that swapped the two slots would fail this"
        );
    }

    /// Mutant #12: assigning each fetched row's score to a *different*
    /// entry in the merge map (e.g. the next row's, off by one) rather than
    /// its own. Every other identity-sensitive test in this module uses a
    /// single matching item or several tied-score items, so a shifted
    /// assignment is undetectable there: with one row, "the next row" wraps
    /// to itself; with tied scores, swapping two equal values changes
    /// nothing observable. This test uses three items with distinct,
    /// well-separated vector similarity to the probe — the same corpus shape
    /// as `retrieval::vector_search_ranks_by_similarity`, but driven through
    /// `retrieve::candidates`'s fusion rather than `neighbours`, and with an
    /// embedding-only query so only the vector arm's assignment loop is
    /// exercised. A shifted assignment reorders the fused ranking, because
    /// relevance is exactly the (misassigned) vector score in an
    /// embedding-only query — top-ranked stops being the exact match.
    #[test]
    fn candidates_reports_each_items_own_vector_score_not_anothers() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        for body in [
            "the cat sat on the mat",
            "the cat sat on a rug",
            "quarterly revenue exceeded projections",
        ] {
            embed_item(&c, &scope, &fx::item(&scope, body));
        }

        let query = CandidateQuery {
            embedding: Some(embedder().embed("the cat sat on the mat").unwrap()),
            text: None,
            filters: HardFilters::default(),
            limit: 3,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(
            hits[0].item.body, "the cat sat on the mat",
            "the exact match must rank first; a misassigned vector score \
             reorders the fused ranking"
        );
        assert_eq!(hits[2].item.body, "quarterly revenue exceeded projections");
        assert!(
            hits[0].relevance >= hits[1].relevance && hits[1].relevance >= hits[2].relevance,
            "hits must come back sorted descending by relevance: {:?}",
            hits.iter()
                .map(|h| (h.item.body.clone(), h.relevance))
                .collect::<Vec<_>>()
        );
    }

    /// Mutants #5/#6: `VECTOR_WEIGHT` collapsed to `1.0` or `0.0`, dropping
    /// one signal's contribution to `relevance`. `hybrid_returns_both_signal_sources`
    /// (conformance) only asserts `vector_score`/`keyword_score` are `Some`
    /// and `relevance > 0.0`, all of which stay true at either extreme — run
    /// against this crate, both survived it. This checks the fused *value*:
    /// the same decoupled fixture as the swap test above, with the expected
    /// relevance computed from a **hardcoded** 0.7/0.3 split rather than by
    /// reading `VECTOR_WEIGHT` back — referencing the constant under test
    /// would make the assertion track any value the constant was mutated to,
    /// proving nothing.
    #[test]
    fn fusion_weights_vector_and_keyword_by_the_documented_ratio() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();

        let query_text = "distinctive keyword phrase for weighting";
        let item = fx::item(&scope, query_text);
        crate::items::insert(&c, &item).unwrap();
        let unrelated_vector = QuantizedVector::from_embedding(
            &embedder()
                .embed("wholly unconnected content for weighting")
                .unwrap(),
        );
        vectors::insert(&c, &item.id, &scope, &unrelated_vector).unwrap();

        let query = CandidateQuery {
            embedding: Some(embedder().embed(query_text).unwrap()),
            text: Some(query_text.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        let vector_score = hit.vector_score.expect("vector score missing");
        let keyword_score = hit.keyword_score.expect("keyword score missing");
        assert!(
            (vector_score - keyword_score).abs() > 0.05,
            "the two signals must differ enough for the weighting to be \
             observable in the fused relevance: vector_score={vector_score} \
             keyword_score={keyword_score}"
        );

        let expected = 0.7 * vector_score + 0.3 * keyword_score;
        assert!(
            (hit.relevance - expected).abs() < 1e-4,
            "relevance ({}) did not match the documented 0.7 vector / 0.3 \
             keyword split (expected {expected}) — VECTOR_WEIGHT dropped one \
             signal's contribution or used the wrong ratio",
            hit.relevance
        );
    }

    /// Mutant #11: keying the merge map on `item.body` instead of
    /// `item.id.as_str()` would fuse two distinct items sharing a body into
    /// one candidate.
    #[test]
    fn fusion_does_not_merge_distinct_items_sharing_a_body() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();
        let body = "two distinct items with the exact same body";

        let a = fx::item(&scope, body);
        let b = fx::item(&scope, body);
        assert_ne!(a.id, b.id, "the fixture must mint distinct ids");
        embed_item(&c, &scope, &a);
        embed_item(&c, &scope, &b);

        let query = CandidateQuery {
            embedding: Some(embedder().embed(body).unwrap()),
            text: Some(body.to_string()),
            filters: HardFilters::default(),
            limit: 10,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        assert_eq!(
            hits.len(),
            2,
            "two items sharing a body were fused into one candidate; expected \
             both, distinct by id"
        );
        let ids: std::collections::BTreeSet<_> = hits.iter().map(|h| h.item.id.clone()).collect();
        assert_eq!(ids, [a.id, b.id].into_iter().collect());
    }

    /// Mutant #8: dropping the ascending-`ItemId` tie-break in `candidates`.
    /// Four items share one body, so under fusion their vector *and* keyword
    /// scores are exactly tied — relevance ties exactly, not approximately —
    /// and a `limit` smaller than the tied group's size can only be answered
    /// correctly by the tie-break. Literal, non-ascending-insertion-order ids,
    /// for the same reason `neighbours_break_ties_before_truncating_at_k`
    /// uses them: generated ULIDs are ordered only to millisecond resolution.
    #[test]
    fn candidates_break_ties_by_ascending_item_id() {
        let c = conn();
        let scope = Scope::new("t", "s", "n").unwrap();
        let body = "a fully tied fusion candidate";

        let ids: Vec<ItemId> = [
            "01CX5ZZKBKACTAV9WEVGEMMVR0",
            "01CX5ZZKBKACTAV9WEVGEMMVR1",
            "01CX5ZZKBKACTAV9WEVGEMMVR2",
            "01CX5ZZKBKACTAV9WEVGEMMVR3",
        ]
        .iter()
        .map(|s| ItemId::parse(s).unwrap())
        .collect();

        // Inserted out of ascending order, so a backend with no tie-break at
        // all returns the wrong pair rather than passing by coincidence.
        for i in [2usize, 0, 3, 1] {
            let item = fx::item_with_id(&scope, ids[i].clone(), body);
            embed_item(&c, &scope, &item);
        }

        let query = CandidateQuery {
            embedding: Some(embedder().embed(body).unwrap()),
            text: Some(body.to_string()),
            filters: HardFilters::default(),
            limit: 2,
        };
        let hits = candidates(&c, &scope, &query).unwrap();
        let got: Vec<ItemId> = hits.into_iter().map(|h| h.item.id).collect();
        assert_eq!(
            got,
            vec![ids[0].clone(), ids[1].clone()],
            "with every relevance tied, the two lowest ItemIds must survive a \
             limit of 2"
        );
    }
}
