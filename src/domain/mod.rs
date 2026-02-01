//! Core domain types - pure Rust, no external dependencies.
//!
//! This module contains the fundamental domain types that define
//! the business logic of Claude-Pilot. These types are:
//! - Serializable (serde)
//! - Schema-derivable (schemars for structured LLM output)
//! - Framework-independent

mod progress;

pub use progress::{ProgressAnalysis, ProgressSnapshot, ProgressTracker};

// Re-export escalation types from recovery module for API ergonomics
pub use crate::recovery::{
    AttemptedStrategy, EscalationContext, EscalationUrgency, HumanAction, HumanResponse,
    StrategyOutcome,
};
