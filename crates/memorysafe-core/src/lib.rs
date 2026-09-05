//! Pure types and the governance policy trait. No I/O.

pub mod error;
pub mod ids;

pub use error::CoreError;
pub use ids::{AuditId, ItemId, Namespace, Scope, SubjectId, TenantId};
