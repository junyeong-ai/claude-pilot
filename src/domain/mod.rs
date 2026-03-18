//! Core domain types - pure Rust, no external dependencies.
//!
//! This module contains the fundamental domain types that define
//! the business logic of Claude-Pilot. These types are:
//! - Serializable (serde)
//! - Schema-derivable (schemars for structured LLM output)
//! - Framework-independent

mod primitives;
mod progress;

pub use primitives::{
    ConsensusStrategyKind, EscalationLevel, Quorum, RoundOutcome, Severity, VoteDecision,
};
pub use progress::{ProgressAnalysis, ProgressSnapshot, ProgressTracker};
