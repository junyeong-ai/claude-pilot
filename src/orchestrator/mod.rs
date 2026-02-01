//! Mission execution orchestration.
//!
//! Coordinates the complete mission lifecycle:
//! - `Orchestrator`: Main execution engine with task scheduling
//! - `LifecycleManager`: Process locking and heartbeat management
//! - `SignalHandler`: Graceful shutdown on SIGINT/SIGTERM
//! - `FocusTracker`: Anti-drift scope tracking

mod engine;
mod focus;
mod lifecycle;
mod signal;

pub use engine::Orchestrator;
pub use focus::{
    CompletionValidation, DriftWarning, ExpectedScope, FileClassification, FocusTracker,
    WarningSeverity,
};
pub use lifecycle::{LifecycleManager, LockGuard, LockInfo};
pub use signal::SignalHandler;
