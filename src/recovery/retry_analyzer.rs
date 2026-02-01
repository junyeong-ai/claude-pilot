use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::agent::TaskAgent;
use crate::config::{RetryAnalyzerConfig, RetryHistoryConfig};
use crate::error::{ExecutionError, Result};
use crate::learning::{
    RetryContext, RetryHistory, RetryOutcome, best_strategy, extract_issue_signature, find_similar,
};
use crate::mission::Task;
use crate::planning::Evidence;

use super::types::{RetryAnalysisResult, RetryDecisionType};

/// Truncate error message to include in decision context (UTF-8 safe).
fn truncate_error_context(message: &str, max_len: usize) -> String {
    let first_line = message.lines().next().unwrap_or(message);
    crate::utils::truncate_with_marker(first_line, max_len)
}

/// Structured error signal for deterministic classification.
/// These signals enable fast, accurate classification without LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ErrorSignal {
    ExitCode(i32),
    HttpStatus(u16),
    /// HTTP response with Retry-After header hint (in seconds)
    HttpStatusWithRetry {
        status: u16,
        retry_after_secs: u64,
    },
    Execution(ExecutionErrorKind),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionErrorKind {
    Timeout,
    ChunkTimeout,
    RateLimited,
    ContextOverflow,
    NetworkError,
    StreamError,
    ToolNotFound,
    ParseError,
}

impl From<&ExecutionError> for ExecutionErrorKind {
    fn from(err: &ExecutionError) -> Self {
        match err {
            ExecutionError::Timeout { .. } => Self::Timeout,
            ExecutionError::ChunkTimeout { .. } => Self::ChunkTimeout,
            ExecutionError::RateLimited { .. } => Self::RateLimited,
            ExecutionError::ContextOverflow { .. } => Self::ContextOverflow,
            ExecutionError::NetworkError(_) => Self::NetworkError,
            ExecutionError::StreamError(_) => Self::StreamError,
            ExecutionError::ToolNotFound(_) => Self::ToolNotFound,
            ExecutionError::ParseError(_) => Self::ParseError,
            ExecutionError::Other(_) => Self::ParseError, // fallback
        }
    }
}

/// Transience classification with confidence level.
/// LLM should be consulted for Ambiguous cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransienceCategory {
    /// Definitely transient - safe to retry immediately (e.g., HTTP 429 rate limit)
    DefinitivelyTransient,
    /// Likely transient but context matters - LLM should verify
    LikelyTransient,
    /// Cannot determine - LLM judgment required
    Ambiguous,
    /// Likely permanent but context might reveal otherwise
    LikelyPermanent,
    /// Definitely permanent - do not retry (e.g., 404 not found)
    DefinitivelyPermanent,
}

impl TransienceCategory {
    pub fn is_definitive(&self) -> bool {
        matches!(
            self,
            Self::DefinitivelyTransient | Self::DefinitivelyPermanent
        )
    }

    pub fn should_retry(&self) -> Option<bool> {
        match self {
            Self::DefinitivelyTransient => Some(true),
            Self::DefinitivelyPermanent => Some(false),
            _ => None, // LLM decides
        }
    }
}

