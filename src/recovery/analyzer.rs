use super::types::{FailureAnalysis, FailureCategory, FailurePhase};
use crate::error::{ExecutionError, PilotError};
use crate::mission::Mission;

pub struct FailureAnalyzer;

impl FailureAnalyzer {
    pub fn analyze(error: &PilotError, mission: &Mission, phase: FailurePhase) -> FailureAnalysis {
        let category = Self::categorize(error);
        let message = error.to_string();

        let mut analysis = FailureAnalysis::new(category, phase, &message);

        if let Some(task_id) = Self::extract_task_id(error) {
            analysis = analysis.with_task(task_id);
        }

        if let Some(checkpoint) = Self::find_last_checkpoint(mission) {
            analysis = analysis.with_checkpoint(checkpoint);
        }

        // Extract structured execution error info if available
        if let PilotError::AgentExecution(msg) = error {
            let exec_error = ExecutionError::from_message(msg);
            analysis = analysis.with_transient(exec_error.is_transient());
        }

        analysis
    }

    pub fn categorize(error: &PilotError) -> FailureCategory {
        match error {
            PilotError::VerificationFailed { .. } => Self::categorize_verification_failure(error),
            PilotError::BuildFailed(_) => FailureCategory::BuildFailure,
            PilotError::TestFailed(_) => FailureCategory::TestFailure,
            PilotError::Git(_) => FailureCategory::GitError,
            PilotError::AgentExecution(msg) => Self::categorize_from_execution_error(msg),
            PilotError::MaxRetriesExceeded(_) => FailureCategory::ResourceExhaustion,
            PilotError::MaxIterationsExceeded(_) => FailureCategory::ResourceExhaustion,
            PilotError::Planning(msg) => Self::categorize_planning_error(msg),
            PilotError::PlanValidation(_) => FailureCategory::InvalidOutput,
            PilotError::EvidenceGathering(_) => FailureCategory::AgentError,
            PilotError::Worktree { .. } => FailureCategory::GitError,
            PilotError::BranchExists(_) => FailureCategory::GitError,
            PilotError::Session(_) => FailureCategory::AgentError,
            _ => FailureCategory::Unknown,
        }
    }

    /// Categorize agent error using structured ExecutionError type.
    /// This replaces string-based pattern matching with type-safe classification.
    fn categorize_from_execution_error(msg: &str) -> FailureCategory {
        let exec_error = ExecutionError::from_message(msg);

        match exec_error {
            ExecutionError::Timeout { .. } | ExecutionError::ChunkTimeout { .. } => {
                FailureCategory::ExecutionTimeout
            }
            ExecutionError::RateLimited { .. } => FailureCategory::NetworkError,
            ExecutionError::ContextOverflow { .. } => FailureCategory::ContextOverflow,
            ExecutionError::NetworkError(_) => FailureCategory::NetworkError,
            ExecutionError::StreamError(_) => FailureCategory::AgentError,
            ExecutionError::ToolNotFound(_) => FailureCategory::ToolNotFound,
            ExecutionError::ParseError(_) => FailureCategory::InvalidOutput,
            ExecutionError::Other(_) => FailureCategory::AgentError,
        }
    }

    /// Categorize verification failure using structured check types.
    fn categorize_verification_failure(error: &PilotError) -> FailureCategory {
        use crate::error::FailedCheckType;

        if let PilotError::VerificationFailed { checks, .. } = error {
            for check in checks {
                // Prefer structured type information (type-safe)
                if let Some(check_type) = check.check_type {
                    return match check_type {
                        FailedCheckType::Build => FailureCategory::BuildFailure,
                        FailedCheckType::Test => FailureCategory::TestFailure,
                        FailedCheckType::Lint => FailureCategory::LintFailure,
                        FailedCheckType::TypeCheck => FailureCategory::BuildFailure,
                        FailedCheckType::FileOperation => FailureCategory::BuildFailure,
                        FailedCheckType::Custom => FailureCategory::BuildFailure,
                    };
                }
            }
        }

        FailureCategory::BuildFailure
    }

