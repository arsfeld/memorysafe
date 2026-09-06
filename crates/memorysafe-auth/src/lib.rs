//! Tenant-scoped API keys.
//!
//! §10 of the design says an API key identifies exactly one tenant and that
//! subject and namespace arrive per request and are validated against it. Both
//! network adapters need that, so it lives here rather than twice.

mod key;
mod store;

pub use key::{ApiKeyRecord, GeneratedKey, KEY_PREFIX, generate};
pub use store::{ApiKeyStore, Authenticated};

use memorysafe_core::CoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no credential presented")]
    Missing,
    #[error("credential is not a MemorySafe API key")]
    Malformed,
    /// Deliberately indistinguishable from a wrong secret. Splitting the two
    /// would let a caller enumerate valid key ids.
    #[error("unknown API key")]
    Unknown,
    #[error("API key is disabled")]
    Disabled,
    #[error("key is scoped to tenant {authorized}, request named {requested}")]
    WrongTenant {
        authorized: String,
        requested: String,
    },
    #[error("'{component}' is reserved and may not be used as a subject or namespace")]
    Reserved { component: &'static str },
    #[error(transparent)]
    Scope(#[from] CoreError),
    #[error("system randomness unavailable")]
    Rng,
}

impl AuthError {
    /// True when the caller has not established *who* they are — 401. False
    /// when they have and the answer is still no — 403.
    pub fn is_unauthenticated(&self) -> bool {
        matches!(
            self,
            AuthError::Missing | AuthError::Malformed | AuthError::Unknown
        )
    }
}
