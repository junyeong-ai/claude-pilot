//! Shared types and contracts - foundation for all multi-agent modules.

mod consensus;
mod contracts;
mod identity;
mod module;
mod priority;

pub use consensus::{ConsensusOutcome, ConsensusSessionStatus, TierLevel, TierResult};
pub use identity::{AgentId, RoleCategory};
pub use module::PermissionProfile;
pub use priority::{TaskComplexity, TaskPriority};

pub(crate) use contracts::{
    ApiChange, ApiChangeType, ApiContract, ConvergenceCheckResult,
    GlobalConstraints, SemanticConflict, SemanticConflictType, TechDecision,
    TypeChange, TypeChangeType,
};
pub(crate) use module::QualifiedModule;