    fn categorize_planning_error(_msg: &str) -> FailureCategory {
        FailureCategory::AgentError
    }

    fn extract_task_id(error: &PilotError) -> Option<String> {
        match error {
            PilotError::TaskNotFound { task_id, .. } => Some(task_id.clone()),
            _ => None,
        }
    }

    fn find_last_checkpoint(mission: &Mission) -> Option<String> {
        let completed_count = mission
            .tasks
            .iter()
            .filter(|t| t.status == crate::mission::TaskStatus::Completed)
            .count();

        if completed_count > 0 {
            Some(format!("checkpoint-{}", completed_count))
        } else {
            None
        }
    }

    /// Check if a failure can be retried based on category and current retry count.
    pub fn is_retryable(category: FailureCategory, retry_count: u32) -> bool {
        retry_count < category.max_retries()
    }

    /// Determine if a failure should be escalated to human intervention.
    pub fn should_escalate(analysis: &FailureAnalysis) -> bool {
        !analysis.category.is_recoverable()
            || !Self::is_retryable(analysis.category, analysis.retry_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorize_token_limit_api_error() {
        // Real error message from Claude API - using structured ExecutionError
        let error_msg = "API error (HTTP 400): prompt is too long: 212893 tokens > 200000 maximum";
        let category = FailureAnalyzer::categorize_from_execution_error(error_msg);
        assert_eq!(category, FailureCategory::ContextOverflow);
    }

    #[test]
    fn test_categorize_token_limit_variations() {
        // Only unambiguous patterns that match explicit API responses
        let patterns = [
            "prompt is too long: 100000 tokens > 50000 maximum",
            "tokens > 50000 limit",
            "maximum context length exceeded",
            "context_length_exceeded",
        ];

        for pattern in patterns {
            let category = FailureAnalyzer::categorize_from_execution_error(pattern);
            assert_eq!(
                category,
                FailureCategory::ContextOverflow,
                "Failed for pattern: {}",
                pattern
            );
        }
    }

    #[test]
    fn test_categorize_timeout() {
        // Must use explicit timeout patterns
        let category =
            FailureAnalyzer::categorize_from_execution_error("timed out after 30 seconds");
        assert_eq!(category, FailureCategory::ExecutionTimeout);
    }

    #[test]
    fn test_categorize_network() {
        // Must use explicit HTTP codes for network errors
        let category = FailureAnalyzer::categorize_from_execution_error("HTTP 502: Bad Gateway");
        assert_eq!(category, FailureCategory::NetworkError);
    }

    #[test]
    fn test_categorize_generic_agent_error() {
        let category = FailureAnalyzer::categorize_from_execution_error("some unknown error");
        assert_eq!(category, FailureCategory::AgentError);
    }

    #[test]
    fn test_execution_error_transient_detection() {
        // Only explicit patterns are recognized as transient
        let transient_errors = [
            "timed out after 30s",
            "chunk timeout after 60s",
            "HTTP 502: Bad Gateway",
            "429 Too Many Requests",
        ];

        for msg in transient_errors {
            let exec_error = ExecutionError::from_message(msg);
            assert!(
                exec_error.is_transient(),
                "Expected transient error for: {}",
                msg
            );
        }

        // Ambiguous messages are classified as Other (permanent by default)
        let permanent_or_ambiguous = ["some unknown error", "generic failure"];

        for msg in permanent_or_ambiguous {
            let exec_error = ExecutionError::from_message(msg);
            assert!(
                exec_error.is_permanent(),
                "Expected permanent error for: {}",
                msg
            );
        }
    }

    #[test]
    fn test_execution_error_retry_hints() {
        use crate::config::ExecutionErrorConfig;
        let config = ExecutionErrorConfig::default();

        let timeout = ExecutionError::Timeout {
            operation: "test".to_string(),
            duration_secs: 60,
        };
        assert_eq!(timeout.max_retries(&config), 3);
        assert!(timeout.suggested_delay(&config).as_secs() > 0);

        let context_overflow = ExecutionError::ContextOverflow {
            message: "too long".to_string(),
        };
        assert_eq!(context_overflow.max_retries(&config), 0);
    }
}
