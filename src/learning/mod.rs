//! Learning extraction and pattern retention.
//!
//! Captures and stores learnings from mission execution:
//! - `LearningExtractor`: Extracts learnings from task outputs
//! - `RetryHistory`: Tracks strategy effectiveness for future attempts
//! - `ClaudeCodeWriter`: Persists learnings to CLAUDE.md

mod candidates;
mod extractor;
mod history;
mod writer;

pub use candidates::{CandidateMetadata, CandidateType, ExtractionCandidate, ExtractionResult};
pub use extractor::{
    ExtractedAgent, ExtractedRule, ExtractedSkill, ExtractionResponse, LearningExtractor,
};
pub use history::{
    OutcomeStatus, RetryContext, RetryHistory, RetryOutcome, StrategyStats, best_strategy,
    extract_issue_signature, find_similar, strategy_effectiveness,
};
pub use writer::ClaudeCodeWriter;