impl ErrorSignal {
    pub fn transience(&self) -> TransienceCategory {
        match self {
            // HTTP with Retry-After: definitely transient, server tells us when to retry
            Self::HttpStatusWithRetry {
                status: 429 | 503, ..
            } => TransienceCategory::DefinitivelyTransient,
            Self::HttpStatusWithRetry { status, .. } if *status >= 500 => {
                TransienceCategory::LikelyTransient
            }
            Self::HttpStatusWithRetry { .. } => TransienceCategory::Ambiguous,

            // HTTP status codes without Retry-After
            Self::HttpStatus(429) => TransienceCategory::DefinitivelyTransient, // Rate limit
            Self::HttpStatus(502 | 504) => TransienceCategory::LikelyTransient, // Gateway errors
            Self::HttpStatus(503) => TransienceCategory::Ambiguous, // Could be temp or permanent
            Self::HttpStatus(400 | 422) => TransienceCategory::DefinitivelyPermanent, // Bad request
            Self::HttpStatus(401 | 403) => TransienceCategory::LikelyPermanent, // Auth - might be fixable
            Self::HttpStatus(404) => TransienceCategory::DefinitivelyPermanent, // Not found

            // Exit codes - PLATFORM DEPENDENT
            // NOTE: These codes are POSIX/Linux conventions. Windows uses different codes.
            // Using LikelyTransient instead of Definitive because:
            // 1. Exit code 124 = timeout on Linux (coreutils), but may mean something else on BSD/macOS
            // 2. Exit code 137 = killed by SIGKILL (128+9) on Linux
            // 3. Exit code 143 = killed by SIGTERM (128+15) on Linux
            // 4. Windows doesn't use these conventions at all
            Self::ExitCode(124 | 137 | 143) => TransienceCategory::LikelyTransient,
            Self::ExitCode(1 | 2) => TransienceCategory::Ambiguous, // Generic error
            // Exit 126 = cannot execute, 127 = command not found (POSIX standard)
            Self::ExitCode(126 | 127) => TransienceCategory::DefinitivelyPermanent,

            // Execution errors (internal, platform-independent)
            Self::Execution(kind) => match kind {
                ExecutionErrorKind::Timeout | ExecutionErrorKind::ChunkTimeout => {
                    TransienceCategory::DefinitivelyTransient
                }
                ExecutionErrorKind::RateLimited => TransienceCategory::DefinitivelyTransient,
                ExecutionErrorKind::NetworkError | ExecutionErrorKind::StreamError => {
                    TransienceCategory::LikelyTransient
                }
                ExecutionErrorKind::ContextOverflow | ExecutionErrorKind::ToolNotFound => {
                    TransienceCategory::DefinitivelyPermanent
                }
                ExecutionErrorKind::ParseError => TransienceCategory::LikelyPermanent,
            },

            _ => TransienceCategory::Ambiguous,
        }
    }

    /// Get suggested retry delay in seconds, if available from Retry-After header.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::HttpStatusWithRetry {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        }
    }

    /// Legacy method for compatibility - prefer `transience()` for nuanced decisions.
    pub fn is_transient(&self) -> bool {
        matches!(
            self.transience(),
            TransienceCategory::DefinitivelyTransient | TransienceCategory::LikelyTransient
        )
    }

    /// Legacy method for compatibility - prefer `transience()` for nuanced decisions.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self.transience(),
            TransienceCategory::DefinitivelyPermanent | TransienceCategory::LikelyPermanent
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RetryDecision {
    ContinueWithNewApproach {
        strategy: String,
        reasoning: String,
    },
    ContinueSameApproach {
        reason: String,
    },
    Escalate {
        reason: String,
        diagnosis: String,
        suggested_actions: Vec<String>,
    },
}

impl RetryDecision {
    pub fn should_retry(&self) -> bool {
        !matches!(self, Self::Escalate { .. })
    }

