//! FTS5 keyword search over `items_fts`.
//!
//! `items_fts` is external-content over `items.rowid` (see `schema.rs`), so a
//! hit is joined back to its item row by `rowid`. Every user-supplied query
//! goes through [`escape_fts_query`] before it reaches SQLite: FTS5 treats
//! `"`, `*`, `NEAR`, `AND`, `OR` and parentheses as operators, so raw text is
//! both a query-injection risk (a crafted query can turn into `MATCH
//! everything`) and a crash risk (an unbalanced `(` is a syntax error FTS5
//! rejects outright).

use crate::items::{ITEM_COLUMNS, row_to_item};
use crate::retrieve::filter_sql;
use crate::tenant::SqlResultExt;
use crate::vectors::{self, AccessStats};
use memorysafe_backend::{BackendError, HardFilters};
use memorysafe_core::{MemoryItem, Scope};
use rusqlite::Connection;

/// Turns arbitrary user text into a safe FTS5 MATCH expression.
///
/// Every whitespace-separated term is wrapped in double quotes, which makes
/// FTS5 treat it as a literal string rather than an operator; internal quotes
/// are doubled per FTS5's own escaping rule (`"` becomes `""`). Terms are
/// OR-ed so a multi-word query behaves like "any of these", which is what
/// hybrid retrieval wants — precision comes from the vector side.
///
/// Returns `None` for empty or whitespace-only input, so a caller can skip
/// the query entirely rather than asking FTS5 to match an empty expression.
pub fn escape_fts_query(raw: &str) -> Option<String> {
    let terms: Vec<String> = raw
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

/// Returns `(item, access, bm25_score)` where a higher score is a better
/// match. The access columns ride alongside for the same reason they do in
/// `vectors::search` — see `vectors::AccessStats`. Both arms of the fusion must
/// carry them, or a keyword-only hit reaches `ScoredCandidate` with nothing to
/// populate `last_accessed_at` from.
///
/// `filters` is applied in the SQL `WHERE` clause via `filter_sql`, before
/// `LIMIT`: a filter enforced only after the row set is already capped at
/// `limit` can let excluded rows crowd real matches out of the result
/// entirely — see `filter_sql`'s doc.
pub fn search(
    conn: &Connection,
    scope: &Scope,
    raw_query: &str,
    filters: &HardFilters,
    limit: usize,
) -> Result<Vec<(MemoryItem, AccessStats, f32)>, BackendError> {
    let Some(expr) = escape_fts_query(raw_query) else {
        return Ok(vec![]);
    };

    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
        Box::new(expr),
        Box::new(scope.subject.as_str().to_string()),
        Box::new(scope.namespace.as_str().to_string()),
    ];
    let filter_clause = filter_sql(filters, &mut args);
    args.push(Box::new(limit as i64));
    let limit_placeholder = args.len();

    let sql = format!(
        "SELECT {cols}, i.last_access AS last_access,
                i.access_count AS access_count, bm25(items_fts) AS bm25
         FROM items_fts
         JOIN items i ON i.rowid = items_fts.rowid
         WHERE items_fts MATCH ?1 AND i.subject = ?2 AND i.namespace = ?3{filter_clause}
         ORDER BY bm25 ASC LIMIT ?{limit_placeholder}",
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
            let access = vectors::access_stats(r)?;
            let bm25: f64 = r.get("bm25")?;
            Ok((item, access, bm25 as f32))
        })
        .sql()?;

    // bm25() returns negative values, more negative meaning a better match.
    // Flip and squash into (0, 1] so it can be fused with cosine.
    let mut out = Vec::new();
    for row in rows {
        let (item, access, bm25) = row.sql()?;
        let positive = (-bm25).max(0.0);
        out.push((item, access, positive / (1.0 + positive)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_become_quoted_terms() {
        assert_eq!(
            escape_fts_query("zstandard").as_deref(),
            Some("\"zstandard\"")
        );
        assert_eq!(
            escape_fts_query("cat mat").as_deref(),
            Some("\"cat\" OR \"mat\"")
        );
    }

    #[test]
    fn operators_are_neutralised_rather_than_interpreted() {
        // "AND"/"OR"/"NEAR" must be searched for, not executed.
        assert_eq!(
            escape_fts_query("a AND b").as_deref(),
            Some("\"a\" OR \"AND\" OR \"b\"")
        );
        assert_eq!(escape_fts_query("*").as_deref(), Some("\"*\""));
        assert_eq!(
            escape_fts_query("(unbalanced").as_deref(),
            Some("\"(unbalanced\"")
        );
    }

    #[test]
    fn embedded_quotes_are_doubled_so_the_term_stays_one_token() {
        assert_eq!(
            escape_fts_query("say \"hi\"").as_deref(),
            Some("\"say\" OR \"\"\"hi\"\"\"")
        );
    }

    #[test]
    fn empty_or_punctuation_only_input_yields_no_query() {
        assert_eq!(escape_fts_query(""), None);
        assert_eq!(escape_fts_query("   "), None);
        assert_eq!(escape_fts_query("\""), Some("\"\"\"\"".to_string()));
    }

    /// `search`'s row-to-score association must not scramble under multiple
    /// matches of differing strength — mutant #13 in the task-22 dispatch
    /// notes ("`keyword::search` returns the right rows with another row's
    /// `bm25`"): scores stay in a plausible range and the set stays sorted,
    /// but the wrong row gets the wrong score, so a same-set/same-order
    /// assertion cannot see it. This checks identity, not just ordering: the
    /// item with more matching terms must both rank first *and* be the one
    /// whose body actually shares more terms with the query.
    #[test]
    fn search_associates_each_row_with_its_own_score_not_anothers() {
        let c = conn();
        let scope = memorysafe_core::Scope::new("t", "s", "n").unwrap();

        let strong = memorysafe_backend::conformance::fx::item(&scope, "alpha bravo charlie delta");
        crate::items::insert(&c, &strong).unwrap();
        let weak = memorysafe_backend::conformance::fx::item(
            &scope,
            "alpha unrelated unrelated unrelated",
        );
        crate::items::insert(&c, &weak).unwrap();

        let hits = search(
            &c,
            &scope,
            "alpha bravo charlie delta",
            &HardFilters::default(),
            10,
        )
        .unwrap();
        assert_eq!(
            hits.len(),
            2,
            "both items must match on the shared term 'alpha'"
        );
        assert_eq!(
            hits[0].0.id, strong.id,
            "the four-term match must outrank the one-term match"
        );
        assert_eq!(hits[1].0.id, weak.id);
        assert!(
            hits[0].2 > hits[1].2,
            "the stronger match's own score must be the higher one: {} vs {}",
            hits[0].2,
            hits[1].2
        );
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::schema::initialise(&c).unwrap();
        c
    }
}
