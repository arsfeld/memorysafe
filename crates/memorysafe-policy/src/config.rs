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
    ///
    /// **Deliberately unchanged when the MMR fill moved from a pairwise `max`
    /// to union coverage**, which is not the same as unexamined.
    ///
    /// **The strongest reason is measurement-independent**, so take it first:
    /// the pairwise `max` was a biased-LOW estimator of "how much of this
    /// candidate is already covered", and union coverage is the unbiased one.
    /// Damping a correction with the tradeoff knob is the wrong response to
    /// fixing a biased estimator — it re-suppresses exactly the thing the fix
    /// recovered. Nothing below can overturn that; the measurements only bound
    /// how much room there was to argue the other way.
    ///
    /// **What actually changed, stated precisely.** Union coverage is `>=` the
    /// pairwise max always, so the penalty rose. It did NOT rise by the same
    /// amount on every candidate: `union - max` is zero where a candidate's
    /// covered tokens sit inside a single selected item and large where they
    /// are split across several, which is the whole point of the change.
    /// Measured, `sd(union - max)` across candidates reaches `0.08` in prose
    /// and narrow-topic scopes but stays under `0.03` in terse ones, and the
    /// share of candidates with `union == max` collapses from 100% at one
    /// selected item to 0.8% (prose) and 0.0% (narrow topic) by five — while
    /// STAYING above 92% for terse, distinctive bodies. So the increment is
    /// per-candidate and shape-dependent, which is why the question cannot be
    /// settled by looking at the penalty's level.
    ///
    /// **The statistic that settles it is the argmax, not the spread.** An
    /// earlier revision of this note derived a "hold ranking power fixed"
    /// range of `0.70..=0.84` from `(1 - lambda_new) * sd(union) =
    /// (1 - lambda_old) * sd(max)`. That is a heuristic, not a fact: `sd` is
    /// whole-set spread, MMR takes an argmax, and extra spread landing
    /// mid-pack moves `sd` without moving any pick. Measured directly — the
    /// rate at which the union form's pick differs from the pairwise-max
    /// form's pick at the shipped `0.70`, swept over lambda — the heuristic
    /// does not survive:
    ///
    /// ```text
    ///                       lambda:  0.70   0.75   0.80   0.84   0.90   0.95
    ///   prose-like,   5 selected:   17.2%  13.7%  11.5%  10.4%  10.9%  12.7%
    ///   narrow topic, 5 selected:   19.3%  16.3%  14.1%  13.2%  13.3%  14.5%
    ///   terse facts,  5 selected:    0.7%   2.2%   3.4%   4.5%   6.3%   7.9%
    ///   prose-like,   1 selected:    0.0%   3.4%   6.4%   8.7%  11.9%  14.3%
    /// ```
    ///
    /// Three things fall out, and each independently blocks a move:
    ///
    /// 1. **No lambda drives the divergence to zero.** It bottoms out near
    ///    10-13%, because most of the change is a different candidate WINNING,
    ///    not a scale factor lambda can undo. The `0.70..=0.84` range promised
    ///    a recovery that is not available at any price.
    /// 2. **The shapes disagree on the DIRECTION.** Terse, distinctive bodies
    ///    are already optimal at `0.70` and get monotonically worse above it;
    ///    prose and narrow-topic scopes have a shallow minimum near
    ///    `0.84..=0.90`. One constant cannot serve both, and `0.70` is the only
    ///    value that is never the worst choice.
    /// 3. **At one selected item the two forms are IDENTICAL** (a union over a
    ///    single set is that set), so the divergence at `0.70` is 0.0% in
    ///    EVERY shape, and every lambda above it introduces divergence where
    ///    there was none — at `0.95`, 14.3% of picks in prose, 17.5% in a
    ///    narrow-topic scope, 2.3% in a terse one. The MMR fill faces an empty
    ///    or one-item selected set on its first picks in every recall, so a
    ///    lambda raised to help later rounds actively corrupts the earliest
    ///    and most consequential ones.
    ///
    /// Finally, much of the effect is a MEASUREMENT ARTIFACT rather than
    /// content redundancy: the shapes above differ chiefly in function-word
    /// density (45% vs 15%), and `similarity`'s tokenizer removes no
    /// stopwords. The lever for that is the tokenizer, not a tuned constant
    /// here that would hide the artifact instead of leaving it visible.
    ///
    /// Stated plainly, since a documented default invites being read as a
    /// derived one: `0.70` is the conventional relevance-leaning MMR lambda,
    /// not a value this project measured against a corpus. It was not
    /// well-founded before this change either, and this change does not make
    /// it worse-founded. The first real corpus should re-derive it.
    ///
    /// Every number above comes from `examples/mmr_calibration.rs`
    /// (`cargo run --release --example mmr_calibration -p memorysafe-policy`),
    /// retained in the checkout so they can be re-derived rather than trusted.
    /// They are SYNTHETIC — a generated vocabulary with a function-word head,
    /// because this project has no corpus yet — so they bound the shape of
    /// each effect and its sensitivity to function-word density, and are not
    /// calibration data.
    pub mmr_lambda: f32,
    /// `compose::working_set`'s MMR fill: an item more than this fraction of
    /// whose own content is already covered by the union of what is already
    /// selected is tagged `ReasonCode::DiversityCut` rather than
    /// `ReasonCode::HighValue` in its evidence. Not a fragility gate and not
    /// part of the ladder above — it never changes which item is chosen, only
    /// how the choice is explained in the audit trail, so a tenant override
    /// here is cosmetic rather than behavioural.
    ///
    /// **Also deliberately unchanged under union coverage**, and for a
    /// different reason than `mmr_lambda`'s. This is a SEMANTIC anchor, not a
    /// distribution calibration: "more than half of this item's own content is
    /// already present" is the same checkable English sentence before and
    /// after, and the union form makes it TRUE on exactly the cases where the
    /// pairwise `max` made it false. More rows will carry the tag; that is the
    /// correction landing, not drift to be absorbed. Re-tuning the number to
    /// hold the tag's firing rate flat would be fitting a durable audit label
    /// to a desired appearance rather than to what the mechanism does — the
    /// defect the `ExactDuplicate` -> `NearDuplicate` rename settled at
    /// `3a504a3`, which this same change is already applying to the evidence
    /// key beside it.
    ///
    /// And no value of this constant restores what changed. Union coverage is
    /// monotonically non-decreasing as the working set fills, so the tag is
    /// now partly a function of an item's POSITION in that set; raising the
    /// threshold moves where the position cutoff falls without making the tag
    /// position-independent again.
    ///
    /// The measured cost, flagged rather than tuned away: in a
    /// narrow-vocabulary scope the tag fires on essentially every MMR pick
    /// past about five selected items — `0.3% -> 94.1%` at five selected and
    /// `0.7% -> 100.0%` at ten — at which point it separates nothing. In a
    /// terse, distinctive scope it stays at `0.0% -> 0.0%` throughout, so this
    /// is not a uniform inflation but the same function-word artifact
    /// `mmr_lambda`'s note names, and it has the same fix: a stopword-aware
    /// tokenizer, not a threshold move. Numbers from
    /// `examples/mmr_calibration.rs`, with the caveats recorded there.
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