    pub fn is_escalation(&self) -> bool {
        matches!(self, Self::Escalate { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureAttempt {
    pub attempt_number: u32,
    pub error_message: String,
    pub error_category: String,
    pub approach_taken: Option<String>,
    #[serde(default)]
    pub signals: Vec<ErrorSignal>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailureHistory {
    pub task_id: String,
    pub task_description: String,
    pub attempts: Vec<FailureAttempt>,
    #[serde(default = "Utc::now")]
    pub last_updated_at: DateTime<Utc>,
}

impl FailureHistory {
    pub fn new(task: &Task) -> Self {
        Self {
            task_id: task.id.clone(),
            task_description: task.description.clone(),
            attempts: Vec::new(),
            last_updated_at: Utc::now(),
        }
    }

    pub fn add_attempt(
        &mut self,
        error: &str,
        category: &str,
        approach: Option<&str>,
        max_history: usize,
    ) {
        // Auto-extract signals from error message
        let signals = Self::extract_signals(error);
        self.add_attempt_with_signals(error, category, approach, signals, max_history);
    }

    pub fn add_attempt_with_signals(
        &mut self,
        error: &str,
        category: &str,
        approach: Option<&str>,
        signals: Vec<ErrorSignal>,
        max_history: usize,
    ) {
        self.attempts.push(FailureAttempt {
            attempt_number: self.attempts.len() as u32 + 1,
            error_message: error.to_string(),
            error_category: category.to_string(),
            approach_taken: approach.map(String::from),
            signals,
        });

        if self.attempts.len() > max_history {
            self.attempts.remove(0);
        }
        self.last_updated_at = Utc::now();
    }

    /// Extract structured signals from error message.
    fn extract_signals(error: &str) -> Vec<ErrorSignal> {
        let mut signals = Vec::new();
        let exec_error = ExecutionError::from_message(error);

        // Map ExecutionError to ErrorSignal
        match &exec_error {
            ExecutionError::Timeout { .. } => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::Timeout));
            }
            ExecutionError::ChunkTimeout { .. } => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::ChunkTimeout));
            }
            ExecutionError::RateLimited { .. } => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::RateLimited));
            }
            ExecutionError::ContextOverflow { .. } => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::ContextOverflow));
            }
            ExecutionError::NetworkError(_) => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::NetworkError));
            }
            ExecutionError::StreamError(_) => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::StreamError));
            }
            ExecutionError::ToolNotFound(_) => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::ToolNotFound));
            }
            ExecutionError::ParseError(_) => {
                signals.push(ErrorSignal::Execution(ExecutionErrorKind::ParseError));
            }
            ExecutionError::Other(_) => {
                // No signal for ambiguous errors - let pattern matching or LLM handle
            }
        }

        // Extract HTTP status codes with word boundary awareness
        // Prevents false positives like "4291" matching "429"
        let retry_after = Self::parse_retry_after(error);

        for code in [429, 502, 503, 504, 400, 401, 403, 404, 422] {
            if Self::contains_http_code(error, code) {
                // Use HttpStatusWithRetry if we have Retry-After header for this status
                if let Some(secs) = retry_after {
                    signals.push(ErrorSignal::HttpStatusWithRetry {
                        status: code,
                        retry_after_secs: secs,
                    });
                } else {
                    signals.push(ErrorSignal::HttpStatus(code));
                }
            }
        }

        signals
    }

    /// Check if error message contains HTTP status code with word boundaries.
    /// Prevents false positives (e.g., "4291" should not match "429").
    fn contains_http_code(error: &str, code: u16) -> bool {
        let code_str = code.to_string();
        let error_bytes = error.as_bytes();

        for (i, _) in error.match_indices(&code_str) {
            // Check character before (if exists)
            let before_ok = i == 0
                || !error_bytes
                    .get(i - 1)
                    .map(|&b| b.is_ascii_digit())
                    .unwrap_or(false);

            // Check character after (if exists)
            let after_ok = i + code_str.len() >= error.len()
                || !error_bytes
                    .get(i + code_str.len())
                    .map(|&b| b.is_ascii_digit())
                    .unwrap_or(false);

            if before_ok && after_ok {
                return true;
            }
        }

        false
    }

    /// Parse Retry-After value from error message.
    ///
    /// Uses HTTP header format (RFC 7231) which is language-agnostic.
    /// Prose formats like "retry after 30 seconds" are intentionally NOT parsed
    /// because they are English-specific and unreliable.
    ///
    /// Supported format: "Retry-After: 30" (HTTP header, case-insensitive)
    fn parse_retry_after(error: &str) -> Option<u64> {
        // Only parse RFC-defined HTTP header format (language-agnostic)
        // "Retry-After:" is the HTTP header name, consistent across all locales
        let error_lower = error.to_lowercase();

        // Look for "retry-after:" (HTTP header style - RFC 7231)
        if let Some(pos) = error_lower.find("retry-after:") {
            // Use same string for indexing (avoid byte offset mismatch)
            let after_colon = &error_lower[pos + 12..];
            return Self::extract_number_from_start(after_colon);
        }

        // Do NOT parse prose patterns like "retry after X seconds"
        // These are English-specific and unreliable across locales
        None
    }

    /// Extract leading number from a string (stops at non-digit).
    fn extract_number_from_start(s: &str) -> Option<u64> {
        let trimmed = s.trim_start();
        let num_str: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if num_str.is_empty() {
            return None;
        }
        num_str.parse().ok()
    }

    pub fn is_stale(&self, max_age: std::time::Duration) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_updated_at);
        // Clock skew (negative elapsed) → treat as stale to allow recovery
        elapsed.to_std().map(|d| d > max_age).unwrap_or(true)
    }

    pub fn has_repeated_identical_error(&self, consecutive_threshold: usize) -> bool {
        if self.attempts.len() < consecutive_threshold {
            return false;
        }
        let recent: Vec<_> = self
            .attempts
            .iter()
            .rev()
            .take(consecutive_threshold)
            .collect();
        recent
            .iter()
            .all(|a| a.error_message == recent[0].error_message)
    }

    pub fn error_categories(&self) -> Vec<String> {
        self.attempts
            .iter()
            .map(|a| a.error_category.clone())
            .collect()
    }
}

