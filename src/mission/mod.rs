//! Mission and task type definitions.
//!
//! Core domain types for mission execution:
//! - `Mission`: Top-level work unit with tasks and phases
//! - `Task`: Individual executable unit with dependencies
//! - `MissionStore`: Persistence layer for missions

mod status;
mod store;
mod task;
mod types;

pub use status::TaskStatus;
pub use store::MissionStore;
pub use task::{AgentType, Task, TaskResult};
pub use types::{
    IsolationMode, Learning, LearningCategory, Mission, MissionFlags, OnComplete, Phase, Priority,
    Progress, RiskLevel,
};
