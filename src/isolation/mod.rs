//! Git worktree isolation management.
//!
//! Provides safe isolation strategies for mission execution:
//! - `IsolationManager`: Creates and manages git worktrees
//! - `IsolationPlanner`: Analyzes risk and recommends isolation level

mod manager;
mod planner;

pub use manager::IsolationManager;
pub use planner::{IsolationPlanner, ScopeAnalysis};
