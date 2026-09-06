# Open items — `memorysafe-policy`

Findings raised during Tasks 25-30 that were reviewed, judged **Minor**, and
deliberately not fixed. Written into the repo because the workspace they were
recorded in (`.superpowers/sdd/`) is git-ignored, and **a roll-up nobody can
read is a silent discard** — which is the failure this list exists to prevent,
so it must not be the failure that ends it.

**Citations are by file and symbol, never line number.** These entries outlived
a 156-line file move and a module extraction; a line citation would have needed
re-resolving after each and a symbol did not.

**Entries marked DO NOT FIX are not oversights.** Their correct disposition is to
remain open, and closing them would add a test that asserts what the control flow
already guarantees. Each states the condition under which that stops being true.

---

## Do not fix while the stated condition holds

**`admit`'s `SensitivityConflict` / `ProtectedFragile` exclusivity has no test.**
The `if`/`else` makes emitting both structurally impossible, so an assertion
would test the control flow rather than behaviour. **Fires if that becomes two
independent `if` statements or two unconditional pushes** — then the assertion
becomes meaningful and must be written in the same commit. A guard comment at the
site says so; this entry is its index.

**`value::score`'s neutrality-of-absence has no test.** The absent branch returns
`base` literally, so neutrality is structural. **Fires if the two branches are
folded into one expression, or the absent branch stops returning `base`
unmodified.** A guard comment on the `None => base` arm forbids the fold.

## Needs a decision, not a tidy-up

**`admit`'s protection gate is a hard cutoff on fragility alone, ignoring value,
while `eviction::cost` is value-weighted.** So a protection-expired,
high-fragility-but-now-low-value item can rank cheap to evict. Both behaviours
are defensible in isolation; whether they should agree is a design question that
predates these tasks and was never decided.

**`serde_json` is declared under `[dependencies]` in `memorysafe-policy`, and
every one of its 11 uses is in `value.rs` after that file's `#[cfg(test)]`.** The
crate has no `[dev-dependencies]` section. A test-only crate under
`[dependencies]` ships in the dependency graph of every downstream consumer.
`memorysafe-core` (4 non-test uses) and `memorysafe-backend` (3) are genuine, so
the remedy is local: move it. **It contradicts the plan's verbatim `Cargo.toml`
block, so it needs a plan amendment alongside the edit** — not a silent fix.

## Documentation gaps where the silence is the defect

**`sensitivity::looks_like_phone` misses space-separated numbers** (`555 123
4567`) and **JWT-shaped secrets are excluded by construction** (dot-delimited, so
the tokenizer never presents them). Neither is named in a doc comment, so a
reader cannot tell a limitation from a bug. **The fix is a doc comment at the
function, not a behaviour change.**

## Coverage and hygiene

**`similarity::coverage` and `token_set` have no direct unit test.** The module's
tests are all `overlap_*`; both new functions are exercised transitively and
end-to-end, so the multi-body union property *is* tested — just never at the
function's own home.

**A test comment in `compose` over-claims because it is coupled to a default.**
It asserts both its assertions fail under the pairwise-max form, but the
reason-code half holds only while `diversity_cut_similarity == 0.50`. The value
assertion still discriminates, so the test is sound and the comment is what
over-claims. **The sibling test two functions away does this correctly**, stating
its inequality in terms of `cfg.mmr_lambda` so it fails loudly instead — copy it.

**`similarity::coverage` returns bare `f32` for a value provably in `[0,1]`**
where the project's convention names `Score`. Matching `overlap`'s pre-existing
`f32` is the defensible reading and splitting the two return types would be
worse; recorded because the new function is where the convention could have been
applied.

**`eviction`'s reclaim sort recomputes `cost()` per comparison** rather than
precomputing per candidate. Harmless at expected volumes; a Schwartzian transform
if it ever matters.

**`redundancy` re-exports `Verdict`, which `lib.rs` already re-exports at the
crate root** — two import paths for one type, and `redundancy`'s test module ends
up testing `BaselineConfig::classify`, a `config` concern.

**`sensitivity::lexical_density` clones the token vector where a `HashSet` would
do.**

**`compose.rs` is ~1500 lines**, roughly two-thirds tests, with several 25-35
line test comments restating material now also in `config.rs`. Defensible as
decision record; the near-verbatim duplication will drift, and when it does the
code is the survivor.

## Named next change

**`similarity::overlap` does no stopword removal.** Task 30's calibration found
this is the root of both large measured effects — coverage rates and the
`DiversityCut` firing rate are both dominated by function words rather than
topical overlap. It is the highest-value change remaining in `similarity`, and
`examples/mmr_calibration.rs` is the harness to measure it with.
