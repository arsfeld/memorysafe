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

## The denominator nobody had printed

`cargo mutants` finds **187 mutants** in `memorysafe-backend-sqlite` — `lib.rs` 46,
`retrieve.rs` 40, `items.rs` 37, `tenant.rs` 14, `vectors.rs` 13, `keyword.rs` 13,
`aggregates.rs` 12, `capacity.rs` 6, `audit.rs` 4, `schema.rs` 2 — at roughly six seconds
each, so about twenty minutes for the set.

**Every mutation result in this project's history was hand-selected by whoever chose what to
break.** They found real defects, and they are a sample from a population whose size nobody
had measured. `mutate` tests a hypothesis; `cargo mutants` supplies the denominator. Use the
second before believing a clean sweep from the first.
