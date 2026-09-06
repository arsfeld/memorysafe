//! Textual similarity proxy, shared by every policy path that has to ask "how
//! much of this item does that other item already say?"
//!
//! Kept as its own module for the reason `eviction` is: `compose`'s MMR
//! diversity penalty and `maintain`'s consolidation and fragility decay are
//! all asking the same question of the same data, and a governance product
//! that answered it differently depending on which entry point asked would be
//! a real defect. Anything comparing two bodies textually should call
//! `overlap` rather than reimplement it.
//!
//! The backend's embeddings are not carried through to the policy seam at all
//! — neither `ComposeContext` nor `MaintainContext` has anywhere to put one —
//! so this token-set proxy is what a pure policy has to work with. It is
//! crude, and every caller's own doc says what it is trusted for.

/// Cheap textual proxy for "how much of `a`'s own content is already covered
/// by `b`". The backend's vectors are not carried through to the policy, so
/// this proxy works over token sets instead of embeddings — crude, but it
/// reliably catches the case its callers exist for: near-paraphrases.
///
/// **Directional, not symmetric.** `overlap(a, b)` is `|A ∩ B| / |A|`, where
/// `A` is the SET of `a`'s distinct lower-cased, whitespace-split tokens and
/// `B` is `b`'s — the fraction of `a`'s OWN content also present in `b`, not
/// a symmetric "how alike are these two texts". Every call shape asks the
/// question in that direction, with `a` the item whose redundancy is in
/// question:
///
/// - `compose`'s MMR fill uses it as a dissimilarity PENALTY,
///   `overlap(&candidate.item.body, &selected.item.body)` — "what fraction of
///   THIS CANDIDATE is already covered by what has been selected".
/// - `maintain`'s consolidation uses it to pick which of two existing items is
///   absorbed: `overlap(absorbed, target)` — "what fraction of THIS ITEM
///   would survive inside that one".
/// - `maintain`'s fragility decay uses it as the neighbour `relevance`
///   `fragility::score` reads, `overlap(&subject.body, &neighbour.body)` —
///   "how much of THIS ITEM could be relearned from that neighbour", which is
///   what fragility asks of a neighbourhood.
///
/// A symmetric measure gets this backwards. Consider a candidate that
/// EXTENDS an already-selected item (adds new information not already
/// present) versus one that is entirely CONTAINED IN it (adds nothing new):
/// Jaccard (`|A∩B|/|A∪B|`) scores both at `0.50` in the case where one set is
/// exactly double the other — treating "adds nothing" as no more similar
/// than "adds something", which is precisely the distinction this penalty
/// exists to make. `|A∩B|/|A|` gets both directions right: the extending
/// candidate scores `0.50` (its own new tokens count against its own
/// denominator, pulling the ratio down), the contained candidate scores
/// `1.00` (all of it is already covered, so it should be penalised fully).
///
/// **A gap this leaves deliberately**, for any caller that compares against a
/// SET rather than a single item: a candidate covered by that set taken
/// together, but by no single member of it, is not detected. The caller
/// (`working_set`'s MMR fill) takes a `max` over
/// pairwise `overlap` calls against each selected item individually, and
/// `max` over pairwise comparisons cannot see a union that only emerges
/// across several of them. This is canonical MMR's own property — it is
/// defined pairwise against the selected set — not a defect introduced here.
pub fn overlap(a: &str, b: &str) -> f32 {
    let toks = |s: &str| -> std::collections::HashSet<String> {
        s.split_whitespace().map(|t| t.to_lowercase()).collect()
    };
    let (ta, tb) = (toks(a), toks(b));
    // Only `ta` (the candidate, `A`) needs an emptiness guard: `ta.is_empty()`
    // risks dividing `0 / 0` (NaN). `tb` empty needs no special case —
    // `ta.intersection(&tb)` is empty whenever `tb` is, regardless of `ta`'s
    // contents, so `0 / ta.len()` already reads as the correct `0.0` on its
    // own.
    if ta.is_empty() {
        return 0.0;
    }
    let shared = ta.intersection(&tb).count();
    shared as f32 / ta.len() as f32
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_treats_an_empty_candidate_as_zero_and_an_empty_selected_item_needs_no_guard() {
        // Rejects: removing or inverting the `ta.is_empty()` guard. Without
        // it, an empty candidate (`a`) divides `0 / 0` (NaN), and
        // `assert_eq!(_, 0.0)` fails against NaN (NaN != anything, including
        // itself), so the first assertion is not a case that could pass by
        // luck.
        // The second assertion pins a structural fact about `|A ∩ B| / |A|`
        // rather than testing a guard-removal mutation: an empty SELECTED
        // item (`b`) needs no special case at all — `ta.intersection(&tb)`
        // is empty whenever `tb` is, so `0 / ta.len()` already reads as the
        // correct `0.0` without a guard. (This is why the function's guard
        // checks only `ta.is_empty()`, not `ta.is_empty() || tb.is_empty()`
        // as an earlier, symmetric version of this formula needed.)
        assert_eq!(overlap("", "cats and dogs"), 0.0);
        assert_eq!(overlap("cats and dogs", ""), 0.0);
    }

    #[test]
    fn overlap_divides_the_shared_count_rather_than_taking_a_remainder() {
        // Rejects: the ratio's `/` weakened to `%`. `shared / ta.len()` and
        // `shared % ta.len()` agree whenever `shared` is 0 or a multiple of
        // `ta.len()`, so this fixture is chosen so they diverge: the
        // candidate `"a b c d"` has 4 distinct tokens, exactly 1 of which
        // ("a") is also in `"a x"`, giving `1 / 4 = 0.25` under division and
        // `1 % 4 = 1.0` under remainder.
        // Vacuous if `shared` were 0 or a multiple of `ta.len()` — pinned by
        // the arithmetic in the comment, not by inspection alone.
        assert_eq!(overlap("a b c d", "a x"), 0.25);
    }

    #[test]
    fn overlap_of_a_repeated_token_is_not_inflated_by_the_repeats() {
        // Rejects: tokenizing into a `Vec` (multiset) rather than a
        // `HashSet` for either side — with `a`'s multiset `["the", "the",
        // "cat"]` (occurrence-count numerator) over `ta.len() == 3`
        // (multiset denominator), "the" is counted twice against `b`'s
        // single "the", giving `2 / 3 ≈ 0.6666667`, not the `0.5` a
        // distinct-token count gives (`shared = |{the, cat} ∩ {the, dog}| =
        // 1`, `|A| = 2`).
        //
        // An earlier version of this fixture (`overlap("the the the", "the
        // cat")`, expecting `1.0`) was NOT testing deduplication at all,
        // in any version of the code, and this is what actually went wrong
        // here — not that a once-discriminating fixture quietly stopped
        // discriminating. That fixture was chosen for a DIFFERENT, real
        // claim: it forced `shared = 3` against `min(3, 2) = 2`, i.e. `1.5`
        // before the (then-current) `.min(1.0)` clamp, and it discriminated
        // correctly for "the clamp exists". When the clamp was replaced by
        // this directional `|A|` denominator — which makes a clamp
        // unneeded, since `|A ∩ B| ≤ |A|` always — that fixture's original
        // purpose became obsolete. Instead of deleting the test, it was
        // RENAMED and its rejection claim swapped to "tokenizes into a
        // `HashSet`, not a `Vec`" — a claim the fixture had never supported:
        // `3 / 3` and `1 / 1` are both `1.0`, so it cannot tell a multiset
        // from a set either before or after the rewrite. The fixture's
        // credibility (it looked considered — a specific name over a
        // specific number) carried across to a claim it was never built to
        // support, and reading the result over did not catch it.
        //
        // The rule this is worth stating plainly: when a code change makes
        // a test's purpose obsolete, DELETE the test — do not repurpose the
        // fixture for a new claim. A fixture chosen to exercise property X
        // is evidence about X only.
        //
        // Vacuous if `a` had at most one occurrence of any token that also
        // appears in `b` — pinned by construction: "the" appears twice in
        // `a` and matches `b`'s one occurrence, which is exactly the
        // multiset/set divergence this fixture exists to force.
        assert_eq!(overlap("the the cat", "the dog"), 0.5);
    }

    #[test]
    fn overlap_penalises_a_contained_candidate_fully_and_an_extending_one_only_partially() {
        // The motivating case for `|A ∩ B| / |A|` over a symmetric measure
        // like Jaccard (`|A ∩ B| / |A ∪ B|`): a candidate that ADDS
        // information to what is already selected must be penalised less
        // than one that adds NOTHING, and a symmetric measure cannot tell
        // the two apart. `long` (6 distinct tokens) is exactly `short`'s 3
        // tokens plus 3 new ones, so `short` ⊆ `long`.
        //
        // Rejects: a symmetric similarity measure standing in for this
        // directional one. Jaccard scores BOTH directions at
        // `3 / 6 = 0.50` here (the union is `long` either way, since one set
        // contains the other) — it cannot distinguish "this candidate is a
        // strict extension, so partially penalise it" from "this candidate
        // is a strict subset, so fully penalise it", which is exactly the
        // distinction this function exists to make.
        // Vacuous if the two directions produced the same number regardless
        // of formula — pinned by asserting both directions to DIFFERENT
        // values (`0.5` and `1.0`), not merely that each is in range.
        let short = "the cat sat";
        let long = "the cat sat plus more words";

        // `long` as the candidate, `short` as what's already selected:
        // `long` EXTENDS `short` with new information ("plus", "more",
        // "words") — only half its own content is already covered.
        assert_eq!(overlap(long, short), 0.5);

        // `short` as the candidate, `long` as what's already selected:
        // `short` is entirely CONTAINED IN `long` — nothing new, fully
        // covered, maximally penalised.
        assert_eq!(overlap(short, long), 1.0);
    }
}