pub struct RetryAnalyzer {
    agent: Arc<TaskAgent>,
    config: RetryAnalyzerConfig,
    learning_history: RetryHistory,
    learning_config: RetryHistoryConfig,
}

impl RetryAnalyzer {
    pub fn new(
        agent: Arc<TaskAgent>,
        config: RetryAnalyzerConfig,
        learning_history: RetryHistory,
        learning_config: RetryHistoryConfig,
    ) -> Self {
        Self {
            agent,
            config,
            learning_history,
            learning_config,
        }
    }

    /// Analyze failure history and determine retry strategy.
    /// Evidence context improves LLM's ability to suggest relevant approaches.
    pub async fn analyze(
        &self,
        history: &FailureHistory,
        working_dir: &std::path::Path,
    ) -> Result<RetryDecision> {
        self.analyze_with_evidence(history, working_dir, None).await
    }

    /// Analyze with full Evidence context for better-informed retry decisions.
    pub async fn analyze_with_evidence(
        &self,
        history: &FailureHistory,
        working_dir: &std::path::Path,
        evidence: Option<&Evidence>,
    ) -> Result<RetryDecision> {
        if let Some(quick) = self.quick_analysis(history) {
            return Ok(quick);
        }

        if self.learning_config.enabled
            && let Some(learned) = self.learned_strategy_analysis(history).await?
        {
            return Ok(learned);
        }

        self.llm_analysis(history, working_dir, evidence).await
    }

    async fn learned_strategy_analysis(
        &self,
        history: &FailureHistory,
    ) -> Result<Option<RetryDecision>> {
        let last = match history.attempts.last() {
            Some(a) => a,
            None => return Ok(None),
        };

        let signature = extract_issue_signature(&last.error_category, &last.error_message);
        let outcomes = self.learning_history.load().await?;

        let similar = find_similar(&outcomes, &last.error_category, &signature);
        if similar.len() < self.learning_config.min_samples_for_learning {
            debug!(
                task_id = %history.task_id,
                samples = similar.len(),
                required = self.learning_config.min_samples_for_learning,
                "Insufficient samples for learned strategy"
            );
            return Ok(None);
        }

        if let Some((strategy, rate)) = best_strategy(
            &outcomes,
            &last.error_category,
            self.learning_config.min_samples_for_learning,
        ) && rate >= self.learning_config.min_success_rate
        {
            info!(
                task_id = %history.task_id,
                strategy = %strategy,
                success_rate = rate,
                "Using learned strategy"
            );
            return Ok(Some(RetryDecision::ContinueWithNewApproach {
                strategy,
                reasoning: format!(
                    "Learned strategy with {:.0}% success rate from {} past attempts",
                    rate * 100.0,
                    similar.len()
                ),
            }));
        }

        Ok(None)
    }

    pub async fn record_outcome(
        &self,
        history: &FailureHistory,
        strategy: &str,
        success: bool,
    ) -> Result<()> {
        if !self.learning_config.enabled {
            return Ok(());
        }

        let last = match history.attempts.last() {
            Some(a) => a,
            None => return Ok(()),
        };

        let signature = extract_issue_signature(&last.error_category, &last.error_message);

        let context = RetryContext {
            task_id: Some(history.task_id.clone()),
            files: Vec::new(),
            error_message: last.error_message.clone(),
            attempt_number: last.attempt_number,
        };

        let outcome = if success {
            RetryOutcome::success(&last.error_category, &signature, strategy, context)
        } else {
            RetryOutcome::failure(&last.error_category, &signature, strategy, context)
        };

        self.learning_history.append(&outcome).await?;
        debug!(
            task_id = %history.task_id,
            strategy = %strategy,
            success = success,
            "Recorded retry outcome"
        );

        Ok(())
    }

