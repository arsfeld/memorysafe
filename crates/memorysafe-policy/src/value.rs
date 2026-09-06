use crate::config::BaselineConfig;
use memorysafe_core::{Candidate, ScopeStats, Score};

/// Specificity proxy: length relative to the corpus median, saturating. A stub
/// carries little; a paragraph usually carries more. Crude but stable, and it
/// avoids needing corpus-wide token statistics on the write path.
///
/// `cfg.specificity_median_floor_bytes` guards the ratio when a scope reports
/// a degenerate (zero or tiny) median, e.g. before enough items exist for the
/// statistic to mean anything — see `ScopeStats::median_item_bytes`'s own
/// caveat.
fn specificity(cand: &Candidate, stats: &ScopeStats, cfg: &BaselineConfig) -> f32 {
    let median = stats
        .median_item_bytes
        .max(cfg.specificity_median_floor_bytes) as f32;
    let ratio = cand.byte_size as f32 / median;
    // Saturating curve: 1x median ≈ 0.5, 3x ≈ 0.75, diminishing after.
    ratio / (1.0 + ratio)
}

/// Distinct-token fraction: repetitive filler scores lower than dense prose.
fn lexical_density(cand: &Candidate) -> f32 {
    let tokens: Vec<String> = cand
        .body
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let mut unique = tokens.clone();
    unique.sort();
    unique.dedup();
    unique.len() as f32 / tokens.len() as f32
}

/// How much a source's own say-so is worth, before content is even read.
/// `source_kind` is a caller-supplied `Candidate.attrs` string, independent of
/// the engine-assigned `Source` a `MemoryItem` eventually carries. Anything
/// unset or unrecognised — including an explicit `"agent"` — falls to the
/// same neutral `cfg.source_trust_default`.
fn source_trust(cand: &Candidate, cfg: &BaselineConfig) -> f32 {
    match cand.attrs.get("source_kind").and_then(|v| v.as_str()) {
        Some("human") => cfg.source_trust_human,
        Some("tool") => cfg.source_trust_tool,
        Some("session") => cfg.source_trust_default,
        _ => cfg.source_trust_default,
    }
}

