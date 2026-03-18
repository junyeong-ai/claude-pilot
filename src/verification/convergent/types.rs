use schemars::JsonSchema;
use serde::Deserialize;

use super::super::history::{ConvergenceState, FixStrategy, VerificationHistory};
use super::super::issue::Issue;
use crate::domain::Severity;

#[derive(Debug, Clone)]
pub struct ConvergenceResult {
    pub converged: bool,
    pub total_rounds: u32,
    pub history: VerificationHistory,
    pub state: ConvergenceState,
    pub final_issues: Vec<Issue>,
    pub persistent_issues: Vec<Issue>,
    pub termination_reason: Option<super::super::solvability::EarlyTerminationReason>,
}

impl ConvergenceResult {
    pub fn converged(history: VerificationHistory, state: ConvergenceState) -> Self {
        Self {
            converged: true,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues: Vec::new(),
            termination_reason: None,
        }
    }

    pub fn not_converged(
        history: VerificationHistory,
        state: ConvergenceState,
        persistent_issue_confidence: f32,
    ) -> Self {
        let persistent_issues =
            Self::extract_persistent_issues(&state, persistent_issue_confidence);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: None,
        }
    }

    pub fn early_terminated(
        history: VerificationHistory,
        state: ConvergenceState,
        reason: super::super::solvability::EarlyTerminationReason,
        persistent_issue_confidence: f32,
    ) -> Self {
        let persistent_issues =
            Self::extract_persistent_issues(&state, persistent_issue_confidence);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: Some(reason),
        }
    }

    pub fn oscillating(
        history: VerificationHistory,
        state: ConvergenceState,
        persistent_issue_confidence: f32,
    ) -> Self {
        let persistent_issues =
            Self::extract_persistent_issues(&state, persistent_issue_confidence);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: Some(super::super::solvability::EarlyTerminationReason::Oscillation),
        }
    }

    fn extract_persistent_issues(state: &ConvergenceState, confidence: f32) -> Vec<Issue> {
        state
            .issue_registry
            .persistent_issues(state.persistent_issue_threshold)
            .iter()
            .map(|entry| {
                Issue::new(
                    entry.category.clone(),
                    Severity::Error,
                    format!(
                        "{} (seen {} times)",
                        entry.signature, entry.total_occurrences
                    ),
                    confidence,
                )
            })
            .collect()
    }
}

/// Structured response for oscillation termination decision.
/// Uses boolean instead of string matching to avoid English keyword parsing.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OscillationDecision {
    /// true = terminate (stuck), false = continue (making progress)
    pub should_terminate: bool,
    /// Brief explanation for the decision
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Pattern context for LLM decision-making.
///
/// Contains pattern information with confidence scores so LLM can assess utility.
/// Not a hard gate - LLM sees all relevant patterns and decides based on context.
#[derive(Debug, Clone)]
pub struct PatternContext {
    pub pattern_id: String,
    pub strategy: FixStrategy,
    pub confidence: f32,
    pub success_rate: f32,
    pub total_uses: u32,
}

impl PatternContext {
    /// Format for LLM context.
    ///
    /// Provides confidence information so LLM can weigh the suggestion appropriately.
    /// Lower confidence patterns are still included - LLM judges their utility.
    pub fn format_for_llm(&self) -> String {
        let confidence_level = if self.confidence >= 0.8 {
            "HIGH"
        } else if self.confidence >= 0.6 {
            "MODERATE"
        } else if self.confidence >= 0.4 {
            "LOW"
        } else {
            "EXPERIMENTAL"
        };

        format!(
            r"**Historical Pattern** (confidence: {} - {:.0}%)
- Suggested strategy: {:?}
- Success rate: {:.0}% ({} applications)
Note: Use this as guidance. Your judgment on the specific context takes precedence.",
            confidence_level,
            self.confidence * 100.0,
            self.strategy,
            self.success_rate * 100.0,
            self.total_uses,
        )
    }
}
