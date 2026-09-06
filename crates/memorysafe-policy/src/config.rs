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
    /// Fraction of the recall budget reserved for replay of fragile or
    /// long-unaccessed items.
    pub replay_quota: f32,
    /// MMR tradeoff: 1.0 is pure relevance, 0.0 is pure diversity.
    pub mmr_lambda: f32,
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
            replay_quota: 0.20,
            mmr_lambda: 0.70,
            value_half_life_days: 90.0,
            source_trust_weight: 0.20,
            replay_stale_days: 30.0,
            source_trust_human: 1.0,
            source_trust_tool: 0.7,
            source_trust_default: 0.5,
            content_specificity_weight: 0.6,
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
