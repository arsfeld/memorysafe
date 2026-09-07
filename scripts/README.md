# scripts

Checks this project's conventions demand and that a note in someone's head does not
reliably supply.

Both existed for a day in an unversioned sibling directory — not a git repository at all,
one reboot from gone, and requiring a person to remember to invoke them. That is the state
another lane's nine audit tools were found in this morning, and it is worse than a written
rule rather than better, because it *feels* like a mechanism: it runs, it produces output,
it finds things. Durable output is not durable capability.

## `stub-sweep [path-to-lib.rs]`

Lists every `Backend` method and its first body line, flagging bodies that are a bare
`Ok(..)`. A stub is identified by its **body**, not by its signature shape — the definition
was written down long before the ad-hoc greps that contradicted it, and
`async fn \w+\(\s*$|async fn \w+\(&self, _` reports 10 against a tree with 8.

Prints the count and the denominator: `stubs (body is a bare Ok(..)): N of M methods`.

## `mutate <repo-relative-file> <anchor> <replacement> [label]`

Applies one textual mutation, runs the workspace suite, reports a verdict.

**The working tree is never touched.** The mutation happens in a scratch copy extracted from
a git ref (`MUTATE_REF`, default `HEAD`), so this is safe to run while someone else is
editing, and the verdict is pinned to a commit — reproducible by anyone with that SHA rather
than a photograph of one desk at one moment. The first version mutated in place and restored
via `trap`, which is correct only while nobody else is working; that is not a condition a
script can check, so it stopped being a condition.

**Refuses to report a verdict** unless the anchor matched exactly once and the tree
compiled.

Three ways a mutation run lies, all producing the same clean output as a genuinely
surviving mutant, all hit for real here:

1. the anchor matched zero places, or several — nothing was mutated;
2. the mutant did not compile — nothing was run;
3. you grepped `^error` to catch (2), but `cargo test` prints `error: test failed` on every
   *successful* kill, so the check fires exactly when the mutation worked.

Silence means "no test caught it" only after 1 and 2 are excluded. And a survivor is a
finding only if some input distinguishes the mutant from the original.

**A fourth way, that wears no error message at all: a mutant left applied in the working
tree.** This is what "the working tree is never touched" above is *for* — but only when
`mutate` is what did the touching. Hand-applied mutation testing (edit a file, run the
suite, revert, repeat — necessary whenever the mutation isn't a clean textual anchor `mutate`
can target) has no such guarantee, and there is nothing to warn you: the tree still
compiles and the suite still passes, which is exactly what a genuine survivor looks like.
No red test, no build error, no clippy warning — it ships as if it were the fix. The only
reliable tell is reading `git diff` before every commit, not just after a run that reported
a survivor. Worth naming precisely: documenting a mutant in prose (a comment recording that a
`WHERE` clause turned into a tautology survived) and actually leaving that tautology in the
compiled `WHERE` clause look identical to a `grep` for the mutated string — the search finds
both, and cannot tell you which one it found.

## Not yet a CI gate

`stub-sweep` becomes one when the count reaches zero — a step asserting it stays there is
what makes "no method silently reverts to a stub" unwriteable rather than remembered.

`mutate` is not a gate **yet**, and the reason is fixable rather than structural. It mutates
the working tree in place, which is a property of this implementation and not of mutation
testing: `cargo mutants` copies the tree and mutates the *copy*, which is why it runs in CI
on other projects. It is already installed here — `cargo-mutants 27.1.0` — with no
`mutants.toml` and no CI step.

That distinction is worth stating precisely, because the first version of this file said
`mutate` *"cannot"* be a gate and used it to justify keeping the script in a weaker
category. **A "cannot" that is really a "have not" is the load-bearing kind of error**, and
it was written by someone who had spent the day removing exactly that shape from tests.

## The mutation baseline — measured, so a later run has something to differ from

|  | `e7525a6` (pre-freeze) | `2d5701c` (freeze) |
|---|---|---|
| mutants | 187 | 211 |
| caught | 123 | 147 |
| missed | 21 | 16 |
| timeouts | 0 | **2** |
| unviable | 43 | 46 |
| viable | 144 | 165 |
| score on viable | 85.4% | 89.1% |
| wall clock | 6 min | 9 min |

**One verdict moved for a reason that has nothing to do with the code**, and it is the most
useful thing in this table. `keyword.rs`'s squash mutant went MISSED → TIMEOUT. A timeout is
**not** a kill — cargo-mutants reports it separately because it is unresolved.

`cargo mutants` derives its per-mutant timeout from the baseline test run:

    e7525a6   baseline test 1s   auto timeout 20s   headroom 20x
    2d5701c   baseline test 4s   auto timeout 22s   headroom 5.5x

The portability task collapsed 28 individually-bound conformance tests into one call to
`run_conformance_suite`, whose `run!` **awaits each test inline** — so fifty tests that had
been running in parallel within the binary now run serially. The conformance binary went
2.5s to 4.1s *while gaining 22 tests*.

**A measurement whose instrument is calibrated from the artifact under test drifts when the
artifact's shape changes, not only its content.** Hence the pinned `timeout_multiplier` in
`.cargo/mutants.toml`: on auto, verdicts stop being comparable across any commit that
changes test structure.

## What a mutation score does NOT measure — the boundary, marked by a Critical

**89.1% was the score on the commit that carried silent data loss in the compliance path.**

The freeze review found that `export(include_audit: true)` dropped every audit row for a
scope holding no items — exactly what `purge_subject(Preserve)` creates, so a migrated
tenant lost the only remaining evidence that an erasure happened.

**No mutation run could have found it, and the run at that commit had `portability.rs` fully
in scope.** Mutation testing perturbs an existing expression and asks whether the tests
notice. C1 was not a perturbable expression — it was the **wrong choice of data source**,
deriving the audit scopes from the items just exported. No single-token operator generates
"read this from a different table". The defect was semantically wrong and syntactically
invisible.

So: **a mutation score measures test sensitivity to local perturbation, not
defect-freedom.** Three instruments, three distinct blind spots, and the third is reached by
neither:

| instrument | finds | blind to |
|---|---|---|
| `cargo mutants` | local perturbations the tests miss | wrong source, wrong shape — anything not a token edit |
| a hypothesis-driven review pass | discriminators, with measured inputs | whatever nobody thought to hypothesise |
| neither | — | assertions *structurally* unable to observe a field |

The third has a worked example in `docs/known-gaps.md`: the exported vector's `scale`.
`export_import_round_trips_exactly` compares ranked ids from `neighbours`, and **cosine
ranking is invariant under per-vector positive scaling** — so that assertion cannot pin
`scale` however hard it is strengthened. Found by reasoning about what an assertion can
*see*, which is not a thing either tool does.

## The denominator nobody had printed

`cargo mutants` finds **187 mutants** in `memorysafe-backend-sqlite` — `lib.rs` 46,
`retrieve.rs` 40, `items.rs` 37, `tenant.rs` 14, `vectors.rs` 13, `keyword.rs` 13,
`aggregates.rs` 12, `capacity.rs` 6, `audit.rs` 4, `schema.rs` 2 — at roughly six seconds
each, so about twenty minutes for the set.

**Every mutation result in this project's history was hand-selected by whoever chose what to
break.** They found real defects, and they are a sample from a population whose size nobody
had measured. `mutate` tests a hypothesis; `cargo mutants` supplies the denominator. Use the
second before believing a clean sweep from the first.
