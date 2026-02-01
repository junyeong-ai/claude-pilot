//! Shared types and contracts - foundation for all multi-agent modules.

mod contracts;
mod types;

pub use contracts::{ApplyEvent, EventContext, ToEvent, ValidateMessage};
pub use types::{
    // Core types
    AgentId,
    // API/Type change tracking
    ApiChange,
    ApiChangeType,
    ApiContract,
    ConflictSeverity,
    ConsensusOutcome,
    ConstraintType,
    ConvergenceCheckResult,
    // Enhanced tier results
    EnhancedTierResult,
    // Global constraints (Phase 1)
    GlobalConstraints,
    PermissionProfile,
    // Cross-workspace types
    QualifiedModule,
    Quorum,
    RoleCategory,
    RoundOutcome,
    RuntimeConstraint,
    // Semantic conflicts
    SemanticConflict,
    SemanticConflictType,
    SessionStatus,
    TaskComplexity,
    TaskPriority,
    TechDecision,
    TierLevel,
    TierResult,
    TierTask,
    TypeChange,
    TypeChangeType,
    VoteDecision,
};
