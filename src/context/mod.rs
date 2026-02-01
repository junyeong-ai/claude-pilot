//! Context management for LLM interactions.
//!
//! This module handles:
//! - Token budget tracking and allocation across mission phases
//! - Context compaction when approaching token limits
//! - Mission context persistence and recovery
//! - Actual usage tracking from API responses

mod budget;
mod compaction;
mod estimator;
mod manager;
mod persistence;
mod task_context;
mod types;

pub use budget::{
    ActualUsage, BudgetAllocation, BudgetCategory, BudgetUsage, PhaseComplexity, TokenBudget,
};
pub use compaction::{CompactionArchive, CompactionResult, CompactionTrigger, ContextCompactor};
pub use estimator::{ContextEstimate, ContextEstimator, LoadingRecommendation};
pub use manager::ContextManager;
pub use persistence::ContextPersistence;
pub use task_context::TaskContextBudget;
pub use types::{
    CompactionState, MissionContext, MissionSummary, PhaseStatus, PhaseSummary, TaskEntry,
    TaskRegistry, VerificationStatus,
};
