//! Pluggable strategy implementations.
//!
//! This module provides strategy traits and implementations for:
//! - Evidence gathering (multi-level escalation)
//! - Convergence (meta-strategy with level transitions)

pub mod convergence;
pub mod evidence;

pub use convergence::{
    ConvergenceContext, ConvergenceLevel, ConvergenceStrategy, MetaConvergenceOrchestrator,
    MetaConvergenceResult, MetaConvergenceState, PartialConvergencePolicy,
};
pub use evidence::{
    EvidenceEscalator, EvidenceGap, EvidenceResult, EvidenceStrategy, GapImportance,
    GatheringContext, GatheringOutcome,
};
