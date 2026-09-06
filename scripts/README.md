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

## `mutate <file> <anchor> <replacement> [label]`

Applies one textual mutation, runs the workspace suite, restores via `trap` on every exit
path. **Refuses to report a verdict** unless the anchor matched exactly once and the tree
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
`mutate` cannot be a CI gate: it deliberately breaks the tree. It is a developer tool, and
being in the repository makes it durable, not automatic.
