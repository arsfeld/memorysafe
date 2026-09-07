use crate::schema;
use memorysafe_backend::BackendError;
use memorysafe_core::TenantId;
use rusqlite::Connection;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as AsyncMutex;

pub fn storage_error(e: impl std::fmt::Display, retryable: bool) -> BackendError {
    BackendError::Storage {
        message: e.to_string(),
        retryable,
    }
}

fn to_backend(e: rusqlite::Error) -> BackendError {
    let retryable = matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    );
    storage_error(e, retryable)
}

/// `memorysafe-backend` must not depend on `rusqlite`, so the conversion from
/// `rusqlite::Error` to `BackendError` is defined locally in this crate as an
/// extension trait. Every SQL call in Tasks 20–24 ends in `.sql()?` rather
/// than a bare `?`, and so do the closures in this task's tests.
pub trait SqlResultExt<T> {
    fn sql(self) -> Result<T, BackendError>;
}

impl<T> SqlResultExt<T> for rusqlite::Result<T> {
    fn sql(self) -> Result<T, BackendError> {
        self.map_err(to_backend)
    }
}

type Handle = Arc<StdMutex<Connection>>;

/// The pooled connections and the root they live under. Separated from
/// [`TenantManager`] only so it can be `Arc`-cloned into a blocking task:
/// opening a database file and installing the schema are blocking work and
/// must not run on an async runtime thread.
struct Pool {
    root: PathBuf,
    cache: StdMutex<lru::LruCache<String, Handle>>,
    /// Connections opened over this pool's lifetime, including reopens after
    /// an eviction. Exposed as [`TenantManager::open_count`] so a test can
    /// assert that an eviction actually happened rather than asserting it in
    /// a comment.
    opened: AtomicU64,
}

impl Pool {
    fn handle(&self, key: &str) -> Result<Handle, BackendError> {
        {
            let mut cache = self.cache.lock().expect("pool mutex");
            if let Some(h) = cache.get(key) {
                return Ok(Arc::clone(h));
            }
        }

        // Miss. The open and the schema install are blocking work — a file
        // create plus a DDL batch plus an fsync — so they happen **outside**
        // the pool lock; holding it across them would queue every other
        // tenant's lookup behind one tenant's first open.
        //
        // Releasing the lock is what preflight F1 names as a race: two callers
        // can both miss for one tenant and both open the file, and in the
        // sketch the second `put` evicted the first while its holder kept
        // using it. The insert below **narrows** that: it re-checks the cache
        // under the same lock that publishes the winner, so a loser whose
        // winner is still cached drops its connection before ever using it —
        // one wasted open instead of a second live handle.
        //
        // It does not close it, and the difference matters. If the winner has
        // already been evicted by capacity pressure or a concurrent `forget`,
        // the loser's re-check misses and it publishes its own handle: two
        // live connections to one file. That is benign here only because it is
        // the *same* state a plain LRU eviction produces — hold a handle, let
        // capacity push it out, ask again — which needs no race at all and
        // cannot be designed away while the pool is bounded. `with_write`'s
        // per-tenant async mutex is what covers it, permanently.
        //
        // `create_dir_all` is here rather than in `TenantManager::new` so an
        // uncreatable root is a `BackendError` and not a panic in a library
        // constructor (preflight F6).
        std::fs::create_dir_all(&self.root).map_err(|e| storage_error(e, false))?;
        // `TenantId` validation forbids `/`, `..`, uppercase and control
        // characters, so this is a safe path component by construction.
        let path = self.root.join(format!("{key}.db"));
        let conn = Connection::open(&path).map_err(to_backend)?;
        schema::initialise(&conn).map_err(to_backend)?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        let handle: Handle = Arc::new(StdMutex::new(conn));

        // `push` rather than `put` so the entry the LRU drops comes back out
        // and is released *after* the guard: dropping the last handle to a WAL
        // database checkpoints it and unlinks the `-wal`/`-shm` sidecars, and
        // that file I/O must not happen under the cross-tenant pool mutex —
        // the same reason the open above is outside it.
        let displaced = {
            let mut cache = self.cache.lock().expect("pool mutex");
            if let Some(winner) = cache.get(key) {
                return Ok(Arc::clone(winner));
            }
            cache.push(key.to_owned(), Arc::clone(&handle))
        };
        drop(displaced);
        Ok(handle)
    }

