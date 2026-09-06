# AGENTS.md

## Project overview

MemorySafe is a Rust 2024 Cargo workspace for deterministic, governed memory infrastructure. The current workspace contains core domain types, embedding utilities, a backend contract/conformance suite, and a SQLite backend.

## Toolchain

- Use the repository-pinned Rust toolchain from `rust-toolchain.toml` (Rust 1.97.1 with `rustfmt` and `clippy`).
- Run commands from the repository root.
- Keep `Cargo.lock` committed and update it intentionally.
- The workspace forbids `unsafe` code and denies the configured Rust and Clippy lints.

## Architecture and conventions

- `memorysafe-core`: pure domain types and policy interfaces. Do not add I/O dependencies.
- `memorysafe-embed`: deterministic/optional local-only embeddings. Do not introduce network-capable dependencies; model loading must remain local-only.
- `memorysafe-backend`: backend trait, write/query types, and reusable conformance tests.
- `memorysafe-backend-sqlite`: SQLite implementation. Preserve one-database-file-per-tenant isolation, atomic item/audit writes, and plaintext-free audit records.
- Scope is always tenant → subject → namespace. Validate identifiers through the existing typed constructors rather than bypassing them.
- Prefer deterministic tests with temporary directories and the built-in deterministic embedder. Tests must not require network access, external services, or model downloads.
- Put shared backend behavior in the conformance suite so every backend implementation inherits the contract.
- Treat privacy, isolation, atomicity, capacity accounting, and export/import behavior as invariants; add regression tests with behavior changes.
- Follow existing module organization and error types. Avoid broad refactors mixed with functional changes.

## Build and run

This repository currently builds libraries rather than a standalone application.

```bash
cargo build --workspace
cargo build --workspace --all-features
```

For API usage, see the example in `README.md`.

## Test

```bash
# Full workspace suite (matches CI coverage)
cargo test --workspace --all-features

# SQLite backend conformance only
cargo test -p memorysafe-backend-sqlite --test conformance

# One package or test filter
cargo test -p <package>
cargo test <test_name>
```

## Required checks before committing

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

Also verify dependency purity when changing manifests:

```bash
cargo tree -p memorysafe-core --edges normal
cargo tree -p memorysafe-embed --all-features --edges normal
```

`memorysafe-core` must not acquire I/O dependencies. `memorysafe-embed` must not reach a network stack under any feature combination.

## Repository scripts

- `scripts/stub-sweep [path-to-lib.rs]`: reports bare `Ok(..)` backend method stubs.
- `scripts/mutate <file> <anchor> <replacement> [label]`: runs a mutation in an isolated scratch copy; it does not modify the working tree.

Read `scripts/README.md` before relying on mutation results.

## Git and GitHub

- `origin` uses the VM's GitHub integration at `github.int.exe.xyz`; keep it that way so fetch and push use short-lived integration credentials.
- The default branch is `master`.
- Keep commits focused and use the repository's existing Conventional Commit-style subjects (`feat:`, `fix:`, `docs:`, etc.).
- Do not commit generated `target/` contents, secrets, `.env` files, tenant databases, or model artifacts.
