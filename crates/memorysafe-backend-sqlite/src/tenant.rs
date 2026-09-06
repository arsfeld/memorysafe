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
        // using it — two live connections, and a `StdMutex<Connection>` that
        // no longer serialises anything. What closes it here is that the
        // loser's connection is **dropped before it is ever used**: the insert
        // re-checks the cache under the same lock that publishes the winner,
        // and yields. The cost of the race is one wasted open, not a second
        // live handle.
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

        let mut cache = self.cache.lock().expect("pool mutex");
        if let Some(winner) = cache.get(key) {
            return Ok(Arc::clone(winner));
        }
        cache.put(key.to_owned(), Arc::clone(&handle));
        Ok(handle)
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
/// [`Self::with_write`]**, never [`Self::with_conn`]. `with_conn` is for reads,
/// which are safe to run concurrently under WAL.
///
/// `memorysafe_backend::aggregates` requires that concurrent increments of one
/// aggregate row cannot interleave and asks each backend to say which of an
/// atomic upsert, a row lock or serialised writers it chose. This backend
/// serialises the writers, here.
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
    pub fn forget(&self, tenant: &TenantId) {
        let mut cache = self.pool.cache.lock().expect("pool mutex");
        cache.pop(tenant.as_str());
    }

    fn write_lock(&self, tenant: &TenantId) -> Arc<AsyncMutex<()>> {
        let mut locks = self.write_locks.lock().expect("write-lock map mutex");
        Arc::clone(
            locks
                .entry(tenant.as_str().to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    /// Runs `f` on the tenant's connection off the async runtime. Concurrent
    /// readers are fine under WAL.
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
            let mut conn = handle.lock().expect("connection mutex");
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