/// How useful this memory is likely to be — spec §363's "weighted
/// combination of content specificity, source trust, explicit caller
/// weight, and recency" (recency excluded: `Candidate` carries no
/// timestamps, and at admit time every candidate is equally recent —
/// recency is realised as decay during maintenance, Task 29).
///
/// The blend is built in two stages, both driven by config so a reader can
/// check each one against the spec sentence directly:
///
/// 1. `base` blends content signal (specificity and lexical density, mixed
///    by `cfg.content_specificity_weight`) with source trust, mixed by
///    `cfg.source_trust_weight` — this is the whole of `base`'s job, and it
///    is exactly what `score` returns when no caller weight is supplied.
/// 2. If `Candidate.attrs["value_weight"]` is present, it is folded in as a
///    THIRD weighted term, not a replacement for the first two: the caller
///    value (itself clamped to `[0,1]` before blending, so a 5.0 behaves
///    exactly as 1.0 would, not merely as whatever the final clamp below
///    happens to allow through) gets `cfg.caller_weight_weight`'s share, and
///    `base` keeps the rest. A caller cannot make an item maximally valuable
///    by assertion — the corpus- and trust-derived signal always keeps
///    `1.0 - cfg.caller_weight_weight` of the vote.
///
/// **Absence is neutral, not zero.** `Candidate.attrs.get(...).and_then(...)`
/// already distinguishes three inputs: the key missing, the key present with
/// a non-numeric value (treated the same as missing — a malformed hint
/// should not fail a write, but this does mean "absent" covers more than
/// literally-absent; do not fold a fourth meaning into it without noting it
/// here), and the key present with a number. Only the first two skip stage 2
/// entirely and return `base` untouched — that is what makes an
/// un-annotated item score identically to how it always did. `Some(0.0)` is
/// NOT one of those two: it is the caller explicitly asserting "this is
/// worthless," and it must drag the score down through stage 2 like any
/// other caller value. Collapsing `Some(0.0)` into "absent" (e.g. an
/// `unwrap_or(0.0)`-shaped shortcut) would be a governance system ignoring
/// an explicit instruction from the only person who bothered to give one.
pub fn score(cand: &Candidate, stats: &ScopeStats, cfg: &BaselineConfig) -> Score {
    let content = cfg.content_specificity_weight * specificity(cand, stats, cfg)
        + (1.0 - cfg.content_specificity_weight) * lexical_density(cand);
    let trust = source_trust(cand, cfg);
    let base = (1.0 - cfg.source_trust_weight) * content + cfg.source_trust_weight * trust;

    let blended = match cand.attrs.get("value_weight").and_then(|v| v.as_f64()) {
        Some(w) => {
            let caller = Score::clamped(w as f32).get();
            cfg.caller_weight_weight * caller + (1.0 - cfg.caller_weight_weight) * base
        }
        // Neutrality lives ENTIRELY in this arm returning `base` and nothing
        // else. Do not fold this into one expression with the `Some` arm
        // (e.g. `w.unwrap_or(base)` fed through the same formula, or worse,
        // `unwrap_or(0.0)`/`unwrap_or(0.5)` standing in for "no opinion") —
        // any of those computes `k*base + (1-k)*base`, which is only equal
        // to `base` by the same coincidence a fixed-point check on this
        // exact formula would need `k` to cooperate for, not by guarantee.
        // Same failure shape as `fragility.rs`'s "do not fold the two
        // branches back into one symmetric-looking expression" — a two-arm
        // function that returns a blend looks like it wants to be one line,
        // and that instinct is exactly what would quietly end this
        // guarantee, with no test able to catch it once folded.
        None => base,
    };
    Score::clamped(blended)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BaselineConfig;
    use crate::testkit::candidate_from;
    use memorysafe_core::ScopeStats;

    fn stats() -> ScopeStats {
        ScopeStats {
            item_count: 50,
            median_item_bytes: 100,
            ..Default::default()
        }
    }

    #[test]
    fn substantive_content_beats_a_stub() {
        let cfg = BaselineConfig::default();
        let rich = score(
            &candidate_from(
                "the production database migration runs at 02:00 UTC on Sundays",
                None,
            ),
            &stats(),
            &cfg,
        );
        let thin = score(&candidate_from("ok", None), &stats(), &cfg);
        assert!(rich > thin, "rich={rich:?} thin={thin:?}");
    }

    #[test]
    fn a_human_source_outranks_an_agent_source_all_else_equal() {
        // Strict `>`, and an explicit `"agent"` source (not merely an unset
        // one): `>=` would still pass if `source_trust("human")` regressed to
        // the same neutral default as an agent, which is exactly the
        // regression this test's name promises to catch.
        let cfg = BaselineConfig::default();
        let text = "the deadline moved to the fifteenth";
        let mut human = candidate_from(text, None);
        human
            .attrs
            .insert("source_kind".into(), serde_json::json!("human"));
        let mut agent = candidate_from(text, None);
        agent
            .attrs
            .insert("source_kind".into(), serde_json::json!("agent"));
        assert!(score(&human, &stats(), &cfg) > score(&agent, &stats(), &cfg));
    }

    #[test]
    fn an_explicit_caller_weight_is_one_weighted_term_not_an_override() {
        // Same fixture as `score_blends_content_and_trust_by_the_configured_weight`:
        // base (content/trust alone) = 0.62. A caller weight of 1.0 must move
        // the score TOWARD 1.0 by exactly `cfg.caller_weight_weight`'s share
        // — 0.25*1.0 + 0.75*0.62 = 0.715 — not REPLACE it with 1.0, which is
        // the defect this test exists to catch (the old early-return
        // implementation would return exactly `Score::ONE` here).
        let cfg = BaselineConfig::default(); // caller_weight_weight = 0.25
        let plain = Candidate {
            byte_size: 300,
            ..candidate_from("echo echo", None)
        };
        let mut weighted = plain.clone();
        weighted
            .attrs
            .insert("value_weight".into(), serde_json::json!(1.0));

        let base = score(&plain, &stats(), &cfg);
        let w = score(&weighted, &stats(), &cfg);

        assert!((base.get() - 0.62).abs() < 1e-6, "got {base:?}");
        assert!((w.get() - 0.715).abs() < 1e-6, "got {w:?}");
        assert!(w > base);
        assert_ne!(
            w.get(),
            1.0,
            "a caller weight of 1.0 must not override the computed score entirely"
        );
    }

    #[test]
    fn an_absent_caller_weight_differs_from_an_explicit_zero() {
        // `None` (key not present) and `Some(0.0)` (key present, value zero)
        // are different inputs to `attrs.get(...).and_then(as_f64)` and must
        // produce different scores: absence is neutral (the base blend,
        // untouched), while an explicit zero is the caller asserting "this
        // is worthless" and must pull the score DOWN through the weighted
        // blend. Collapsing the two (e.g. an `unwrap_or(0.0)` shortcut) would
        // make an explicit zero indistinguishable from silence.
        let cfg = BaselineConfig::default(); // caller_weight_weight = 0.25
        let absent_cand = Candidate {
            byte_size: 300,
            ..candidate_from("echo echo", None)
        };
        let mut zeroed_cand = absent_cand.clone();
        zeroed_cand
            .attrs
            .insert("value_weight".into(), serde_json::json!(0.0));

        let absent = score(&absent_cand, &stats(), &cfg);
        let zeroed = score(&zeroed_cand, &stats(), &cfg);

        // base = 0.62 (see `score_blends_content_and_trust_by_the_configured_weight`).
        assert!((absent.get() - 0.62).abs() < 1e-6, "got {absent:?}");
        // 0.25*0.0 + 0.75*0.62 = 0.465
        assert!((zeroed.get() - 0.465).abs() < 1e-6, "got {zeroed:?}");
        assert!(
            zeroed < absent,
            "an explicit zero must drag the score down, not sit at the neutral base"
        );
    }

    #[test]
    fn score_never_panics_on_edge_case_bodies() {
        // `Score::clamped` guarantees `[0,1]` regardless of the formula
        // inside `score`, so this cannot catch an out-of-range *value* — only
        // a panic (e.g. division by zero) on edge-case input. Named for what
        // it actually guarantees, not for a range check it cannot fail.
        let cfg = BaselineConfig::default();
        for text in ["", "x", &"word ".repeat(5000)] {
            let s = score(&candidate_from(text, None), &stats(), &cfg);
            assert!((0.0..=1.0).contains(&s.get()), "{text:?} produced {s:?}");
        }
    }

    #[test]
    fn an_out_of_range_caller_weight_is_clamped_before_blending() {
        // A caller weight outside [0,1] must be treated exactly as its
        // nearest valid extreme BEFORE it enters the blend — not merely
        // relying on the final `Score::clamped` to mop up whatever comes out
        // the other end. Those two are NOT the same thing once `value_weight`
        // is a weighted term rather than an override: with
        // `caller_weight_weight = 0.25` and base = 0.62 (see
        // `score_blends_content_and_trust_by_the_configured_weight`), a
        // weight of 5.0 blended WITHOUT input clamping gives
        // 0.25*5.0 + 0.75*0.62 = 1.715, which the final clamp saturates to
        // 1.0 — a different number from the correctly-clamped
        // 0.25*1.0 + 0.75*0.62 = 0.715. Comparing against the weight's own
        // valid extreme catches the difference; comparing against a bare
        // `Score::ONE`/`Score::ZERO` (the old assertion, valid only for the
        // early-return implementation) would not.
        let cfg = BaselineConfig::default();
        let cand_with = |w: f64| {
            let mut c = Candidate {
                byte_size: 300,
                ..candidate_from("echo echo", None)
            };
            c.attrs.insert("value_weight".into(), serde_json::json!(w));
            c
        };
        let s = stats();

        let too_high = score(&cand_with(5.0), &s, &cfg);
        let at_max = score(&cand_with(1.0), &s, &cfg);
        assert_eq!(
            too_high, at_max,
            "5.0 must score identically to 1.0, not saturate via the final clamp alone"
        );
        assert!((at_max.get() - 0.715).abs() < 1e-6, "got {at_max:?}");

        let too_low = score(&cand_with(-3.0), &s, &cfg);
        let at_min = score(&cand_with(0.0), &s, &cfg);
        assert_eq!(
            too_low, at_min,
            "-3.0 must score identically to 0.0, not saturate via the final clamp alone"
        );
        assert!((at_min.get() - 0.465).abs() < 1e-6, "got {at_min:?}");
    }

    #[test]
    fn feeding_the_base_score_back_as_caller_weight_confirms_complementary_coefficients() {
        // NOT a test of neutrality. An absent `value_weight` returning
        // `base` untouched is guaranteed BY CONSTRUCTION — see the `None =>
        // base` arm in `score` and its own guard comment — so no test can
        // fail on that arm short of someone editing it; asserting `first ==
        // base` below is a fixture sanity check, not proof of anything.
        //
        // What re-feeding a score back as its own caller weight actually
        // verifies: with `C` the content/trust base and `k` the configured
        // `caller_weight_weight`, the `Some` arm's two coefficients (`k` and
        // `1-k`) must be genuinely complementary — sum to exactly 1 and draw
        // from the SAME `base` — or `k*C + (1-k)*C` would not collapse back
        // to `C`. That holds for ANY `k`, so this does not depend on today's
        // default surviving unchanged.
        //
        // Base is fixed at 0.625, deliberately NOT 0.5. 0.5 is the fixed
        // point of a "collapse absence to a midpoint default" bug — a `None`
        // arm that used a hardcoded `w = 0.5` in place of `base` gives
        // `k*0.5 + (1-k)*0.5 = 0.5` on the first pass too, so a 0.5 fixture
        // cannot distinguish that bug from the correct implementation (this
        // is the second time this crate's own fixtures have coincided at the
        // fixed point of a symmetric operation — see `eviction.rs`'s
        // `cost_is_the_product_of_value_and_fragility`, whose fixture used
        // to sit at fragility `0.5`, the fixed point of `x -> 1-x`, for the
        // same reason). At 0.625 the same bug instead gives
        // `0.25*0.5 + 0.75*0.625 = 0.59375` on the first pass — already
        // visibly different from the correct 0.625, so the fixture-sanity
        // assertion below catches it immediately. Had that first number
        // coincidentally matched 0.625 anyway, the fixed-point check would
        // still catch it on the second pass: re-feeding 0.59375 (a REAL
        // `value_weight`, so the bug's midpoint substitution no longer
        // applies) gives `0.25*0.59375 + 0.75*0.625 = 0.6171875`, and
        // `0.59375 != 0.6171875`. Do not simplify this back to a round
        // number.
        //
        // Every number here is dyadic so both passes land on the exact same
        // f32 by construction, not by floating-point luck:
        // `content_specificity_weight` and `source_trust_weight` are
        // overridden away from the crate's non-dyadic defaults (0.6/0.2);
        // content is pinned to specificity alone (weight 1.0) at 1x the
        // corpus median (0.5), trust to an explicit human source (1.0), and
        // source_trust_weight to 0.25, giving
        // base = 0.75*0.5 + 0.25*1.0 = 0.625 = 5/8 exactly.
        let cfg = BaselineConfig {
            content_specificity_weight: 1.0,
            source_trust_weight: 0.25,
            ..BaselineConfig::default() // caller_weight_weight stays the default 0.25
        };
        let s = stats(); // median_item_bytes: 100
        let mut cand = Candidate {
            byte_size: 100, // 1x median -> specificity = 0.5
            ..candidate_from("echo echo", None)
        };
        cand.attrs
            .insert("source_kind".into(), serde_json::json!("human")); // trust = 1.0

        let first = score(&cand, &s, &cfg);
        assert_eq!(
            first.get(),
            0.625,
            "fixture arithmetic: 0.75*0.5 + 0.25*1.0 (also rules out a \
             trivially vacuous zero base)"
        );

        let mut fed_back = cand.clone();
        fed_back
            .attrs
            .insert("value_weight".into(), serde_json::json!(first.get() as f64));
        let second = score(&fed_back, &s, &cfg);

        assert_eq!(
            second, first,
            "k*C + (1-k)*C must collapse back to C: the caller-weight and \
             base-weight coefficients must be genuinely complementary"
        );
    }

    // The tests above only prove an ordering (`rich > thin`, `weighted >
    // plain`); a formula that scaled everything by the same wrong factor
    // could still satisfy them. These pin the documented calibration anchors
    // and the blend formula itself, directly on the private helpers.

    #[test]
    fn specificity_hits_its_documented_calibration_anchors() {
        let cfg = BaselineConfig::default();
        let s = stats(); // median_item_bytes: 100
        let at_median = Candidate {
            byte_size: 100,
            ..candidate_from("x", None)
        };
        assert_eq!(
            specificity(&at_median, &s, &cfg),
            0.5,
            "1x median must be 0.5"
        );

        let triple_median = Candidate {
            byte_size: 300,
            ..candidate_from("x", None)
        };
        assert_eq!(
            specificity(&triple_median, &s, &cfg),
            0.75,
            "3x median must be 0.75"
        );
    }

    #[test]
    fn lexical_density_is_the_fraction_of_distinct_tokens() {
        let repetitive = candidate_from("go go go go", None);
        assert_eq!(lexical_density(&repetitive), 0.25);

        let diverse = candidate_from("the quick brown fox", None);
        assert_eq!(lexical_density(&diverse), 1.0);
    }

    #[test]
    fn source_trust_ranks_human_above_tool_above_unspecified() {
        let cfg = BaselineConfig::default();
        let mut human = candidate_from("x", None);
        human
            .attrs
            .insert("source_kind".into(), serde_json::json!("human"));
        let mut tool = candidate_from("x", None);
        tool.attrs
            .insert("source_kind".into(), serde_json::json!("tool"));
        let unset = candidate_from("x", None);

        assert_eq!(source_trust(&human, &cfg), 1.0);
        assert_eq!(source_trust(&tool, &cfg), 0.7);
        assert_eq!(source_trust(&unset, &cfg), 0.5);
    }

    #[test]
    fn score_blends_content_and_trust_by_the_configured_weight() {
        let cfg = BaselineConfig::default(); // source_trust_weight = 0.20
        // byte_size = 3x the corpus median -> specificity = 0.75 (its own
        // calibration anchor); two tokens, one repeated -> lexical_density =
        // 0.5; no source_kind attr -> trust = 0.5 (the neutral default).
        let cand = Candidate {
            byte_size: 300,
            ..candidate_from("echo echo", None)
        };
        let s = score(&cand, &stats(), &cfg);
        // content = 0.6*0.75 + 0.4*0.5 = 0.65
        // blended = 0.8*0.65 + 0.2*0.5 = 0.62
        assert!((s.get() - 0.62).abs() < 1e-6, "got {s:?}");
    }

    // F3: every threshold promoted into `BaselineConfig` gets a test proving
    // it is actually READ from config — not merely that the default value
    // still behaves as it did when it was a hardcoded literal, which a
    // decorative field could also satisfy.

    #[test]
    fn specificity_reads_its_median_floor_from_config() {
        let degenerate_stats = ScopeStats {
            item_count: 2,
            median_item_bytes: 1, // degenerate: far below either floor
            ..Default::default()
        };
        let cfg = BaselineConfig {
            specificity_median_floor_bytes: 10,
            ..BaselineConfig::default()
        };
        let cand = Candidate {
            byte_size: 10,
            ..candidate_from("x", None)
        };
        // ratio = byte_size / floor = 10 / 10 = 1.0 -> specificity = 0.5,
        // which only happens if the CONFIGURED floor (10) was used. Against
        // the hardcoded former default (50) the ratio would be 10/50 = 0.2.
        assert_eq!(specificity(&cand, &degenerate_stats, &cfg), 0.5);
    }

    #[test]
    fn content_specificity_weight_is_configurable() {
        // At weight 1.0, content is pure specificity and density has zero
        // effect; at weight 0.0, the mirror holds. `source_trust_weight: 0.0`
        // isolates content from the trust blend entirely.
        let cand = Candidate {
            byte_size: 100,                        // 1x median -> specificity = 0.5
            ..candidate_from("go go go go", None)  // lexical_density = 0.25
        };
        let pure_specificity_cfg = BaselineConfig {
            content_specificity_weight: 1.0,
            source_trust_weight: 0.0,
            ..BaselineConfig::default()
        };
        let pure_density_cfg = BaselineConfig {
            content_specificity_weight: 0.0,
            source_trust_weight: 0.0,
            ..BaselineConfig::default()
        };
        assert_eq!(score(&cand, &stats(), &pure_specificity_cfg).get(), 0.5);
        assert_eq!(score(&cand, &stats(), &pure_density_cfg).get(), 0.25);
    }

    #[test]
    fn source_trust_values_are_configurable() {
        let cfg = BaselineConfig {
            source_trust_human: 0.81,
            source_trust_tool: 0.42,
            source_trust_default: 0.33,
            ..BaselineConfig::default()
        };
        let mut human = candidate_from("x", None);
        human
            .attrs
            .insert("source_kind".into(), serde_json::json!("human"));
        let mut tool = candidate_from("x", None);
        tool.attrs
            .insert("source_kind".into(), serde_json::json!("tool"));
        let unset = candidate_from("x", None);

        assert_eq!(source_trust(&human, &cfg), 0.81);
        assert_eq!(source_trust(&tool, &cfg), 0.42);
        assert_eq!(source_trust(&unset, &cfg), 0.33);
    }
}
