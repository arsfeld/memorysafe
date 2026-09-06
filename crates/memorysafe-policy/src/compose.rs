//! `compose`: turn a scored candidate list into a `WorkingSet`.
//!
//! `Search` mode is pass-through: relevance order, packed to budget, nothing
//! governed. `WorkingSet` mode is where the policy earns its name — a
//! `replay_quota` slice of the budget is reserved for items that are fragile
//! or have gone unaccessed for a long time (so they are not left to decay out
//! of recall forever), and the remainder is filled by Maximal Marginal
//! Relevance, trading relevance against dissimilarity to what has already
//! been selected so the working set is not three paraphrases of one fact.

use crate::config::BaselineConfig;
use memorysafe_core::{
    ComposeContext, OMITTED_CAP, OmittedItem, Reason, ReasonCode, RecallMode, RecallRequest,
    ScoredCandidate, SelectedItem, WorkingSet, features,
};
use time::Duration;

/// Cheap textual proxy for "these two say the same thing", used by MMR. The
/// backend's vectors are not carried through to the policy, so similarity is
/// computed over token overlap instead — crude, but it reliably catches the
/// case MMR exists for: near-paraphrases crowding out distinct facts.
fn overlap(a: &str, b: &str) -> f32 {
    let toks =
        |s: &str| -> Vec<String> { s.split_whitespace().map(|t| t.to_lowercase()).collect() };
    let (ta, tb) = (toks(a), toks(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let shared = ta.iter().filter(|t| tb.contains(t)).count();
    // Clamped to `[0.0, 1.0]`: `shared` counts token OCCURRENCES in `a`
    // against `b`'s membership (a multiset count, not a distinct-token/set
    // intersection), so a token repeated within `a` more times than the
    // denominator can push the raw ratio above 1.0 — e.g. `overlap("the the
    // the", "the cat")` is `3 / 2 = 1.5` without this clamp. A distinct-token
    // count would also bound this at 1.0 and is arguably the more faithful
    // "similarity", but it changes the numeric result for every
    // already-pinned fixture in this module's tests (several assert an exact
    // value), whereas this clamp changes nothing for any value that was
    // already in range. Fixing "unbounded" does not require also fixing
    // "counts occurrences, not distinct tokens" — that is a separate, larger
    // change this task did not ask for.
    (shared as f32 / ta.len().min(tb.len()) as f32).min(1.0)
}

fn fits(req: &RecallRequest, tokens: u32, items: usize) -> bool {
    req.budget.fits(tokens, items)
}

/// True when an item deserves a slot it would not win on relevance: it is
/// fragile, or it has not been touched in a long time. This is `replay` from
/// the continual-learning lineage, applied to a context window. The two
/// thresholds below are the middle and lowest rungs of the fragility ladder
/// documented on `BaselineConfig` (see `replay_fragile_threshold` and
/// `replay_stale_fragile_threshold` there for the full rationale, alongside
/// `protection_fragile_threshold`, the ladder's third rung, used by `admit`).
///
/// **`fragility.get() >= cfg.replay_fragile_threshold` is reachable today,
/// not only from fixtures.** `fragility::score` returns `Score::ONE` outright
/// when a candidate has no near neighbours at all (that function's own early
/// return) — exactly the "nothing else like this exists in the corpus" case
/// this branch exists to catch, and a real, current code path, not a
/// hypothetical future producer. The "cannot exceed 0.5" ceiling on
/// `fragility::score` — a defect in the *engine's* neighbour-fetch path,
/// tracked separately — applies only to that function's non-empty-neighbours
/// branch; it says nothing about the empty-neighbours branch, which this
/// threshold is reachable through regardless of that defect. `compose` is
/// also a pure function of whatever `ScoredCandidate::fragility` it is handed
/// in any case — nothing here restricts it to values an engine has ever
/// actually produced — so this branch remains independently exercisable via
/// a hand-built fixture too (see this module's own tests), which was true
/// before and is simply no longer the only path to it.
fn replay_due(c: &ScoredCandidate, ctx: &ComposeContext, cfg: &BaselineConfig) -> bool {
    // Staleness is measured from the last *recall*, not from creation. The
    // replay quota exists to resurface what is never recalled, so an old item
    // recalled yesterday is not stale and must not win a reserved slot.
    // Measuring from `created_at` inverts the feature: it promotes exactly the
    // items that are already being used.
    //
    // A never-recalled item falls back to `created_at`, because its age is the
    // only proxy available for "how long has nobody looked at this". That
    // fallback is a decision taken here, visibly, which is why
    // `last_accessed_at` is `None` rather than a backend-invented
    // `Some(created_at)` — the policy can see it is guessing.
    let since_access = c.last_accessed_at.unwrap_or(c.item.created_at);
    let stale = ctx.now - since_access >= Duration::days(cfg.replay_stale_days as i64);
    c.fragility.get() >= cfg.replay_fragile_threshold
        || (stale && c.fragility.get() >= cfg.replay_stale_fragile_threshold)
}

pub fn working_set(
    req: &RecallRequest,
    candidates: &[ScoredCandidate],
    ctx: &ComposeContext,
    cfg: &BaselineConfig,
) -> WorkingSet {
    if candidates.is_empty() {
        return WorkingSet::empty();
    }

    let mut ranked: Vec<&ScoredCandidate> = candidates.iter().collect();
    ranked.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| a.item.id.cmp(&b.item.id))
    });

    let mut selected: Vec<SelectedItem> = Vec::new();
    let mut chosen: Vec<usize> = Vec::new();
    let mut tokens: u32 = 0;

    let push = |selected: &mut Vec<SelectedItem>,
                tokens: &mut u32,
                c: &ScoredCandidate,
                reason: Reason| {
        *tokens += c.estimated_tokens;
        selected.push(SelectedItem {
            item: c.item.clone(),
            relevance: c.relevance,
            reason,
        });
    };

    if req.mode == RecallMode::Search {
        for (i, c) in ranked.iter().enumerate() {
            if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                break;
            }
            chosen.push(i);
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    ReasonCode::HighValue,
                    "ranked by relevance in search mode",
                    features! { "relevance" => c.relevance },
                ),
            );
        }
    } else {
        // Reserve part of the budget for replay before relevance consumes it.
        let slot_budget = req.budget.max_items.unwrap_or(ranked.len());
        let replay_slots =
            ((slot_budget as f32 * cfg.replay_quota).floor() as usize).min(slot_budget);
        // The `.min(slot_budget)` above makes this trivially true today, but
        // it is also the load-bearing precondition of the equivalent-mutant
        // proof at the replay loop's `fits` call below (the one that argues
        // the item-count argument there can never be the blocker). A future
        // edit to this derivation — e.g. basing the quota on `ranked.len()`
        // instead of `slot_budget`, or dropping the `.min` — could silently
        // invalidate that proof while every existing test stays green (an
        // eviction-run of exactly this edit is what surfaced the risk: it
        // returned 3 selected items for a 2-item budget). This assertion is
        // the proof's tripwire, not a defence against a scenario reachable
        // today.
        debug_assert!(
            replay_slots <= slot_budget,
            "replay reservation must fit the item budget"
        );

        let mut replayed = 0usize;
        for (i, c) in ranked.iter().enumerate() {
            if replayed >= replay_slots {
                break;
            }
            if !replay_due(c, ctx, cfg) {
                continue;
            }
            // The item-count argument here (`selected.len() + 1`) can never be
            // what makes this `fits` call fail: `selected.len() == replayed`
            // at this point (nothing but this loop has populated `selected`
            // yet), the loop guard above already establishes `replayed <
            // replay_slots`, and `replay_slots <= slot_budget`, which is
            // itself `req.budget.max_items` whenever that is `Some` (and
            // `fits`'s item-count check is vacuously true whenever it is
            // `None`). So `selected.len() + 1 <= max_items` holds by
            // construction on every iteration that reaches this line — only
            // the token argument can turn this check false. A mutation
            // testing pass that flips `selected.len() + 1` here (e.g. to `*
            // 1`) is therefore an equivalent mutant: no fixture can kill it
            // without also changing `replay_slots`'s own derivation.
            if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                break;
            }
            chosen.push(i);
            replayed += 1;
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    ReasonCode::ReplayDue,
                    "fragile or long unaccessed; surfaced to keep it live",
                    features! {
                        "fragility" => c.fragility.get(),
                        "relevance" => c.relevance,
                        "age_days" => (ctx.now - c.item.created_at).whole_days() as f64,
                    },
                ),
            );
        }

        // Fill the rest by Maximal Marginal Relevance.
        loop {
            let mut best: Option<(usize, f32)> = None;
            for (i, c) in ranked.iter().enumerate() {
                if chosen.contains(&i) {
                    continue;
                }
                if !fits(req, tokens + c.estimated_tokens, selected.len() + 1) {
                    continue;
                }
                let max_sim = selected
                    .iter()
                    .map(|s| overlap(&c.item.body, &s.item.body))
                    .fold(0.0f32, f32::max);
                let mmr = cfg.mmr_lambda * c.relevance - (1.0 - cfg.mmr_lambda) * max_sim;
                // `ScoredCandidate::relevance` is a bare, unclamped `f32` by
                // its own contract, and warns a degenerate zero-vector cosine
                // can be NaN — reachable from the real producer, not just a
                // fixture. `total_cmp` (in the initial sort above) places a
                // positive NaN first, and NaN then poisons every comparison
                // against it: `is_none_or` seats it as `best` when `best` is
                // still `None`, and `mmr > NaN` is `false` for every
                // candidate that follows, so nothing could ever displace it.
                // Skipping a non-finite `mmr` here, rather than only checking
                // `c.relevance` for NaN, also excludes the (currently
                // unreachable but not type-excluded) case of a NaN `max_sim`.
                if !mmr.is_finite() {
                    continue;
                }
                if best.is_none_or(|(_, b)| mmr > b) {
                    best = Some((i, mmr));
                }
            }
            let Some((i, mmr)) = best else { break };
            let c = ranked[i];
            chosen.push(i);
            let max_sim = selected
                .iter()
                .map(|s| overlap(&c.item.body, &s.item.body))
                .fold(0.0f32, f32::max);
            push(
                &mut selected,
                &mut tokens,
                c,
                Reason::new(
                    if max_sim > cfg.diversity_cut_similarity {
                        ReasonCode::DiversityCut
                    } else {
                        ReasonCode::HighValue
                    },
                    "selected by relevance traded against redundancy with the set so far",
                    features! {
                        "relevance" => c.relevance,
                        "max_similarity_to_selected" => max_sim,
                        "mmr" => mmr,
                    },
                ),
            );
        }
    }

    // `omitted` reports what THIS FUNCTION considered and cut — never items
    // the backend already excluded before `compose` saw them (the sensitivity
    // ceiling, scope, tag/kind filters, and any hard time-range filter are
    // all applied in SQL, below the policy). `omitted.len()` is a count of
    // policy-level exclusions, not "how many results existed in the scope
    // minus how many came back".
    //
    // Sorted by descending relevance, ties broken by ascending `ItemId` (the
    // same rule `ranked`'s own initial sort uses), and truncated to
    // `OMITTED_CAP` (see that constant's own doc comment) AFTER sorting, not
    // before: the omissions a caller most needs explained are the ones that
    // nearly made it — a candidate cut at relevance 0.02 is unsurprising and
    // needs no explanation, while one cut at 0.89 does. Truncating first (or
    // not sorting at all) would let an arbitrary run of low-relevance filler
    // crowd the near-misses out of a capped list. The sort is re-applied
    // explicitly here, over the omitted subset alone, rather than relied on
    // implicitly from `ranked` already being sorted that way (which it is,
    // today) — so this ordering guarantee survives a future change to how
    // `chosen` is tracked, rather than depending on an invariant a reader
    // would have to trace back to the top of the function to find.
    //
    // A consequence worth stating plainly: `omitted` is NOT a representative
    // sample of why things were cut. Surfacing the most-surprising omissions
    // first, by construction, preserves which SPECIFIC omissions matter and
    // discards the DISTRIBUTION of reasons across the full cut set — a scope
    // where 900 candidates were cut for ordinary low relevance and 3 for
    // being a near-duplicate would show only those 3 (or fewer, once
    // truncated), not a proportional sample of the 900.
    let mut omitted_candidates: Vec<&ScoredCandidate> = ranked
        .iter()
        .enumerate()
        .filter(|(i, _)| !chosen.contains(i))
        .map(|(_, c)| *c)
        .collect();
    omitted_candidates.sort_by(|a, b| {
        b.relevance
            .total_cmp(&a.relevance)
            .then_with(|| a.item.id.cmp(&b.item.id))
    });
    let omitted: Vec<OmittedItem> = omitted_candidates
        .into_iter()
        .take(OMITTED_CAP)
        .map(|c| OmittedItem {
            id: c.item.id.clone(),
            reason: Reason::new(
                ReasonCode::BudgetExhausted,
                "considered but did not fit the budget",
                features! { "relevance" => c.relevance },
            ),
        })
        .collect();

    WorkingSet {
        items: selected,
        tokens_used: tokens,
        omitted,
        audit_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::{candidate, scope};
    use memorysafe_core::{
        ReasonCode, RecallBudget, RecallMode, ScopeStats, Score, SensitivityLevel,
    };
    use time::{Duration, OffsetDateTime};

    fn ctx() -> ComposeContext {
        ComposeContext {
            scope: scope(),
            stats: ScopeStats::default(),
            now: OffsetDateTime::UNIX_EPOCH + Duration::days(365),
        }
    }

    fn req(mode: RecallMode, max_items: usize) -> RecallRequest {
        RecallRequest {
            scope: scope(),
            query: Some("cats".into()),
            tags_any: vec![],
            kinds: vec![],
            // Not in the brief's own fixture: `RecallRequest` grew these two
            // time-filter fields after the brief was written (see the plan
            // discrepancy note in the task report). `None` is a no-op filter,
            // matching every candidate regardless of `occurred_at`.
            occurred_after: None,
            occurred_before: None,
            mode,
            budget: RecallBudget {
                max_tokens: Some(10_000),
                max_items: Some(max_items),
            },
            sensitivity_ceiling: SensitivityLevel::Restricted,
        }
    }

    #[test]
    fn search_mode_returns_pure_relevance_order() {
        // Rejects: composition logic leaking into Search mode (governance —
        // replay, MMR — must be bypassed entirely, not merely de-prioritised).
        // Vacuous if the three relevances did not have a strict total order;
        // they are pairwise distinct so no tie-break rule can paper over a
        // wrong sort.
        let cands = vec![
            candidate("low", 0.2),
            candidate("high", 0.9),
            candidate("mid", 0.5),
        ];
        let ws = working_set(
            &req(RecallMode::Search, 3),
            &cands,
            &ctx(),
            &BaselineConfig::default(),
        );
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(bodies, vec!["high", "mid", "low"]);
    }

    #[test]
    fn the_item_budget_is_respected_and_the_rest_is_reported_as_omitted() {
        // Rejects: a `compose` that ignores `max_items` and returns every
        // candidate, or that drops the rest silently instead of reporting it.
        // Vacuous if the token budget were also tight enough to bind — it is
        // 10,000 against 10 items of 10 tokens each (100 total), so only the
        // item cap can be the thing stopping selection at 3. This isolates
        // the item-count half of `RecallBudget::fits`'s AND from the token
        // half; `the_token_budget_is_respected` below isolates the other half.
        let cands: Vec<_> = (0..10)
            .map(|i| candidate(&format!("m{i}"), 0.9 - i as f32 * 0.05))
            .collect();
        let ws = working_set(
            &req(RecallMode::Search, 3),
            &cands,
            &ctx(),
            &BaselineConfig::default(),
        );
        assert_eq!(ws.items.len(), 3);
        assert_eq!(ws.omitted.len(), 7);
        assert!(
            ws.omitted
                .iter()
                .all(|o| o.reason.code == ReasonCode::BudgetExhausted)
        );
    }

    #[test]
    fn the_token_budget_is_respected() {
        // Rejects: a `compose` that only checks item count and never sums
        // `estimated_tokens`.
        // Vacuous if the item cap were also tight enough to bind — it is set
        // to 100 against 5 candidates, so only the token cap (250) can be the
        // thing stopping selection at 2. This isolates the token half of
        // `fits`'s AND from the item-count half isolated above.
        let mut cands = Vec::new();
        for i in 0..5 {
            let mut c = candidate(&format!("m{i}"), 0.9);
            c.estimated_tokens = 100;
            cands.push(c);
        }
        let mut r = req(RecallMode::Search, 100);
        r.budget = RecallBudget {
            max_tokens: Some(250),
            max_items: None,
        };
        let ws = working_set(&r, &cands, &ctx(), &BaselineConfig::default());
        assert_eq!(ws.items.len(), 2, "a third item would exceed 250 tokens");
        assert!(ws.tokens_used <= 250);
    }

    #[test]
    fn working_set_mode_diversifies_away_from_near_duplicates() {
        // Brief-mandated. Rejects: an MMR fill that ranks by relevance alone
        // and never applies the dissimilarity penalty, so three paraphrases
        // would crowd out the one distinct fact.
        // Vacuous if the distinct item's relevance (0.60) were high enough to
        // win on relevance alone regardless of diversity — the two isolating
        // tests below (`mmr_prefers_higher_relevance_when_similarity_is_tied`
        // and `mmr_overturns_a_small_relevance_edge_for_a_less_similar_item`)
        // pin the relevance term and the dissimilarity term of the MMR trade
        // separately; this test only proves the combination behaves as
        // advertised end to end.
        let mut cands = vec![
            candidate("the cat sat on the mat", 0.95),
            candidate("the cat sat on a mat", 0.94),
            candidate("the cat sat upon the mat", 0.93),
            candidate("quarterly revenue exceeded projections", 0.60),
        ];
        for c in &mut cands {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws = working_set(
            &req(RecallMode::WorkingSet, 2),
            &cands,
            &ctx(),
            &BaselineConfig::default(),
        );
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(bodies[0], "the cat sat on the mat");
        assert_eq!(
            bodies[1], "quarterly revenue exceeded projections",
            "MMR should prefer a distinct item over a third paraphrase"
        );
        // NOT `DiversityCut`: this candidate has zero overlap with the
        // selected seed, so `max_sim` is `0.0`, which is not above
        // `diversity_cut_similarity` (`0.5`) — its reason is `HighValue`. The
        // `DiversityCut`/`HighValue` boundary itself is covered by
        // `mmr_reason_is_diversity_cut_only_strictly_above_the_half_similarity_threshold`;
        // this assertion previously read
        // `ws.items[1].reason.code == ReasonCode::DiversityCut || bodies.len() == 2`,
        // which is vacuous — the second disjunct is guaranteed by the
        // preceding `bodies` assertions and the request's 2-item budget, so
        // it passed regardless of the (actually false) first disjunct and
        // misdescribed which reason code this path produces.
        assert_eq!(ws.items.len(), 2);
    }

    #[test]
    fn mmr_prefers_higher_relevance_when_similarity_is_tied_at_zero() {
        // Isolates the relevance term of the MMR trade: two remaining
        // candidates share the same (zero) similarity to what is already
        // selected, so `(1 - lambda) * max_sim` contributes identically
        // (zero) to both regardless of the relevance term's shape.
        //
        // Rejects: the relevance term's SIGN being inverted. Verified: with
        // `cfg.mmr_lambda * c.relevance` negated, this test fails (it picks
        // "orange kettle bicycle horizon" instead) — the two candidates'
        // correct mmr values are genuinely distinct here (0.7*0.7=0.49 vs
        // 0.7*0.3=0.21), not a tie, so a sign flip changes which one is
        // largest.
        //
        // Previously claimed, and now corrected after measuring: this does
        // NOT reject a relevance term reshaped by any transform that is
        // monotonically increasing in relevance and applied identically to
        // every candidate. Verified for two such mutations, both of which
        // leave every assertion in this test passing: zeroing the
        // coefficient (`cfg.mmr_lambda * c.relevance` -> `0.0 * c.relevance`,
        // which degenerates every candidate's mmr to the identical value
        // `0.0` here since `max_sim` is zero for all three, so the tie is
        // broken by `ranked`'s own relevance-sorted order — reproducing the
        // right answer for the wrong reason), and `*` weakened to `+`
        // (`cfg.mmr_lambda * c.relevance` -> `cfg.mmr_lambda + c.relevance`,
        // which adds the same constant `mmr_lambda` to every candidate's mmr
        // and so cannot change their relative order). Neither "dropped" nor
        // "replaced by tie-break order" is an accurate description of a
        // specific mutation; both were replaced above with what was actually
        // measured. Closing this hole would mean giving the contested
        // candidates a non-zero, UNEQUAL similarity to the seed instead of an
        // identical zero, so the exact combination shape has to matter for
        // either to win — a larger fixture change than this comment fix, not
        // made here.
        //
        // Vacuous if the two candidates' similarity to the seed differed —
        // pinned by the two `assert_eq!(overlap(...), 0.0)` guards below,
        // which fail this test outright if the fixture ever stops holding
        // similarity constant.
        let seed = candidate("alpha beta gamma delta epsilon", 0.99);
        let higher = candidate("zero seven nine plutonium", 0.7);
        let lower = candidate("orange kettle bicycle horizon", 0.3);
        assert_eq!(overlap(&higher.item.body, &seed.item.body), 0.0);
        assert_eq!(overlap(&lower.item.body, &seed.item.body), 0.0);

        let mut cands = vec![seed, higher, lower];
        for c in &mut cands {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws = working_set(
            &req(RecallMode::WorkingSet, 2),
            &cands,
            &ctx(),
            &BaselineConfig::default(),
        );
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec![
                "alpha beta gamma delta epsilon",
                "zero seven nine plutonium"
            ]
        );
    }

    #[test]
    fn mmr_overturns_a_small_relevance_edge_for_a_less_similar_item() {
        // Isolates the dissimilarity term: "similar" edges out "dissimilar"
        // by only 0.01 relevance, a margin the dissimilarity penalty must
        // overturn. Rejects: a dissimilarity term with the wrong sign (added
        // instead of subtracted) or weighted to zero (relevance-only fill) —
        // both make "similar" win instead, deterministically, not a tie.
        // Vacuous if the relevance gap were large enough to win on its own —
        // pinned by the guard below, which fails if that gap ever grows past
        // what the 0.3 dissimilarity weight (`1 - mmr_lambda`) can overturn.
        let cfg = BaselineConfig::default();
        let seed = candidate("alpha beta gamma delta epsilon", 0.99);
        let similar = candidate("alpha beta gamma delta zeta", 0.51);
        let dissimilar = candidate("quarterly revenue report numbers", 0.50);
        assert!(
            similar.relevance - dissimilar.relevance < (1.0 - cfg.mmr_lambda),
            "the relevance edge must be smaller than the dissimilarity weight can overturn"
        );
        assert!(
            overlap(&similar.item.body, &seed.item.body)
                > overlap(&dissimilar.item.body, &seed.item.body)
        );

        let mut cands = vec![seed, similar, dissimilar];
        for c in &mut cands {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws = working_set(&req(RecallMode::WorkingSet, 2), &cands, &ctx(), &cfg);
        let bodies: Vec<&str> = ws.items.iter().map(|s| s.item.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec![
                "alpha beta gamma delta epsilon",
                "quarterly revenue report numbers"
            ]
        );
    }

    #[test]
    fn overlap_treats_either_side_being_empty_as_zero_not_only_both() {
        // Rejects: the empty-body guard's `||` weakened to `&&`, which only
        // short-circuits when BOTH bodies are empty. With `&&` broken, one
        // side empty falls through to `0 / 0`, i.e. NaN, not 0.0 — and
        // `assert_eq!(_, 0.0)` fails against NaN (NaN != anything, including
        // itself), so this is not a case the assertion could pass by luck.
        // Vacuous if both sides were empty at once, which cannot distinguish
        // `||` from `&&` — both fixtures below have exactly one side empty.
        assert_eq!(overlap("", "cats and dogs"), 0.0);
        assert_eq!(overlap("cats and dogs", ""), 0.0);
    }

    #[test]
    fn overlap_divides_the_shared_count_rather_than_taking_a_remainder() {
        // Rejects: the ratio's `/` weakened to `%`. `shared / ta.len().min(tb.len())`
        // and `shared % ta.len().min(tb.len())` agree whenever `shared` is 0
        // (both 0.0) or would exactly divide, so both operands here are
        // chosen so the two operators diverge: 1 shared token out of a
        // 2-token minimum gives 0.5 under division and 1.0 under remainder.
        // Vacuous if `shared` were 0 or a multiple of the denominator —
        // pinned by the arithmetic in the comment, not by inspection alone.
        assert_eq!(overlap("a b c d", "a x"), 0.5);
    }

    #[test]
    fn overlap_is_clamped_to_one_when_a_repeated_token_would_exceed_it() {
        // Rejects: the missing `.min(1.0)` clamp on the final ratio.
        // `shared` counts token OCCURRENCES in `a` against `b`'s membership,
        // not distinct shared tokens, so a token repeated within `a` more
        // times than `b`'s (shorter) token count pushes the raw ratio above
        // 1.0: "the" appears three times in `a` and matches `b`'s single
        // "the", giving `shared = 3` against `min(3, 2) = 2`, i.e. `1.5`
        // before clamping.
        // Vacuous if `shared <= ta.len().min(tb.len())` in the fixture, which
        // is the ordinary (already-in-range) case every other `overlap` test
        // in this module exercises — this is the one fixture in the module
        // that forces the numerator past the denominator.
        assert_eq!(overlap("the the the", "the cat"), 1.0);
    }

    #[test]
    fn replay_reservation_respects_the_token_budget_on_its_first_pick() {
        // Rejects: `tokens + c.estimated_tokens` in the replay loop's budget
        // check computed as `tokens * c.estimated_tokens`. The two agree
        // whenever `tokens == 0` (the first pick always starts there, since
        // nothing precedes the replay loop), so a fixture needs the
        // multiplication's degenerate zero to actually admit something the
        // addition would have rejected: a single far-oversized item against a
        // tight token budget.
        // Vacuous if the item-count half of the same `fits` call could also
        // explain a rejection — see the doc comment at that call site: it is
        // structurally always-satisfied here (`max_items` is a generous 5),
        // so only the token half can be the blocker.
        let cfg = BaselineConfig::default();
        let mut stale = candidate("a rare fact nobody has read in a year", 0.05);
        stale.fragility = Score::ONE;
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        stale.estimated_tokens = 1000;

        let mut r = req(RecallMode::WorkingSet, 5);
        r.budget = RecallBudget {
            max_tokens: Some(100),
            max_items: Some(5),
        };
        let ws = working_set(&r, &[stale], &ctx(), &cfg);
        assert!(
            ws.items.is_empty(),
            "a 1000-token item must not fit a 100-token budget"
        );
        assert!(ws.tokens_used <= 100);
    }

    #[test]
    fn replay_loop_stops_at_the_reserved_slot_count_not_after_every_eligible_item() {
        // Rejects: `replayed += 1` computed as `replayed *= 1`, which leaves
        // `replayed` at 0 forever, so `if replayed >= replay_slots { break }`
        // never fires and every replay-eligible item gets a reserved slot
        // regardless of the quota.
        // Vacuous if fewer replay-eligible candidates existed than
        // `replay_slots` — three fragile/stale candidates are offered against
        // a quota that reserves exactly one slot, so "stops at one" and
        // "takes all three" are distinguishable outcomes, not the same count
        // either way.
        let cfg = BaselineConfig::default();
        let mut ordinary: Vec<_> = (0..2)
            .map(|i| candidate(&format!("ordinary {i}"), 0.9 - i as f32 * 0.05))
            .collect();
        for c in &mut ordinary {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let mut stale: Vec<_> = (0..3)
            .map(|i| candidate(&format!("stale {i}"), 0.05 - i as f32 * 0.01))
            .collect();
        for c in &mut stale {
            c.fragility = Score::ONE;
            c.item.created_at = OffsetDateTime::UNIX_EPOCH;
        }
        let mut cands = ordinary;
        cands.extend(stale);

        // replay_quota 0.2 * max_items 5 = exactly 1 reserved slot.
        let ws = working_set(&req(RecallMode::WorkingSet, 5), &cands, &ctx(), &cfg);
        let replay_count = ws
            .items
            .iter()
            .filter(|s| s.reason.code == ReasonCode::ReplayDue)
            .count();
        assert_eq!(
            replay_count, 1,
            "only the quota's one reserved slot should carry ReplayDue"
        );
    }

    #[test]
    fn mmr_fill_respects_the_token_budget_after_tokens_have_already_accumulated() {
        // Rejects: `tokens + c.estimated_tokens` in the MMR loop's budget
        // check computed as `tokens * c.estimated_tokens`. Indistinguishable
        // from correct on the very first MMR pick (`tokens == 0` either way),
        // so this needs a *second* pick, after 1 token has already
        // accumulated, against a 500-token item and an exactly-500 budget:
        // `1 + 500 = 501` (rejected) versus `1 * 500 = 500` (wrongly
        // admitted).
        // Vacuous if the item-count cap could also explain a rejection — it
        // is generous (10) here, so only the token cap can decide.
        let cfg = BaselineConfig::default();
        let mut seed = candidate("alpha beta gamma", 0.99);
        seed.fragility = Score::ZERO;
        seed.item.created_at = ctx().now;
        seed.estimated_tokens = 1;
        let mut big = candidate("delta epsilon zeta", 0.5);
        big.fragility = Score::ZERO;
        big.item.created_at = ctx().now;
        big.estimated_tokens = 500;

        let mut r = req(RecallMode::WorkingSet, 10);
        r.budget = RecallBudget {
            max_tokens: Some(500),
            max_items: Some(10),
        };
        let ws = working_set(&r, &[seed, big], &ctx(), &cfg);
        assert_eq!(
            ws.items.len(),
            1,
            "the second item must not fit a 500-token budget after 1 is already used"
        );
        assert!(ws.tokens_used <= 500);
    }

    #[test]
    fn mmr_ties_break_toward_the_earlier_ranked_candidate() {
        // Isolates the `>` in `best.is_none_or(|(_, b)| mmr > b)`: on an
        // exact mmr tie, `>` keeps the first-seen candidate; `>=` would let a
        // later-seen tied candidate overwrite it. Rejects: `>` weakened to
        // `>=`.
        // Vacuous if the two candidates' mmr values differed even slightly —
        // pinned by giving them identical relevance and identical (zero)
        // similarity to an empty selected set, so their mmr values are
        // bit-for-bit equal, not merely close.
        let small_id = memorysafe_core::ItemId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap();
        let large_id = memorysafe_core::ItemId::parse("01BX5ZZKBKACTAV9WEVGEMMVRZ").unwrap();
        let mut a = candidate("alpha unrelated words", 0.7);
        a.fragility = Score::ZERO;
        a.item.created_at = ctx().now;
        a.item.id = large_id.clone();
        let mut b = candidate("beta different words", 0.7);
        b.fragility = Score::ZERO;
        b.item.created_at = ctx().now;
        b.item.id = small_id.clone();

        // `ranked`'s own sort breaks the relevance tie by ascending id, so
        // `b` (small_id) is encountered first regardless of input order.
        let ws = working_set(
            &req(RecallMode::WorkingSet, 1),
            &[a, b],
            &ctx(),
            &BaselineConfig::default(),
        );
        assert_eq!(ws.items.len(), 1);
        assert_eq!(
            ws.items[0].item.id, small_id,
            "a `>` tie-break must keep the first-ranked candidate"
        );
    }

    #[test]
    fn mmr_fill_skips_a_candidate_with_a_nan_relevance_score() {
        // Rejects: the missing `mmr.is_finite()` guard. `ScoredCandidate`'s
        // own doc warns a degenerate zero-vector cosine can make `relevance`
        // NaN. The initial `total_cmp`-based sort places a positive NaN
        // FIRST in descending order, and without the guard `is_none_or`
        // would seat it as `best` on its first (empty-`best`) iteration; from
        // then on `mmr > NaN` is `false` for every real candidate that
        // follows, so nothing can ever displace it — one NaN candidate wins
        // the slot unconditionally, ahead of any genuinely relevant one.
        // Vacuous if the NaN candidate were not ranked first, or if only one
        // candidate existed — a real, ordinary candidate is included
        // specifically to prove it wins the slot instead.
        let mut nanny = candidate("a degenerate zero-vector match", 0.0);
        nanny.relevance = f32::NAN;
        nanny.fragility = Score::ZERO;
        nanny.item.created_at = ctx().now;
        let mut real = candidate("an ordinary relevant memory", 0.5);
        real.fragility = Score::ZERO;
        real.item.created_at = ctx().now;

        let ws = working_set(
            &req(RecallMode::WorkingSet, 1),
            &[nanny, real],
            &ctx(),
            &BaselineConfig::default(),
        );
        assert_eq!(ws.items.len(), 1);
        assert_eq!(
            ws.items[0].item.body, "an ordinary relevant memory",
            "a NaN-relevance candidate must not win a slot ahead of a real one"
        );
    }

    #[test]
    fn mmr_reason_is_diversity_cut_only_strictly_above_the_half_similarity_threshold() {
        // Isolates the `> 0.5` in the MMR fill's reason choice. Rejects: `>`
        // replaced with `==` or `>=` (both would misclassify the exact-0.5
        // case below as `DiversityCut`) or with `<` (would misclassify the
        // 0.8 case below as `HighValue`).
        // Vacuous if the two fixtures' `overlap` values were not pinned
        // exactly at 0.8 and 0.5 — both are asserted directly before the
        // reason-code check, so a fixture drift fails loudly here rather
        // than silently changing which branch is exercised.
        let cfg = BaselineConfig::default();

        let seed_a = candidate("one two three four five", 0.99);
        let above_half = candidate("one two three four six", 0.5);
        assert_eq!(overlap(&above_half.item.body, &seed_a.item.body), 0.8);
        let mut cands_a = vec![seed_a, above_half];
        for c in &mut cands_a {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws_a = working_set(&req(RecallMode::WorkingSet, 2), &cands_a, &ctx(), &cfg);
        assert_eq!(ws_a.items.len(), 2);
        assert_eq!(ws_a.items[1].reason.code, ReasonCode::DiversityCut);

        let seed_b = candidate("one two three four five", 0.99);
        let at_half = candidate("one two eight nine", 0.5);
        assert_eq!(overlap(&at_half.item.body, &seed_b.item.body), 0.5);
        let mut cands_b = vec![seed_b, at_half];
        for c in &mut cands_b {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let ws_b = working_set(&req(RecallMode::WorkingSet, 2), &cands_b, &ctx(), &cfg);
        assert_eq!(ws_b.items.len(), 2);
        assert_eq!(
            ws_b.items[1].reason.code,
            ReasonCode::HighValue,
            "exactly 0.5 must not count as diversity-cut; the threshold is strict"
        );
    }

    #[test]
    fn a_fragile_stale_item_wins_a_replay_slot_it_would_not_win_on_relevance() {
        // Brief-mandated, kept as written.
        //
        // What this test actually proves, corrected from the brief's own
        // framing: `stale.fragility = Score::ONE` makes `fragility >= 0.8`
        // true on its own, AND `created_at = UNIX_EPOCH` makes `stale` true
        // on its own — both disjuncts of `replay_due`'s `||` are true here.
        // Breaking either one independently (a wrong threshold on the first,
        // a dead/broken staleness path on the second) leaves the other
        // disjunct to carry the result, so THIS test cannot fail either way
        // and pins neither. What it does verify is the composition-level
        // claim: a fragile-and-stale item that would lose on relevance alone
        // (0.05 against nine competitors at 0.9) ends up in the working set
        // with a `ReplayDue` reason. The two disjuncts are isolated
        // separately below, in
        // `replay_due_high_fragility_alone_is_sufficient_when_recently_accessed`
        // and `replay_due_staleness_alone_decides_when_fragility_is_between_the_thresholds`.
        let mut relevant: Vec<_> = (0..9)
            .map(|i| candidate(&format!("relevant {i}"), 0.9))
            .collect();
        for c in &mut relevant {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let mut stale = candidate("a rare fact nobody has read in a year", 0.05);
        stale.fragility = Score::ONE;
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        relevant.push(stale);

        let ws = working_set(
            &req(RecallMode::WorkingSet, 5),
            &relevant,
            &ctx(),
            &BaselineConfig::default(),
        );
        assert!(
            ws.items.iter().any(|s| s.item.body.contains("rare fact")),
            "the replay quota did not surface the fragile stale item"
        );
        assert!(
            ws.items
                .iter()
                .any(|s| s.reason.code == ReasonCode::ReplayDue)
        );
    }

    #[test]
    fn replay_due_high_fragility_alone_is_sufficient_when_recently_accessed() {
        // Isolates the `fragility >= 0.8` disjunct: fragility 0.9 clears it
        // on its own, and `last_accessed_at` is pinned to yesterday so the
        // second disjunct (`stale && fragility >= 0.5`) is false regardless
        // of fragility. Rejects: a fragility threshold that has drifted
        // (e.g. requires `>= 1.0`), which would make this candidate false
        // and the assertion below fail.
        // Vacuous if the item were also stale — the guard assertion makes
        // that fixture error loud instead of silently passing for the wrong
        // reason.
        let cfg = BaselineConfig::default();
        let c = ctx();
        let mut candidate = candidate("recently used but fragile", 0.5);
        candidate.fragility = Score::clamped(0.9);
        candidate.item.created_at = OffsetDateTime::UNIX_EPOCH;
        candidate.last_accessed_at = Some(c.now - Duration::days(1));

        let since_access = candidate.last_accessed_at.unwrap();
        assert!(
            c.now - since_access < Duration::days(cfg.replay_stale_days as i64),
            "fixture must not be stale, or this test cannot isolate the fragility disjunct"
        );
        assert!(replay_due(&candidate, &c, &cfg));
    }

    #[test]
    fn replay_due_staleness_alone_decides_when_fragility_is_between_the_thresholds() {
        // Isolates the `stale && fragility >= 0.5` disjunct: fragility 0.6
        // sits strictly between 0.5 and 0.8, so the first disjunct is false
        // for both candidates below and only staleness can move the answer.
        // Also the discriminating pair the brief's mandated test's name
        // implies but its own body cannot check (see the note on that test).
        // Rejects: a `replay_due` that ignores `replay_stale_days`, measures
        // staleness from the wrong timestamp, or has a dead staleness path —
        // any of those make the "stale" case below false, or the "fresh"
        // case true, and the assertions fail.
        // Vacuous if fragility were >= 0.8 (first disjunct alone decides,
        // twice hit by this crate already) or < 0.5 (second disjunct's own
        // fragility guard already false) — pinned by the guard assertions.
        let cfg = BaselineConfig::default();
        let c = ctx();

        let mut stale = candidate("old and moderately fragile", 0.5);
        stale.fragility = Score::clamped(0.6);
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        assert!(stale.fragility.get() < 0.8 && stale.fragility.get() >= 0.5);
        assert!(
            replay_due(&stale, &c, &cfg),
            "0.6 fragility + stale must be replay-due"
        );

        let mut fresh = candidate("new and moderately fragile", 0.5);
        fresh.fragility = Score::clamped(0.6);
        fresh.item.created_at = c.now;
        assert!(fresh.fragility.get() < 0.8 && fresh.fragility.get() >= 0.5);
        assert!(
            !replay_due(&fresh, &c, &cfg),
            "0.6 fragility + fresh must not be replay-due"
        );
    }

    #[test]
    fn replay_due_falls_back_to_created_at_only_when_never_accessed() {
        // Pins the sentinel: `since_access = last_accessed_at.unwrap_or(created_at)`.
        // Rejects: a fallback of `unwrap_or(ctx.now)` (or any other
        // "treat unknown as fresh" default), which would make a never-
        // accessed item look recently touched and never win a replay slot.
        // Vacuous if `last_accessed_at: Some(recent)` and `None` produced the
        // same `since_access` in this fixture — they cannot: `created_at` is
        // pinned to `UNIX_EPOCH` (365 days stale) and the `Some` case is
        // pinned to yesterday (not stale), so the two branches diverge.
        let cfg = BaselineConfig::default();
        let c = ctx();

        let mut never_accessed = candidate("old, never recalled", 0.5);
        never_accessed.fragility = Score::clamped(0.6);
        never_accessed.item.created_at = OffsetDateTime::UNIX_EPOCH;
        never_accessed.last_accessed_at = None;
        assert!(
            replay_due(&never_accessed, &c, &cfg),
            "never-accessed item must fall back to created_at and read as stale"
        );

        let mut recently_accessed = never_accessed.clone();
        recently_accessed.last_accessed_at = Some(c.now - Duration::days(1));
        assert!(
            !replay_due(&recently_accessed, &c, &cfg),
            "an item accessed yesterday is not stale, regardless of how old it was created"
        );
    }

    #[test]
    fn a_zero_replay_quota_reserves_no_slots_even_for_a_maximally_fragile_stale_item() {
        // Isolates the `replay_slots = floor(slot_budget * replay_quota)`
        // arithmetic from `replay_due` itself: the stale item here is exactly
        // the brief's own maximally-replay-due fixture, so if any slot were
        // reserved it would win one. Rejects: a slot count that ignores
        // `cfg.replay_quota` (e.g. hardcoded to always reserve at least one
        // slot).
        // Vacuous if the budget were loose enough for the stale item to win a
        // slot through ordinary MMR relevance instead — five equally-relevant
        // (0.9) competitors exactly fill a 5-item budget, leaving no room and
        // no `ReplayDue` reason anywhere in the result.
        let cfg = BaselineConfig {
            replay_quota: 0.0,
            ..BaselineConfig::default()
        };
        let mut relevant: Vec<_> = (0..5)
            .map(|i| candidate(&format!("relevant {i}"), 0.9))
            .collect();
        for c in &mut relevant {
            c.fragility = Score::ZERO;
            c.item.created_at = ctx().now;
        }
        let mut stale = candidate("a rare fact nobody has read in a year", 0.05);
        stale.fragility = Score::ONE;
        stale.item.created_at = OffsetDateTime::UNIX_EPOCH;
        relevant.push(stale);

        let ws = working_set(&req(RecallMode::WorkingSet, 5), &relevant, &ctx(), &cfg);
        assert!(!ws.items.iter().any(|s| s.item.body.contains("rare fact")));
        assert!(
            !ws.items
                .iter()
                .any(|s| s.reason.code == ReasonCode::ReplayDue)
        );
    }

    #[test]
    fn omitted_items_are_ordered_by_descending_relevance_not_input_or_chosen_order() {
        // Rejects: an `omitted` list built from `candidates`' original input
        // order (or from `chosen`'s insertion order) instead of sorted by
        // descending relevance — either would scramble the omitted list
        // here, since neither matches relevance order, and this test's input
        // order is deliberately NOT already sorted.
        // Vacuous if `chosen` happened to be a contiguous prefix of the
        // relevance-sorted order (the ordinary Search-mode case) — filtering
        // a sorted sequence trivially preserves its order regardless of which
        // implementation produced it, so this needs a WorkingSet-mode
        // scenario where a low-relevance replay pick is chosen AHEAD of
        // higher-relevance items that get cut, breaking that coincidence.
        let cfg = BaselineConfig {
            replay_quota: 0.5,
            ..BaselineConfig::default()
        };
        let mut a = candidate("a highly relevant memory", 0.9);
        a.fragility = Score::ZERO;
        a.item.created_at = ctx().now;
        let mut b = candidate("a moderately relevant memory", 0.8);
        b.fragility = Score::ZERO;
        b.item.created_at = ctx().now;
        let b_id = b.item.id.clone();
        let mut c = candidate("a less relevant memory", 0.7);
        c.fragility = Score::ZERO;
        c.item.created_at = ctx().now;
        let c_id = c.item.id.clone();
        let mut d = candidate("a rare fact nobody has read in a year", 0.1);
        d.fragility = Score::ONE;
        d.item.created_at = OffsetDateTime::UNIX_EPOCH;

        // Deliberately not already sorted by relevance.
        let cands = vec![c, a, d, b];
        let ws = working_set(&req(RecallMode::WorkingSet, 2), &cands, &ctx(), &cfg);

        // D wins the one reserved replay slot despite the lowest relevance;
        // A wins the other slot on relevance. B and C are cut and must come
        // back sorted descending: B (0.8) before C (0.7) — the reverse of
        // this test's own input order for those two.
        assert!(
            ws.items
                .iter()
                .any(|s| s.reason.code == ReasonCode::ReplayDue)
        );
        assert_eq!(ws.omitted.len(), 2);
        assert_eq!(
            ws.omitted[0].id, b_id,
            "the higher-relevance omission must come first"
        );
        assert_eq!(ws.omitted[1].id, c_id);
    }

    #[test]
    fn the_omitted_list_is_capped() {
        // Rejects: an `omitted` vector left unbounded, which lets a wide
        // recall over a large corpus blow up the response.
        // Vacuous if fewer than `OMITTED_CAP + 1` candidates missed the
        // budget — 200 candidates against a 1-item budget guarantees 199
        // omissions, comfortably past the cap.
        let cands: Vec<_> = (0..200).map(|i| candidate(&format!("m{i}"), 0.5)).collect();
        let ws = working_set(
            &req(RecallMode::Search, 1),
            &cands,
            &ctx(),
            &BaselineConfig::default(),
        );
        assert_eq!(ws.omitted.len(), memorysafe_core::OMITTED_CAP);
    }

    #[test]
    fn an_empty_candidate_set_yields_an_empty_working_set() {
        // Rejects: a `compose` that panics or fabricates output on empty
        // input instead of taking the explicit early-return path.
        // Vacuous if `WorkingSet::empty()`'s own fields were wrong rather
        // than this call path — that is `WorkingSet::empty`'s own concern
        // (tested in `memorysafe-core`); this test only checks `working_set`
        // reaches it.
        let ws = working_set(
            &req(RecallMode::WorkingSet, 5),
            &[],
            &ctx(),
            &BaselineConfig::default(),
        );
        assert!(ws.items.is_empty());
        assert!(ws.omitted.is_empty());
        assert_eq!(ws.tokens_used, 0);
    }
}