    /// Drops the cached handle for `key`, if any. The connection itself closes
    /// only when the last `Arc` to it goes, and that close is deliberately
    /// performed outside the pool lock — see `handle`.
    fn evict(&self, key: &str) {
        let evicted = {
            let mut cache = self.cache.lock().expect("pool mutex");
            cache.pop(key)
        };
        drop(evicted);
    }
}

/// Owns one SQLite file per tenant. Connections are pooled with an LRU so a
/// large tenant count does not mean an equally large open-file count; writes
/// for a given tenant are serialised through a per-tenant
/// [`tokio::sync::Mutex`], which is what makes the capacity accounting and the
/// aggregate increments correct.
///
/// # The write lock is the serialiser, and the connection mutex is not
///
/// A pooled handle can be evicted while a caller still holds it, so **two live
/// connections to one tenant's file are a normal state**, not a defect: the
/// evicted handle stays alive in its holder's hands and the next caller opens
/// a fresh one. When that happens the `Mutex<Connection>` each caller holds
/// serialises nothing against the other, and only [`Self::with_write`]'s
/// per-tenant async mutex does.
///
/// The consequence for Tasks 20–24: **a read-modify-write must go through
/// [`Self::with_write`]**, never [`Self::with_conn`].
///
/// `memorysafe_backend::aggregates` requires that concurrent increments of one
/// aggregate row cannot interleave and asks each backend to say which of an
/// atomic upsert, a row lock or serialised writers it chose. This backend
/// serialises the writers, here.
///
/// # This is a handle cache, not a connection pool
///
/// There is at most **one** pooled `Connection` per tenant, behind a
/// `Mutex<Connection>`, so within a tenant reads serialise with each other and
/// with writes. WAL buys cross-tenant concurrency and correctness in the
/// transient two-connection case above — it does not buy concurrent readers
/// within one tenant, and Task 21's recall-latency reasoning must not assume
/// it does. Raising `max_open` raises the number of *tenants* held open, never
/// the parallelism available to any one of them.
///
/// # A panicking closure does not brick the tenant
///
/// A closure that panics while holding the connection poisons its mutex.
/// Left alone, that poisoned handle stays in the cache and fails every later
/// call for the tenant with `retryable: false`, which nothing above will
/// retry — one `unwrap` in one closure, and the tenant is gone for the life of
/// the process. Instead the poisoned handle is dropped from the cache and the
/// tenant reopened on the next call; the poisoned connection is never reused,
/// since it may be inside a half-finished transaction. [`Self::forget`] is the
/// manual equivalent and is no longer needed for this.
///
/// # Deleting a tenant out from under a live manager
///
/// `lib.rs` advertises that tenant deletion is `rm`. A pooled handle keeps
/// writing to an unlinked inode, so the file must not be removed while this
/// manager holds a connection to it: call [`Self::forget`] first, and only
/// then remove `<tenant>.db` together with its `-wal` and `-shm` sidecars.
///
/// # Bounds
///
/// `write_locks` holds one entry per tenant this manager has ever written to
/// and is never pruned. [`Self::forget`] deliberately does not drop it:
/// removing a mutex another task is holding would let the next caller create a
/// fresh one and defeat the serialisation that entry exists for. The map is
/// bounded by the deployment's tenant count, which the pool is not.
pub struct TenantManager {
    pool: Arc<Pool>,
    write_locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl TenantManager {
    /// `max_open` is the pool capacity; `0` is treated as `1`. Nothing touches
    /// the filesystem here — the root is created on the first open, so a root
    /// that cannot be created surfaces as a `BackendError` from the call that
    /// needed it rather than as a panic in this constructor.
    pub fn new(root: PathBuf, max_open: usize) -> Self {
        let cap = NonZeroUsize::new(max_open).unwrap_or(NonZeroUsize::MIN);
        Self {
            pool: Arc::new(Pool {
                root,
                cache: StdMutex::new(lru::LruCache::new(cap)),
                opened: AtomicU64::new(0),
            }),
            write_locks: StdMutex::new(HashMap::new()),
        }
    }

    /// How many connections this manager has opened, reopens after an eviction
    /// included. An observable so a test can assert that the pool evicted
    /// rather than assert it in a comment.
    pub fn open_count(&self) -> u64 {
        self.pool.opened.load(Ordering::Relaxed)
    }

    /// Drops the pooled connection for `tenant`, if any. Call this before
    /// removing a tenant's database file; see the type doc. Callers still
    /// holding a handle keep it — this evicts the cache entry, it does not
    /// close anyone's connection — so the file is safe to remove only once
    /// every in-flight `with_conn`/`with_write` for that tenant has returned.
    ///
    /// The tenant's write lock is deliberately kept: see the type doc.
    ///
    /// **Blocking.** If this drops the last handle, closing the connection
    /// checkpoints the WAL and unlinks the `-wal`/`-shm` sidecars — file I/O,
    /// synchronously, on the calling thread. From an async runtime, call it
    /// inside `spawn_blocking`.
    pub fn forget(&self, tenant: &TenantId) {
        self.pool.evict(tenant.as_str());
    }

    fn write_lock(&self, tenant: &TenantId) -> Arc<AsyncMutex<()>> {
        let mut locks = self.write_locks.lock().expect("write-lock map mutex");
        Arc::clone(
            locks
                .entry(tenant.as_str().to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Runs `f` on the tenant's connection off the async runtime, on the
    /// blocking pool. Reads for *different* tenants run concurrently; reads
    /// for the same tenant do not — see "This is a handle cache, not a
    /// connection pool" on the type.
    ///
    /// **Reads only.** The `Mutex<Connection>` this holds does not make `f`
    /// atomic — see the type doc — so a read-modify-write belongs in
    /// [`Self::with_write`].
    pub async fn with_conn<T, F>(&self, tenant: &TenantId, f: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, BackendError> + Send + 'static,
    {
        let pool = Arc::clone(&self.pool);
        let key = tenant.as_str().to_string();
        tokio::task::spawn_blocking(move || {
            let handle = pool.handle(&key)?;
            // The lock attempt is scoped so the borrow of `handle` ends before
            // it is dropped below; the success path returns from inside it.
            let poisoned = match handle.lock() {
                Ok(mut conn) => return f(&mut conn),
                // Poisoned: a previous closure panicked while holding this
                // connection. Heal by dropping it and reopening — see "A
                // panicking closure does not brick the tenant" on the type.
                //
                // One healing path, taken at the lock rather than behind an
                // `is_poisoned()` pre-check. A pre-check needs a second,
                // narrower branch for the window between checking and locking,
                // and that branch is not reachable from any test — an untested
                // branch on the recovery path is the shape this crate keeps
                // finding. Locking first means the one path is the one the
                // poison test exercises.
                // The `PoisonError` carries a guard borrowing the mutex, so
                // the message is taken out here and the error left behind —
                // otherwise `handle` could not be dropped below.
                Err(e) => e.to_string(),
            };

            // Release the poisoned connection **before** reopening, both
            // references. A closure that panicked inside a write transaction
            // leaves that connection holding SQLite's write lock, so a
            // replacement opened while it is still alive blocks for the whole
            // `busy_timeout` and then fails: measured at 5.019s ending in
            // `database is locked`, against 6.4ms and success once it is
            // dropped.
            pool.evict(&key);
            drop(handle);

            // Two callers healing the same tenant at once can each evict the
            // other's fresh handle and end up on separate connections. That is
            // the same two-connection state ordinary capacity pressure
            // produces, which `with_write` already covers, so it is benign
            // rather than unhandled.
            let fresh = pool.handle(&key)?;
            let mut conn = fresh.lock().map_err(|_| {
                // Both attempts poisoned: another caller panicked in the
                // window. `retryable` is the right advice — the next call runs
                // this same healing path from the top.
                storage_error(
                    format!("tenant connection poisoned by an earlier panic: {poisoned}"),
                    true,
                )
            })?;
            f(&mut conn)
        })
        .await
        .map_err(|e| storage_error(e, false))?
    }

    /// Same, but holds the tenant's write lock for the whole closure. Every
    /// mutating path must go through this.
    pub async fn with_write<T, F>(&self, tenant: &TenantId, f: F) -> Result<T, BackendError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, BackendError> + Send + 'static,
    {
        let lock = self.write_lock(tenant);
        let _guard = lock.lock().await;
        self.with_conn(tenant, f).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memorysafe_core::TenantId;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[tokio::test]
    async fn each_tenant_gets_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);

        let a = TenantId::new("tenant-a").unwrap();
        let b = TenantId::new("tenant-b").unwrap();
        mgr.with_write(&a, |c| {
            c.execute_batch("CREATE TABLE probe(x)").sql()?;
            Ok(())
        })
        .await
        .unwrap();
        mgr.with_write(&b, |c| {
            c.execute_batch("CREATE TABLE probe(x)").sql()?;
            Ok(())
        })
        .await
        .unwrap();

        assert!(dir.path().join("tenant-a.db").exists());
        assert!(dir.path().join("tenant-b.db").exists());
    }

    #[tokio::test]
    async fn the_schema_is_installed_on_first_open() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        let version: i64 = mgr
            .with_conn(&t, |c| {
                Ok(c.query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .sql()?
                .parse()
                .unwrap())
            })
            .await
            .unwrap();
        assert_eq!(version, crate::schema::SCHEMA_VERSION);
    }

    /// The root is created lazily, on the open path, so that an uncreatable
    /// root is a `BackendError` rather than a panic in `TenantManager::new`
    /// (preflight F6). Both halves are asserted: a root several levels below
    /// an existing directory is created, and a root under a *file* is not.
    #[tokio::test]
    async fn an_uncreatable_root_is_an_error_and_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let t = TenantId::new("t").unwrap();

        let nested = TenantManager::new(dir.path().join("a/b/c"), 8);
        nested.with_conn(&t, |_| Ok(())).await.unwrap();
        assert!(dir.path().join("a/b/c/t.db").exists());

        // A root whose parent is a regular file cannot be created.
        std::fs::write(dir.path().join("blocker"), b"not a directory").unwrap();
        let blocked = TenantManager::new(dir.path().join("blocker/root"), 8);
        let err = blocked.with_conn(&t, |_| Ok(())).await.unwrap_err();
        assert!(
            matches!(err, BackendError::Storage { .. }),
            "expected a storage error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn reopening_a_tenant_does_not_wipe_it() {
        let dir = tempfile::tempdir().unwrap();
        let t = TenantId::new("t").unwrap();
        {
            let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
            mgr.with_write(&t, |c| {
                c.execute("INSERT INTO meta(key,value) VALUES('probe','kept')", [])
                    .sql()?;
                Ok(())
            })
            .await
            .unwrap();
        }
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let value: String = mgr
            .with_conn(&t, |c| {
                c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))
                    .sql()
            })
            .await
            .unwrap();
        assert_eq!(value, "kept");
    }

    #[tokio::test]
    async fn the_pool_evicts_but_stays_correct_beyond_its_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 2);