    /// Tiered error classification for retry decisions.
    ///
    /// - Tier 1: Structured signals (exit codes, HTTP status, ExecutionError) - fastest, most accurate
    /// - Tier 2: Pattern matching - fast fallback for unstructured errors
    /// - Tier 3: LLM analysis - for truly ambiguous cases (handled by caller)
    fn quick_analysis(&self, history: &FailureHistory) -> Option<RetryDecision> {
        if history.attempts.len() <= 1 {
            return Some(RetryDecision::ContinueSameApproach {
                reason: "First retry attempt".into(),
            });
        }

        let last_attempt = history.attempts.last()?;

        // Tier 1: Structured signals (deterministic, language-agnostic)
        if let Some(decision) = self.classify_by_signals(history, last_attempt) {
            return Some(decision);
        }

        // Tier 2: Pattern matching (fallback for unstructured errors)
        if let Some(decision) = self.detect_transient_error(history, last_attempt) {
            return Some(decision);
        }
        if let Some(decision) = self.detect_non_transient_error(last_attempt) {
            return Some(decision);
        }

        if history.has_repeated_identical_error(self.config.consecutive_error_threshold) {
            debug!(task_id = %history.task_id, "Detected consecutive identical errors");
        }

        // Tier 3: Defer to LLM (learned strategies or llm_analysis)
        None
    }

    /// Tier 1: Classify errors using structured signals.
    /// Returns Some(decision) if signals provide clear classification.
    fn classify_by_signals(
        &self,
        history: &FailureHistory,
        last_attempt: &FailureAttempt,
    ) -> Option<RetryDecision> {
        if last_attempt.signals.is_empty() {
            return None;
        }

        // Check for transient signals
        let has_transient = last_attempt.signals.iter().any(|s| s.is_transient());
        let has_permanent = last_attempt.signals.iter().any(|s| s.is_permanent());

        if has_transient && !has_permanent {
            if history.attempts.len() >= self.config.transient_error_retry_limit {
                return Some(RetryDecision::Escalate {
                    reason: format!(
                        "Transient error persisted after {} attempts",
                        history.attempts.len()
                    ),
                    diagnosis: self.describe_signals(&last_attempt.signals),
                    suggested_actions: vec![
                        "Check network connectivity".into(),
                        "Verify service availability".into(),
                    ],
                });
            }
            return Some(RetryDecision::ContinueSameApproach {
                reason: format!(
                    "Transient error: {}",
                    self.describe_signals(&last_attempt.signals)
                ),
            });
        }

        if has_permanent {
            let strategy = self.suggest_strategy_for_signals(&last_attempt.signals);
            return Some(RetryDecision::ContinueWithNewApproach {
                strategy,
                reasoning: format!(
                    "Permanent error detected: {}",
                    self.describe_signals(&last_attempt.signals)
                ),
            });
        }

        None
    }

