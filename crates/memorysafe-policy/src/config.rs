use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    NearDuplicate,
    Mergeable,
    Novel,
}

/// Every threshold the baseline uses, in one place. Defaults are the values
/// documented in the spec; tenants may override them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselineConfig {
    /// At or above this, the closest neighbour counts as a near-duplicate —
    /// decided by cosine similarity, not content-digest identity, so two
    /// items at this threshold can still have different digests — and the
    /// write is rejected.
    pub duplicate_threshold: f32,
    /// At or above this (but below `duplicate_threshold`), merge.
    pub merge_threshold: f32,
    /// Neighbours below this are not reported as near-duplicates.
    pub near_duplicate_floor: f32,

    // --- The fragility ladder ---
    //
    // Three gates key off fragility, at three different bars, spending three
    // different kinds of budget. Documented together because the numbers
    // only make sense side by side, and the parallel `*_fragile_threshold`
    // naming exists to keep that visible rather than let three thresholds
    // that started related drift into looking arbitrary and inviting someone
    // to "unify" them:
    //
    //   | gate                              | value | action                             |
    //   |-----------------------------------|-------|------------------------------------|
    //   | `protection_fragile_threshold`    | 0.85  | block eviction outright (`admit`)  |
    //   | `replay_fragile_threshold`        | 0.80  | spend a reserved replay slot,      |
    //   |                                   |       | fragility alone                    |
    //   | `replay_stale_fragile_threshold`  | 0.50  | spend a reserved replay slot,      |
    //   |                                   |       | staleness corroborating            |
    //
    // The rule: the more corroborating evidence, the lower the fragility bar
    // needs to be, and the bars differ because the actions differ in cost.
    // Blocking an eviction is the most expensive and least reversible of the
    // three — it can starve capacity reclaim outright — so it demands the
    // highest bar and no corroborating evidence. Spending one of a handful of
    // reserved replay slots is cheap and reversible at the next recall, so
    // fragility alone justifies it at a lower bar. When staleness also
    // independently argues the item is being lost track of, less fragility
    // evidence is needed, because the second signal corroborates the same
    // conclusion rather than standing alone. Do not collapse these into one
    // number: they gate different actions at different costs, on purpose.
    /// `admit::decide`: fragility at or above this earns a retained item a
    /// protection window rather than `Protection::Normal` — the highest rung
    /// of the fragility ladder documented above, since blocking an eviction
    /// outright is the most expensive of the three actions and gets no
    /// corroborating evidence.
    pub protection_fragile_threshold: f32,
    /// `compose::replay_due`: fragility at or above this alone reserves a
    /// replay slot for the item, no staleness required — the middle rung of
    /// the fragility ladder documented above.
    pub replay_fragile_threshold: f32,
    /// `compose::replay_due`: fragility at or above this, WITH staleness
    /// corroborating, also reserves a replay slot — the lowest rung of the
    /// fragility ladder documented above, reachable only alongside
    /// independent evidence of long non-access.
    pub replay_stale_fragile_threshold: f32,
    /// `admit::decide`: length, in whole days, of the protection window a
    /// fragile item is granted.
    pub protection_window_days: i64,
    /// Fraction of the recall budget reserved for replay of fragile or
    /// long-unaccessed items.
    pub replay_quota: f32,
    /// MMR tradeoff: 1.0 is pure relevance, 0.0 is pure diversity.
    pub mmr_lambda: f32,
    /// `compose::working_set`'s MMR fill: an item whose similarity to what is
    /// already selected exceeds this is tagged `ReasonCode::DiversityCut`
    /// rather than `ReasonCode::HighValue` in its evidence. Not a fragility
    /// gate and not part of the ladder above — it never changes which item is
    /// chosen, only how the choice is explained in the audit trail, so a
    /// tenant override here is cosmetic rather than behavioural.
    pub diversity_cut_similarity: f32,
    /// Half-life in days for value decay during maintenance.
    pub value_half_life_days: f32,
    /// Weight of source trust in the value score.
    pub source_trust_weight: f32,
    /// Days without access before an item counts as replay-due.
    pub replay_stale_days: f32,
    /// `value::source_trust` for an explicit `source_kind: "human"`.
    pub source_trust_human: f32,
    /// `value::source_trust` for an explicit `source_kind: "tool"`.
    pub source_trust_tool: f32,
    /// `value::source_trust` for `source_kind: "session"`, unset, or
    /// anything else unrecognised — the neutral default.
    pub source_trust_default: f32,
    /// `value::score`'s content-signal tradeoff: 1.0 is pure specificity
    /// (length relative to the corpus median), 0.0 is pure lexical density.
    pub content_specificity_weight: f32,
    /// Share of `value::score`'s blend given to an explicit caller-supplied
    /// `Candidate.attrs["value_weight"]`, when present — spec §363's
    /// "explicit caller weight", one weighted term alongside content
    /// specificity and source trust (and, later, recency — Task 29), never a
    /// substitute for them. The remaining `1.0 - this` share stays split
    /// between content and source trust in their own existing ratio, so an
    /// absent `value_weight` reproduces exactly what those two alone would
    /// produce — see `value::score`'s doc comment for the full shape.
    pub caller_weight_weight: f32,
    /// `value::specificity`'s floor under a scope's reported median item
    /// size, guarding the ratio when a scope has too little data (or a
    /// degenerate `0`) for the median to mean anything yet — see
    /// `ScopeStats::median_item_bytes`'s own caveat.
    pub specificity_median_floor_bytes: u64,
    /// Minimum length of an unbroken alphanumeric-ish run for
    /// `sensitivity::looks_like_secret_token` to treat it as the shape of a
    /// bearer token or API key.
    pub credential_token_min_len: usize,
    /// Minimum digit count for `sensitivity::looks_like_phone` to treat a
    /// body as containing a phone number.
    pub phone_digit_min_count: usize,
    /// `sensitivity::assess`'s confidence when a credential pattern or
    /// token shape is detected.
    pub sensitivity_credential_confidence: f32,
    /// `sensitivity::assess`'s confidence for a health, financial, or legal
    /// lexicon match.
    pub sensitivity_category_confidence: f32,
    /// `sensitivity::assess`'s confidence for a detected email or phone
    /// number.
    pub sensitivity_pii_confidence: f32,
    /// `sensitivity::assess`'s confidence when nothing is detected — also
    /// the floor every other confidence above is maxed against.
    pub sensitivity_baseline_confidence: f32,
}