        for i in 0..6 {
            let t = TenantId::new(&format!("tenant-{i}")).unwrap();
            mgr.with_write(&t, |c| {
                c.execute("INSERT INTO meta(key,value) VALUES('probe','v')", [])
                    .sql()?;
                Ok(())
            })
            .await
            .unwrap();
        }
        assert_eq!(mgr.open_count(), 6, "six tenants, six opens");

        // The first tenant was evicted from the pool; its data must survive.
        let t0 = TenantId::new("tenant-0").unwrap();
        let value: String = mgr
            .with_conn(&t0, |c| {
                c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))
                    .sql()
            })
            .await
            .unwrap();
        assert_eq!(value, "v");
        // The eviction is asserted, not claimed in a comment: reading tenant-0
        // again had to reopen its file, because a pool of two cannot still be
        // holding the first of six. A pool that ignored `max_open` would leave
        // this at 6.
        assert_eq!(
            mgr.open_count(),
            7,
            "tenant-0 was still pooled, so `max_open` was not honoured"
        );
    }

    /// The two pragmas the whole design rests on, asserted on a real tenant
    /// file. Neither was asserted anywhere before this test existed, and by
    /// this project's own standard an untested mechanism reads as removable.
    ///
    /// **Read what this covers narrowly.** It kills the deletion of
    /// `journal_mode=WAL`. It does **not** kill anything on the foreign-key
    /// side: the bundled SQLite is compiled with `DEFAULT_FOREIGN_KEYS` (it is
    /// in `PRAGMA compile_options`), so a fresh connection reads `1` before
    /// `initialise` touches it, and this assertion stays green with the
    /// pragma line deleted *and* with it wrapped in a transaction, where it is
    /// a documented silent no-op. Both mutants were run and both survived
    /// here. This test cannot escape that masking, because the manager opens
    /// the connection itself and nothing can force the pragma off first —
    /// `schema::tests::initialise_turns_foreign_keys_on_and_an_item_delete_cascades_to_its_vector`
    /// is the one that does, and it is what actually enforces the pragma.
    /// What this test adds is the end state on a **real file** rather than
    /// in memory, which is where WAL and the `-wal` sidecar can be seen at
    /// all.
    #[tokio::test]
    async fn a_freshly_opened_tenant_is_in_wal_mode_with_foreign_keys_on() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        let (journal, foreign_keys) = mgr
            .with_conn(&t, |c| {
                let journal: String = c.query_row("PRAGMA journal_mode", [], |r| r.get(0)).sql()?;
                let foreign_keys: i64 =
                    c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).sql()?;
                Ok((journal, foreign_keys))
            })
            .await
            .unwrap();

        assert_eq!(journal, "wal", "the tenant database is not in WAL mode");
        assert_eq!(
            foreign_keys, 1,
            "foreign keys are off; the vectors cascade does nothing"
        );
        assert!(
            dir.path().join("t.db-wal").exists(),
            "no WAL sidecar beside the tenant file"
        );
    }

    /// One `unwrap` in one closure must not take the tenant out for the life of
    /// the process. A panic poisons the connection mutex; left alone, the
    /// poisoned handle stays cached and every later call for that tenant fails
    /// with `retryable: false`, which nothing above retries. The pool evicts it
    /// and reopens instead.
    #[tokio::test]
    async fn a_panicking_closure_does_not_brick_the_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        mgr.with_write(&t, |c| {
            c.execute("INSERT INTO meta(key,value) VALUES('probe','kept')", [])
                .sql()?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(mgr.open_count(), 1);

        let blown = mgr
            .with_conn(&t, |_| -> Result<(), BackendError> {
                panic!("a closure in Task 20 unwrapped something")
            })
            .await;
        assert!(blown.is_err(), "a panicking closure returned Ok");

        // The tenant still works, and its data is still there.
        let value: String = mgr
            .with_conn(&t, |c| {
                c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))
                    .sql()
            })
            .await
            .unwrap();
        assert_eq!(value, "kept");
        assert_eq!(
            mgr.open_count(),
            2,
            "the poisoned handle was reused rather than evicted and reopened"
        );
    }

    /// `forget` is what `lib.rs`'s "tenant deletion is `rm`" must be paired
    /// with. It drops the pooled connection and nothing else — the data is
    /// still on disk and the next call reopens it.
    #[tokio::test]
    async fn a_forgotten_tenant_is_reopened_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        mgr.with_write(&t, |c| {
            c.execute("INSERT INTO meta(key,value) VALUES('probe','kept')", [])
                .sql()?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(mgr.open_count(), 1);

        mgr.forget(&t);
        let value: String = mgr
            .with_conn(&t, |c| {
                c.query_row("SELECT value FROM meta WHERE key='probe'", [], |r| r.get(0))
                    .sql()
            })
            .await
            .unwrap();
        assert_eq!(value, "kept");
        assert_eq!(mgr.open_count(), 2, "`forget` did not drop the pool entry");
    }

    /// Healing must **release** the poisoned connection, not merely stop using
    /// it. A closure that panics inside a write transaction leaves that
    /// connection holding SQLite's write lock, and it keeps holding it for as
    /// long as any `Arc` to it is alive. Evicting the cache's reference while
    /// the healing caller still holds its own leaves the replacement blocking
    /// on `busy_timeout` — 5s, and then `database is locked`, from a recovery
    /// path whose entire purpose is that one bad closure cannot take a tenant
    /// out.
    ///
    /// **This used to be a wall-clock proxy** (`elapsed < 2s`) and it was
    /// flaky: under CPU contention the *correct* path — release, then a fresh
    /// `~7ms` write — can itself take longer than any fixed budget for
    /// reasons that have nothing to do with the lock, so the bound produced
    /// false failures. Widening the bound was rejected too: it only makes the
    /// false failure rarer, and it directly weakens the real regression this
    /// test exists to catch, because a *genuinely* blocked replacement waits
    /// on SQLite's `busy_timeout` (5s) and a wider budget can simply
    /// accommodate that wait and pass anyway.
    ///
    /// The replacement below asserts the structural property the old
    /// message already named — *the write succeeds* — and nothing about
    /// timing. `busy_timeout` is what turns "the replacement waits on the
    /// poisoned lock" into a deterministic outcome either way: released in
    /// time, the write succeeds in single-digit milliseconds; not released,
    /// the write blocks for the full 5s and then fails with `database is
    /// locked`. Both outcomes are non-flaky; only their duration differs, and
    /// duration is not what this test checks. Confirmed by breaking the
    /// healing path (dropping `drop(handle)` in `with_conn`) and observing
    /// this assertion fail deterministically at ~5s with that exact error —
    /// see `d4-report.md`.
    #[tokio::test]
    async fn healing_releases_the_write_lock_the_panicking_closure_held() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TenantManager::new(dir.path().to_path_buf(), 8);
        let t = TenantId::new("t").unwrap();

        let blown = mgr
            .with_write(&t, |c| -> Result<(), BackendError> {
                c.execute_batch("BEGIN IMMEDIATE").sql()?;
                c.execute(
                    "INSERT INTO meta(key,value) VALUES('probe','uncommitted')",
                    [],
                )
                .sql()?;
                panic!("a closure in Task 20 unwrapped something mid-transaction")
            })
            .await;
        assert!(blown.is_err(), "a panicking closure returned Ok");

        // The structural property, not a timing proxy for it: if the poisoned
        // connection were still alive here, this write would contend for its
        // write lock and, bounded by `busy_timeout` (`schema::initialise`),
        // fail with `database is locked` rather than merely run slowly — so
        // success itself is the deterministic signal that the poisoned
        // connection was dropped before the reopen.
        let rows: i64 = mgr
            .with_write(&t, |c| {
                c.execute("INSERT INTO meta(key,value) VALUES('after','ok')", [])
                    .sql()?;
                c.query_row("SELECT COUNT(*) FROM meta", [], |r| r.get(0))
                    .sql()
            })
            .await
            .expect(
                "the replacement connection did not get a clean write lock: \
                 the poisoned connection was not dropped before the reopen",
            );
        // schema_version + after. The panicking transaction rolled back with
        // the connection it was opened on.
        assert_eq!(rows, 2, "the uncommitted write survived the panic");
    }

    /// **The rejected implementation is a `with_write` that acquires no lock at
    /// all** and simply forwards to `with_conn`.
    ///
    /// **The vacuity condition, which this test is built to escape:**
    /// `with_conn` runs its closure while holding the `Mutex<Connection>`, so
    /// **whenever the tenant has exactly one pooled connection the assertion
    /// below passes regardless of the write lock.** The briefed version of this
    /// test had exactly that shape, and deleting the async mutex left it green
    /// ten times out of ten.
    ///
    /// What escapes it is the `forget` churn: dropping the pool entry while a
    /// caller still holds the handle is what puts two live connections on one
    /// file, and two live connections is the only state in which the per-tenant
    /// async mutex is the thing serialising anything. The `sleep` widens the
    /// read-modify-write window so an unserialised second connection reliably
    /// observes the stale `n` rather than winning the race by luck.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn writes_to_one_tenant_are_serialized() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(TenantManager::new(dir.path().to_path_buf(), 8));
        let t = TenantId::new("t").unwrap();

        mgr.with_write(&t, |c| {
            c.execute_batch("CREATE TABLE counter(n INTEGER NOT NULL)")
                .sql()?;
            c.execute("INSERT INTO counter(n) VALUES(0)", []).sql()?;
            Ok(())
        })
        .await
        .unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let churn = {
            let mgr = Arc::clone(&mgr);
            let t = t.clone();
            let stop = Arc::clone(&stop);
            tokio::task::spawn_blocking(move || {
                while !stop.load(Ordering::Relaxed) {
                    mgr.forget(&t);
                    std::thread::sleep(Duration::from_micros(100));
                }
            })
        };

        let mut handles = Vec::new();
        for _ in 0..25 {
            let m = Arc::clone(&mgr);
            let t = t.clone();
            handles.push(tokio::spawn(async move {
                // Read-modify-write: only correct if writes are serialized.
                m.with_write(&t, |c| {
                    let n: i64 = c
                        .query_row("SELECT n FROM counter", [], |r| r.get(0))
                        .sql()?;
                    std::thread::sleep(Duration::from_millis(2));
                    c.execute("UPDATE counter SET n = ?1", [n + 1]).sql()?;
                    Ok(())
                })
                .await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        churn.await.unwrap();

        let n: i64 = mgr
            .with_conn(&t, |c| {
                c.query_row("SELECT n FROM counter", [], |r| r.get(0)).sql()
            })
            .await
            .unwrap();
        assert_eq!(n, 25, "writes were not serialized");
        // Without this the test could pass having never left the single-handle
        // case, which is the state in which it proves nothing.
        assert!(
            mgr.open_count() > 1,
            "the churn never forced a reopen, so the two-connection case this \
             test exists to cover was never created"
        );
    }
}
