//! SQLite backend: one database file per tenant.
//!
//! Isolation is structural rather than a query-layer invariant — backup is
//! `cp`, tenant deletion is `rm`, and per-tenant encryption is a key per file.
//!
//! **`rm` has a precondition.** A pooled connection keeps writing to an
//! unlinked inode: the writes land nowhere visible and reads return rows that
//! were deleted. Call [`SqliteBackend::forget_tenant`] first, let every
//! in-flight call for that tenant return, and only then remove `<tenant>.db`
//! together with its `-wal` and `-shm` sidecars.

pub mod schema;
pub mod tenant;

use memorysafe_core::TenantId;
use std::path::PathBuf;
use tenant::TenantManager;

pub struct SqliteBackend {
    pub(crate) tenants: TenantManager,
}

impl SqliteBackend {
    pub fn open(root: PathBuf) -> Self {
        Self::with_max_open(root, 64)
    }

    pub fn with_max_open(root: PathBuf, max_open: usize) -> Self {
        Self {
            tenants: TenantManager::new(root, max_open),
        }
    }

    /// Drops this backend's pooled connection for `tenant`. The operator
    /// half of tenant deletion — see the module doc, and
    /// [`TenantManager::forget`] for what it does and does not close.
    pub fn forget_tenant(&self, tenant: &TenantId) {
        self.tenants.forget(tenant);
    }
}
