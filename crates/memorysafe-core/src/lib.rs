//! Pure types and the governance policy trait. No I/O.

pub mod assessment;
pub mod audit;
pub mod capacity;
pub mod decision;
pub mod error;
pub mod ids;
pub mod item;
pub mod score;

pub use assessment::{
    Assessment, AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityCategory,
    SensitivityLevel,
};
pub use audit::{Actor, ActorKind, AuditEvent, AuditFilter, AuditRecord, ItemRef};
pub use capacity::{Budget, CapacityState, ScopeStats};
pub use decision::{Action, Decision, Eviction, MergeStrategy, PolicyId, Reason, ReasonCode};
pub use error::CoreError;
pub use ids::{AuditId, ItemId, Namespace, Scope, SubjectId, TenantId};
pub use item::{MemoryItem, Protection, Source, SourceKind};
pub use score::{FeatureMap, Score};