    fn describe_signals(&self, signals: &[ErrorSignal]) -> String {
        signals
            .iter()
            .map(|s| match s {
                ErrorSignal::ExitCode(c) => format!("exit {c}"),
                ErrorSignal::HttpStatus(c) => format!("HTTP {c}"),
                ErrorSignal::HttpStatusWithRetry {
                    status,
                    retry_after_secs,
                } => {
                    format!("HTTP {status} (retry after {retry_after_secs}s)")
                }
                ErrorSignal::Execution(k) => format!("{k:?}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn suggest_strategy_for_signals(&self, signals: &[ErrorSignal]) -> String {
        for signal in signals {
            match signal {
                ErrorSignal::Execution(ExecutionErrorKind::ContextOverflow) => {
                    return "reduce_context".into();
                }
                ErrorSignal::Execution(ExecutionErrorKind::ToolNotFound) => {
                    return "check_tools".into();
                }
                ErrorSignal::Execution(ExecutionErrorKind::ParseError) => {
                    return "fix_output_format".into();
                }
                ErrorSignal::ExitCode(127) => return "check_command".into(),
                ErrorSignal::HttpStatus(401 | 403)
                | ErrorSignal::HttpStatusWithRetry {
                    status: 401 | 403, ..
                } => {
                    return "check_auth".into();
                }
                ErrorSignal::HttpStatus(404)
                | ErrorSignal::HttpStatusWithRetry { status: 404, .. } => {
                    return "check_endpoint".into();
                }
                ErrorSignal::HttpStatusWithRetry {
                    status: 429 | 503,
                    retry_after_secs,
                } => {
                    // For rate limiting, suggest waiting
                    return format!("wait_{}s", retry_after_secs);
                }
                _ => {}
            }
        }
        "investigate".into()
    }

    /// Detect transient errors using structured signals only.
    ///
    /// String pattern matching is unreliable (POSIX error names don't work on Windows,
    /// error messages vary by locale/language). Use only:
    /// - ErrorSignal::ExitCode for timeout (124) and kill (137/143)
    /// - ErrorSignal::HttpStatus for transient HTTP codes
    ///
    /// LLM handles all other transient detection via semantic understanding.
    fn detect_transient_error(
        &self,
        history: &FailureHistory,
        last_attempt: &FailureAttempt,
    ) -> Option<RetryDecision> {
        for signal in &last_attempt.signals {
            if signal.is_transient() {
                // Include error context snippet for downstream consumers
                let error_context = truncate_error_context(&last_attempt.error_message, 200);

                if history.attempts.len() >= self.config.transient_error_retry_limit {
                    return Some(RetryDecision::Escalate {
                        reason: format!(
                            "Transient error persisted after {} attempts",
                            history.attempts.len()
                        ),
                        diagnosis: format!("Signal: {:?}\nContext: {}", signal, error_context),
                        suggested_actions: vec![
                            "Check network connectivity".into(),
                            "Verify service availability".into(),
                        ],
                    });
                }

                debug!(
                    task_id = %history.task_id,
                    signal = ?signal,
                    attempt = history.attempts.len(),
                    "Detected transient signal"
                );

                return Some(RetryDecision::ContinueSameApproach {
                    reason: format!(
                        "Transient error ({:?}) - retry with same approach\nContext: {}",
                        signal, error_context
                    ),
                });
            }
        }

        None
    }

    /// Detect non-transient errors using structured signals only.
    ///
    /// String pattern matching is unreliable across platforms/locales.
    /// Use only ErrorSignal for detection. LLM handles semantic error analysis.
    fn detect_non_transient_error(&self, last_attempt: &FailureAttempt) -> Option<RetryDecision> {
        for signal in &last_attempt.signals {
            if signal.is_permanent() {
                let (strategy, reason) = match signal {
                    ErrorSignal::HttpStatus(401) => ("check_auth", "Authentication required"),
                    ErrorSignal::HttpStatus(403) => ("check_permissions", "Access forbidden"),
                    ErrorSignal::HttpStatus(404) => ("check_endpoint", "Resource not found"),
                    ErrorSignal::ExitCode(126) => ("check_permissions", "Permission denied"),
                    ErrorSignal::ExitCode(127) => ("check_command", "Command not found"),
                    _ => continue,
                };

                let error_context = truncate_error_context(&last_attempt.error_message, 200);
                return Some(RetryDecision::ContinueWithNewApproach {
                    strategy: strategy.into(),
                    reasoning: format!("{:?} - {}\nContext: {}", signal, reason, error_context),
                });
            }
        }

        None
    }

    async fn llm_analysis(
        &self,
        history: &FailureHistory,
        working_dir: &std::path::Path,
        evidence: Option<&Evidence>,
    ) -> Result<RetryDecision> {
        info!(task_id = %history.task_id, attempts = history.attempts.len(), "Running LLM retry analysis");

        let prompt = self.build_prompt(history, evidence);
        let response: RetryAnalysisResult = self
            .agent
            .run_prompt_structured(&prompt, working_dir)
            .await?;
        Ok(self.build_decision(response, history))
    }

    fn build_prompt(&self, history: &FailureHistory, evidence: Option<&Evidence>) -> String {
        let attempts = history
            .attempts
            .iter()
            .map(|a| {
                format!(
                    "Attempt {}: [{}] {}",
                    a.attempt_number, a.error_category, a.error_message
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let evidence_context = evidence
            .map(|e| {
                let files: Vec<_> = e
                    .codebase_analysis
                    .relevant_files
                    .iter()
                    .take(10)
                    .map(|f| {
                        format!(
                            "- {} ({:.0}% confidence): {}",
                            f.path,
                            f.confidence * 100.0,
                            f.relevance
                        )
                    })
                    .collect();

                let patterns: Vec<_> = e
                    .codebase_analysis
                    .existing_patterns
                    .iter()
                    .take(5)
                    .map(|p| format!("- {}", p))
                    .collect();

                let deps: Vec<_> = e
                    .dependency_analysis
                    .current_dependencies
                    .iter()
                    .take(5)
                    .map(|d| format!("- {} v{}: {}", d.name, d.version, d.purpose))
                    .collect();

                format!(
                    r"
Codebase Context:
Relevant Files:
{}

Existing Patterns:
{}

Dependencies:
{}",
                    if files.is_empty() {
                        "  (none identified)".to_string()
                    } else {
                        files.join("\n")
                    },
                    if patterns.is_empty() {
                        "  (none identified)".to_string()
                    } else {
                        patterns.join("\n")
                    },
                    if deps.is_empty() {
                        "  (none identified)".to_string()
                    } else {
                        deps.join("\n")
                    }
                )
            })
            .unwrap_or_default();

        format!(
            r"Analyze whether to retry or escalate this failed task.

Task: {} - {}
{evidence_context}
Failure History:
{}

Consider the codebase context when suggesting retry strategies.
Use existing patterns and relevant files to inform your approach.

Decide:
- retry_new_approach: If a different approach might succeed
- retry_same: If the error is transient (network, rate limit)
- escalate: If the problem cannot be fixed by retrying",
            history.task_id, history.task_description, attempts
        )
    }

    fn build_decision(
        &self,
        response: RetryAnalysisResult,
        history: &FailureHistory,
    ) -> RetryDecision {
        match response.decision {
            RetryDecisionType::RetryNewApproach => RetryDecision::ContinueWithNewApproach {
                strategy: response
                    .strategy
                    .unwrap_or_else(|| "Try alternative approach".into()),
                reasoning: response
                    .reasoning
                    .unwrap_or_else(|| "LLM suggested new approach".into()),
            },
            RetryDecisionType::RetrySame => RetryDecision::ContinueSameApproach {
                reason: response
                    .reasoning
                    .unwrap_or_else(|| "Transient error likely".into()),
            },
            RetryDecisionType::Escalate => RetryDecision::Escalate {
                reason: response
                    .reasoning
                    .unwrap_or_else(|| "Cannot determine fix path".into()),
                diagnosis: response
                    .diagnosis
                    .unwrap_or_else(|| format!("Task failed {} times", history.attempts.len())),
                suggested_actions: if response.suggested_actions.is_empty() {
                    vec!["Review error messages manually".into()]
                } else {
                    response.suggested_actions
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consecutive_error_detection() {
        let config = RetryAnalyzerConfig::default();
        let mut history = FailureHistory {
            task_id: "T001".into(),
            task_description: "Test".into(),
            attempts: Vec::new(),
            last_updated_at: Utc::now(),
        };

        history.add_attempt("Error A", "Build", None, config.max_error_history);
        assert!(!history.has_repeated_identical_error(config.consecutive_error_threshold));

        history.add_attempt("Error A", "Build", None, config.max_error_history);
        assert!(!history.has_repeated_identical_error(config.consecutive_error_threshold));

        history.add_attempt("Error A", "Build", None, config.max_error_history);
        assert!(history.has_repeated_identical_error(config.consecutive_error_threshold));

        history.add_attempt("Error B", "Test", None, config.max_error_history);
        assert!(!history.has_repeated_identical_error(config.consecutive_error_threshold));
    }

    #[test]
    fn test_build_decision() {
        let temp_dir = std::env::temp_dir();
        let analyzer = RetryAnalyzer::new(
            Arc::new(TaskAgent::new(Default::default())),
            RetryAnalyzerConfig::default(),
            RetryHistory::new(temp_dir.join("test_history.jsonl"), 90),
            RetryHistoryConfig::default(),
        );

        let history = FailureHistory {
            task_id: "T001".into(),
            task_description: "Test".into(),
            attempts: vec![],
            last_updated_at: Utc::now(),
        };

        // Test retry_new_approach
        let response = RetryAnalysisResult {
            decision: RetryDecisionType::RetryNewApproach,
            strategy: Some("Use different algorithm".into()),
            reasoning: Some("Previous approach failed".into()),
            diagnosis: None,
            suggested_actions: vec![],
        };
        let decision = analyzer.build_decision(response, &history);
        assert!(matches!(
            decision,
            RetryDecision::ContinueWithNewApproach { .. }
        ));

        // Test retry_same
        let response = RetryAnalysisResult {
            decision: RetryDecisionType::RetrySame,
            strategy: None,
            reasoning: Some("Network issue".into()),
            diagnosis: None,
            suggested_actions: vec![],
        };
        let decision = analyzer.build_decision(response, &history);
        assert!(matches!(
            decision,
            RetryDecision::ContinueSameApproach { .. }
        ));

        // Test escalate
        let response = RetryAnalysisResult {
            decision: RetryDecisionType::Escalate,
            strategy: None,
            reasoning: Some("Cannot fix".into()),
            diagnosis: Some("Root cause".into()),
            suggested_actions: vec!["Fix manually".into()],
        };
        let decision = analyzer.build_decision(response, &history);
        assert!(matches!(decision, RetryDecision::Escalate { .. }));
    }

    #[test]
    fn test_signal_extraction() {
        // HTTP status codes
        let signals = FailureHistory::extract_signals("API error (HTTP 429): rate limited");
        assert!(
            signals
                .iter()
                .any(|s| matches!(s, ErrorSignal::HttpStatus(429)))
        );
        assert!(
            signals
                .iter()
                .any(|s| matches!(s, ErrorSignal::Execution(ExecutionErrorKind::RateLimited)))
        );

        // Timeout errors
        let signals = FailureHistory::extract_signals("timed out after 60 seconds");
        assert!(
            signals
                .iter()
                .any(|s| matches!(s, ErrorSignal::Execution(ExecutionErrorKind::Timeout)))
        );

        // Context overflow
        let signals =
            FailureHistory::extract_signals("prompt is too long: 200000 tokens > 100000 maximum");
        assert!(signals.iter().any(|s| matches!(
            s,
            ErrorSignal::Execution(ExecutionErrorKind::ContextOverflow)
        )));

        // Unknown error - no signals
        let signals = FailureHistory::extract_signals("some random error message");
        assert!(signals.is_empty());
    }

    #[test]
    fn test_transience_categories() {
        // Definitive transient
        assert_eq!(
            ErrorSignal::HttpStatus(429).transience(),
            TransienceCategory::DefinitivelyTransient
        );
        assert_eq!(
            ErrorSignal::Execution(ExecutionErrorKind::RateLimited).transience(),
            TransienceCategory::DefinitivelyTransient
        );

        // Likely transient (context might matter)
        assert_eq!(
            ErrorSignal::ExitCode(124).transience(),
            TransienceCategory::LikelyTransient
        );
        assert_eq!(
            ErrorSignal::HttpStatus(502).transience(),
            TransienceCategory::LikelyTransient
        );

        // Ambiguous (LLM should decide)
        assert_eq!(
            ErrorSignal::HttpStatus(503).transience(),
            TransienceCategory::Ambiguous
        );
        assert_eq!(
            ErrorSignal::ExitCode(1).transience(),
            TransienceCategory::Ambiguous
        );

        // Likely permanent
        assert_eq!(
            ErrorSignal::HttpStatus(401).transience(),
            TransienceCategory::LikelyPermanent
        );

        // Definitive permanent
        assert_eq!(
            ErrorSignal::HttpStatus(404).transience(),
            TransienceCategory::DefinitivelyPermanent
        );
        assert_eq!(
            ErrorSignal::ExitCode(127).transience(),
            TransienceCategory::DefinitivelyPermanent
        );
    }

    #[test]
    fn test_legacy_transience_methods() {
        // is_transient includes both Definitive and Likely
        assert!(ErrorSignal::HttpStatus(429).is_transient());
        assert!(ErrorSignal::ExitCode(124).is_transient());
        assert!(!ErrorSignal::HttpStatus(404).is_transient());

        // is_permanent includes both Definitive and Likely
        assert!(ErrorSignal::HttpStatus(401).is_permanent());
        assert!(ErrorSignal::ExitCode(127).is_permanent());
        assert!(!ErrorSignal::HttpStatus(429).is_permanent());

        // Ambiguous cases return false for both
        assert!(!ErrorSignal::HttpStatus(503).is_transient());
        assert!(!ErrorSignal::HttpStatus(503).is_permanent());
    }

    #[test]
    fn test_auto_signal_extraction_on_add_attempt() {
        let config = RetryAnalyzerConfig::default();
        let mut history = FailureHistory {
            task_id: "T001".into(),
            task_description: "Test".into(),
            attempts: Vec::new(),
            last_updated_at: Utc::now(),
        };

        history.add_attempt(
            "API error (HTTP 429): Too Many Requests",
            "AgentError",
            None,
            config.max_error_history,
        );

        let attempt = history.attempts.last().unwrap();
        assert!(!attempt.signals.is_empty());
        assert!(attempt.signals.iter().any(|s| s.is_transient()));
    }
}