impl Default for BaselineConfig {
    fn default() -> Self {
        Self {
            duplicate_threshold: 0.98,
            merge_threshold: 0.93,
            near_duplicate_floor: 0.30,
            protection_fragile_threshold: 0.85,
            replay_fragile_threshold: 0.80,
            replay_stale_fragile_threshold: 0.50,
            protection_window_days: 30,
            replay_quota: 0.20,
            mmr_lambda: 0.70,
            diversity_cut_similarity: 0.50,
            value_half_life_days: 90.0,
            source_trust_weight: 0.20,
            replay_stale_days: 30.0,
            source_trust_human: 1.0,
            source_trust_tool: 0.7,
            source_trust_default: 0.5,
            content_specificity_weight: 0.6,
            caller_weight_weight: 0.25,
            specificity_median_floor_bytes: 50,
            credential_token_min_len: 20,
            phone_digit_min_count: 10,
            sensitivity_credential_confidence: 0.9,
            sensitivity_category_confidence: 0.7,
            sensitivity_pii_confidence: 0.6,
            sensitivity_baseline_confidence: 0.5,
        }
    }
}

impl BaselineConfig {
    pub fn classify(&self, similarity: f32) -> Verdict {
        if similarity >= self.duplicate_threshold {
            Verdict::NearDuplicate
        } else if similarity >= self.merge_threshold {
            Verdict::Mergeable
        } else {
            Verdict::Novel
        }
    }
}
