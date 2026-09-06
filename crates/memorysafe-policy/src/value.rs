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

/// How useful this memory is likely to be. An explicit caller weight
/// (`Candidate.attrs["value_weight"]`) dominates when supplied, because the
/// caller knows things the corpus does not; otherwise value blends content
/// signal (specificity and lexical density, per
/// `cfg.content_specificity_weight`) with source trust, per
/// `cfg.source_trust_weight`.
pub fn score(cand: &Candidate, stats: &ScopeStats, cfg: &BaselineConfig) -> Score {
    if let Some(w) = cand.attrs.get("value_weight").and_then(|v| v.as_f64()) {
        return Score::clamped(w as f32);
    }

    let content = cfg.content_specificity_weight * specificity(cand, stats, cfg)
        + (1.0 - cfg.content_specificity_weight) * lexical_density(cand);
    let trust = source_trust(cand, cfg);
    let blended = (1.0 - cfg.source_trust_weight) * content + cfg.source_trust_weight * trust;
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
    fn an_explicit_caller_weight_is_honoured() {
        let cfg = BaselineConfig::default();
        let mut weighted = candidate_from("a short note", None);
        weighted
            .attrs
            .insert("value_weight".into(), serde_json::json!(1.0));
        let plain = candidate_from("a short note", None);
        assert!(score(&weighted, &stats(), &cfg) > score(&plain, &stats(), &cfg));
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
    fn an_out_of_range_caller_weight_is_clamped() {
        // Unlike the derived content/trust blend (which never leaves [0,1]
        // given the current formula), a caller-supplied `value_weight` is
        // not pre-validated and genuinely can be out of range before
        // `Score::clamped` runs — the one case here that actually exercises
        // the clamp rather than merely avoiding a panic.
        let cfg = BaselineConfig::default();
        let mut too_high = candidate_from("x", None);
        too_high
            .attrs
            .insert("value_weight".into(), serde_json::json!(5.0));
        assert_eq!(score(&too_high, &stats(), &cfg), Score::ONE);

        let mut too_low = candidate_from("x", None);
        too_low
            .attrs
            .insert("value_weight".into(), serde_json::json!(-3.0));
        assert_eq!(score(&too_low, &stats(), &cfg), Score::ZERO);
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
