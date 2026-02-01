//! Core agent contracts and task types.
//!
//! This module defines the fundamental contracts for the multi-agent system:
//! - `SpecializedAgent` trait for agent implementations
//! - `AgentTask` and `TaskContext` for task execution
//! - `AgentTaskResult` and `TaskArtifact` for execution results

mod agent;
mod result;
mod task;
mod utils;

pub use agent::{
    AgentCore, AgentMetrics, AgentPromptBuilder, AgentRole, BoxedAgent, LoadGuard, LoadTracker,
    MetricsSnapshot, SpecializedAgent,
};
pub use result::{
    AgentTaskResult, ArtifactType, ConvergenceResult, TaskArtifact, TaskStatus, VerificationVerdict,
};
pub use task::{AgentTask, TaskContext};
pub use utils::{
    calculate_priority_score, extract_field, extract_file_path, extract_files_from_output,
};
