//! The conformance suite, run against the SQLite backend.
//!
//! Task 24 closes the last four stubbed `Backend` methods — `purge_subject`,
//! `export`, `import` and the `audit_aggregates` read — and with them the
//! last 22 conformance tests, all `lifecycle`. **The suite is frozen as of
//! this task**: `run_conformance_suite` is the single entry point every
//! future backend (Plan 2's Postgres backend included) must pass unmodified.

use memorysafe_backend::conformance::{BackendFactory, run_conformance_suite};
use memorysafe_backend_sqlite::SqliteBackend;

/// Each test gets a backend rooted in its own `TempDir`. The directory is
/// leaked deliberately: it must outlive the backend, and the OS reclaims it.
struct SqliteFactory;

impl BackendFactory for SqliteFactory {
    type B = SqliteBackend;
    // `async fn`, not `fn create(&self) -> impl Future<..> + Send`. The trait
    // declares the latter and both are the same signature after desugaring,
    // but `clippy::manual_async_fn` is denied workspace-wide and fires on the
    // hand-written form — the same deviation the earlier per-test
    // `conformance.rs` this file replaces already made, for the same reason.
    // The `Send` bound the trait states is still checked here: a factory
    // whose future were not `Send` fails to compile.
    async fn create(&self) -> Self::B {
        let dir = tempfile::tempdir().expect("tempdir");
        SqliteBackend::open(dir.keep())
    }
}

/// The whole suite. Plan 2's Postgres backend runs this same function.
#[tokio::test(flavor = "multi_thread")]
async fn sqlite_passes_the_backend_conformance_suite() {
    run_conformance_suite(&SqliteFactory).await;
}
