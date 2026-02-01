//! Failure recovery and retry logic.
//!
//! This module handles:
//! - Failure analysis and categorization
//! - Retry decisions with exponential backoff
//! - Context compaction when approaching limits
//! - Checkpoint management for recovery points
//! - Reasoning/hypothesis tracking for durable execution

mod analyzer;
mod checkpoint;
mod escalation;
mod executor;
mod reasoning;
mod retry_analyzer;
mod types;

pub use analyzer::FailureAnalyzer;
pub use checkpoint::{Checkpoint, CheckpointManager};
pub use escalation::{
    AttemptedStrategy, EscalationContext, EscalationHandler, EscalationStatus, EscalationUrgency,
    HumanAction, HumanResponse, StrategyOutcome,
};
pub use executor::RecoveryExecutor;
pub use reasoning::{
    ChosenOption, DecisionCategory, DecisionImpact, DecisionRecord, EvidenceRef, EvidenceRefType,
    Hypothesis, HypothesisStatus, ReasoningContext, ReasoningHistory, RejectedAlternative,
};
pub use retry_analyzer::{
    ErrorSignal, ExecutionErrorKind, FailureAttempt, FailureHistory, RetryAnalyzer, RetryDecision,
};
pub use types::{
    CompactionLevel, FailureAnalysis, FailureCategory, FailurePhase, RecoveryAction,
    RecoveryStatus, RecoveryStrategy, RetryAnalysisResult, RetryDecisionType,
};
