//! Pure types and the governance policy trait. No I/O.

pub mod assessment;
pub mod audit;
pub mod capacity;
pub mod decision;
pub mod embedding;
pub mod error;
pub mod ids;
pub mod item;
pub mod policy;
pub mod recall;
pub mod score;

pub use assessment::{
    Assessment, AssessorId, RedundancyAssessment, SensitivityAssessment, SensitivityCategory,
    SensitivityLevel,
};
pub use audit::{Actor, ActorKind, AuditEvent, AuditFilter, AuditRecord, ItemRef};
pub use capacity::{Budget, CapacityState, ScopeStats};
pub use decision::{Action, Decision, Eviction, MergeStrategy, PolicyId, Reason, ReasonCode};
pub use embedding::{EmbedderId, Embedding};
pub use error::CoreError;
pub use ids::{AuditId, ItemId, Namespace, Scope, SubjectId, TenantId};
pub use item::{MemoryItem, Protection, Source, SourceKind};
pub use policy::{
    AdmitContext, AssessContext, Assessed, Candidate, ComposeContext, GovernancePolicy,
    MaintainContext, PolicyError,
};
pub use recall::{
    OMITTED_CAP, OmittedItem, RecallBudget, RecallMode, RecallRequest, ScoredCandidate,
    SelectedItem, WorkingSet,
};
pub use score::{FeatureMap, Score};
