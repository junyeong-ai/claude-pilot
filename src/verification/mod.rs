mod archive;
mod checks;
mod convergent;
mod file_tracker;
mod history;
mod issue;
mod pattern_bank;
mod responses;
mod semantic;
mod solvability;
mod verifier;

pub use archive::{
    ArchiveStatistics, FixSummary, IssueSummary, LearnedPattern, PatternType,
    StrategyEffectiveness, TaskVerificationArchive, VerificationArchive, VerificationOutcome,
};
pub use checks::{Check, CheckResult, Verification, VerificationScope};
pub use convergent::{ConvergenceResult, ConvergentVerifier, PatternContext};
pub use file_tracker::{FileChanges, FileTracker};
pub use history::{
    ConvergenceState, FixStrategy, OscillationContext, OscillationPatternType, ProgressTrend,
    VerificationHistory,
};
pub use issue::{Issue, IssueCategory, IssueExtractor, IssueSeverity};
pub use pattern_bank::{
    ConsolidationResult, ErrorSignature, FixAttemptRecord, FixPattern, FixTrajectory, PatternBank,
    PatternBankStatistics, PatternMatch, TrajectoryOutcome,
};
pub use responses::{AiReviewIssue, AiReviewResponse};
pub use semantic::{SemanticValidation, SemanticValidator};
pub use solvability::{EarlyTerminationReason, SolvabilityAnalyzer, SuggestedAction};
pub use verifier::Verifier;
