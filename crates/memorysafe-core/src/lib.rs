//! Pure types and the governance policy trait. No I/O.

pub mod error;
pub mod ids;
pub mod score;

pub use error::CoreError;
pub use ids::{AuditId, ItemId, Namespace, Scope, SubjectId, TenantId};
pub use score::{FeatureMap, Score};
