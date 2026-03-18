use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use crate::config::ExecutionErrorConfig;

#[derive(Debug, Clone, Error)]
pub enum ExecutionError {
    #[error("Timeout after {duration_secs}s: {operation}")]
    Timeout {
        operation: String,
        duration_secs: u64,
    },
    #[error("Chunk timeout after {duration_secs}s (no data received)")]
    ChunkTimeout {
        duration_secs: u64,
    },
    #[error("Rate limited{}", retry_after_secs.map(|s| format!(", retry after {}s", s)).unwrap_or_default())]
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    #[error("Context overflow: {message}")]
    ContextOverflow {
        message: String,
    },
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Stream error: {0}")]
    StreamError(String),
    #[error("Tool not found: {0}")]
    ToolNotFound(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("{0}")]
    Other(String),
}

impl ExecutionError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Timeout { .. }
                | Self::ChunkTimeout { .. }
                | Self::RateLimited { .. }
                | Self::NetworkError(_)
                | Self::StreamError(_)
        )
    }

    pub fn is_permanent(&self) -> bool {
        !self.is_transient()
    }

    pub fn suggested_delay(&self, config: &ExecutionErrorConfig, attempt: u32) -> Duration {
        match self {
            Self::Timeout { .. } => config.timeout.delay_for_attempt(attempt),
            Self::ChunkTimeout { .. } => config.chunk_timeout.delay_for_attempt(attempt),
            Self::RateLimited { retry_after_secs } => {
                // Server-specified retry-after takes precedence (no backoff)
                if let Some(secs) = retry_after_secs {
                    return Duration::from_secs(*secs);
                }
                config.rate_limit.delay_for_attempt(attempt)
            }
            Self::NetworkError(_) => config.network_error.delay_for_attempt(attempt),
            Self::StreamError(_) => config.stream_error.delay_for_attempt(attempt),
            _ => Duration::from_secs(0),
        }
    }

    pub fn max_retries(&self, config: &ExecutionErrorConfig) -> u32 {
        match self {
            Self::Timeout { .. } => config.timeout.retries,
            Self::ChunkTimeout { .. } => config.chunk_timeout.retries,
            Self::RateLimited { .. } => config.rate_limit.retries,
            Self::NetworkError(_) => config.network_error.retries,
            Self::StreamError(_) => config.stream_error.retries,
            _ => 0,
        }
    }

    /// Parse an error message into a structured ExecutionError.
    /// Only matches unambiguous, structured patterns (HTTP codes, explicit keywords).
    /// Ambiguous messages are classified as Other - let LLM decide the appropriate handling.
    pub fn from_message(msg: &str) -> Self {
        // HTTP status codes - universal and unambiguous
        if msg.contains("429") || msg.contains("Too Many Requests") {
            return Self::RateLimited {
                retry_after_secs: Self::extract_retry_after(msg),
            };
        }
        if msg.contains("502") || msg.contains("503") || msg.contains("504") {
            return Self::NetworkError(msg.to_string());
        }

        // Token/context limits - explicit API responses
        if msg.contains("prompt is too long") || msg.contains("tokens >") {
            return Self::ContextOverflow {
                message: msg.to_string(),
            };
        }
        if msg.contains("maximum context length") || msg.contains("context_length_exceeded") {
            return Self::ContextOverflow {
                message: msg.to_string(),
            };
        }

        // Explicit timeout messages (usually from our own code or structured responses)
        if msg.contains("timed out after") || msg.contains("timeout after") {
            let duration_secs = Self::extract_duration_secs(msg).unwrap_or(0);
            if msg.contains("chunk") {
                return Self::ChunkTimeout { duration_secs };
            }
            return Self::Timeout {
                operation: "execution".to_string(),
                duration_secs,
            };
        }

        // Everything else - don't guess, let LLM or retry analyzer decide
        Self::Other(msg.to_string())
    }

    fn extract_duration_secs(msg: &str) -> Option<u64> {
        let msg_lower = msg.to_lowercase();
        for pattern in ["timed out after ", "timeout after "] {
            if let Some(idx) = msg_lower.find(pattern) {
                let after_pattern = &msg_lower[idx + pattern.len()..];
                let num_str: String = after_pattern
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(secs) = num_str.parse() {
                    return Some(secs);
                }
            }
        }
        None
    }

    fn extract_retry_after(msg: &str) -> Option<u64> {
        // Look for "retry after X" or "Retry-After: X" patterns
        // Use case-insensitive search that preserves byte alignment
        let msg_lower = msg.to_lowercase();
        for pattern in ["retry after ", "retry-after: ", "retry_after="] {
            if let Some(idx) = msg_lower.find(pattern) {
                // Extract digits starting after the pattern in the lowercased string
                // This avoids index mismatch issues between original and lowercased strings
                let after_pattern = &msg_lower[idx + pattern.len()..];
                let num_str: String = after_pattern
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(secs) = num_str.parse() {
                    return Some(secs);
                }
            }
        }
        None
    }

    /// Extract token counts from context overflow messages.
    /// Pattern: "212893 tokens > 200000" or similar
    pub fn extract_token_counts(msg: &str) -> (usize, usize) {
        /// Minimum value to be considered a token count (filters out small numbers
        /// like HTTP status codes or line numbers).
        const MIN_TOKEN_COUNT: usize = 1000;

        let numbers: Vec<usize> = msg
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .filter(|&n| n > MIN_TOKEN_COUNT)
            .collect();

        match numbers.as_slice() {
            [estimated, max, ..] => (*estimated, *max),
            [single] => (*single, 0),
            _ => (0, 0),
        }
    }
}


// --- Domain-specific sub-error enums ---

#[derive(Debug, Error)]
pub enum GitError {
    #[error(transparent)]
    Git(#[from] git2::Error),
    #[error("Worktree error at {path:?}: {message}")]
    Worktree { message: String, path: PathBuf },
    #[error("Not in a git repository")]
    NotInGitRepo,
}

#[derive(Debug, Error)]
pub enum MissionError {
    #[error("Mission not found: {0}")]
    NotFound(String),
    #[error("Invalid mission state: expected {expected}, got {actual}")]
    InvalidState { expected: String, actual: String },
    #[error("Mission {mission_id} is already running (PID: {pid}). Use --force to override.")]
    AlreadyRunning { mission_id: String, pid: u32 },
    #[error("Mission paused")]
    Paused,
    #[error("Mission cancelled")]
    Cancelled,
    #[error("Task not found: {mission_id}/{task_id}")]
    TaskNotFound { mission_id: String, task_id: String },
    #[error("Failed to acquire lock for mission: {mission_id}")]
    LockAcquisitionFailed { mission_id: String },
}

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("Verification failed: {message}")]
    Failed {
        message: String,
        checks: Vec<FailedCheck>,
    },
}

// --- Main error enum (wraps sub-errors + cross-cutting concerns) ---

#[derive(Error, Debug)]
pub enum PilotError {
    // Sub-error wrappers
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Mission(#[from] MissionError),
    #[error("State error: {0}")]
    State(String),
    #[error(transparent)]
    Verification(#[from] VerificationError),

    // Cross-cutting concerns (kept flat)
    #[error("Agent execution failed: {0}")]
    AgentExecution(String),

    #[error("Max iterations exceeded for mission: {0}")]
    MaxIterationsExceeded(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Model resolution failed: {0}")]
    ModelResolution(String),

    #[error("Project not initialized. Run 'claude-pilot init' first.")]
    NotInitialized,

    #[error("Planning failed: {0}")]
    Planning(String),

    #[error("Plan validation failed: {0}")]
    PlanValidation(String),

    #[error("Evidence gathering failed: {0}")]
    EvidenceGathering(String),

    #[error("Task decomposition failed: {0}")]
    TaskDecomposition(String),

    #[error("Learning extraction failed: {0}")]
    Learning(String),

    #[error("Recovery error: {0}")]
    Recovery(String),

    #[error("Context overflow: estimated {estimated} tokens exceeds limit of {max}")]
    ContextOverflow { estimated: usize, max: usize },

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Operation timed out: {0}")]
    Timeout(String),

    #[error("Human escalation required: {summary}")]
    EscalationRequired { summary: String },

    #[error("Escalation error: {0}")]
    Escalation(String),

    #[error("File ownership error: {0}")]
    Ownership(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml_bw::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("{0}")]
    Other(String),
}

/// Transitive conversion: git2::Error → GitError → PilotError
impl From<git2::Error> for PilotError {
    fn from(e: git2::Error) -> Self {
        PilotError::Git(GitError::from(e))
    }
}

/// Represents a failed verification check.
/// Contains both structured type (when available) and display information.
#[derive(Debug, Clone)]
pub struct FailedCheck {
    pub name: String,
    pub message: String,
    /// Structured check type for type-safe classification.
    pub check_type: Option<FailedCheckType>,
}

/// Structured check type for recovery strategy selection.
/// Mirrors verification::Check but kept separate to avoid circular dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailedCheckType {
    Build,
    Test,
    Lint,
    TypeCheck,
    FileOperation,
    Custom,
}

impl FailedCheck {
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
            check_type: None,
        }
    }

    pub fn with_type(mut self, check_type: FailedCheckType) -> Self {
        self.check_type = Some(check_type);
        self
    }
}

pub type Result<T> = std::result::Result<T, PilotError>;

impl From<ExecutionError> for PilotError {
    fn from(err: ExecutionError) -> Self {
        match err {
            ExecutionError::Timeout {
                operation,
                duration_secs,
            } => PilotError::Timeout(format!("{} (after {}s)", operation, duration_secs)),
            ExecutionError::ChunkTimeout { duration_secs } => {
                PilotError::Timeout(format!("chunk timeout after {}s", duration_secs))
            }
            ExecutionError::RateLimited { retry_after_secs } => {
                let msg = retry_after_secs
                    .map(|s| format!("rate limited, retry after {}s", s))
                    .unwrap_or_else(|| "rate limited".to_string());
                PilotError::AgentExecution(msg)
            }
            ExecutionError::ContextOverflow { ref message } => {
                let (estimated, max) = ExecutionError::extract_token_counts(message);
                PilotError::ContextOverflow { estimated, max }
            }
            ExecutionError::NetworkError(msg) => {
                PilotError::AgentExecution(format!("network: {}", msg))
            }
            ExecutionError::StreamError(msg) => {
                PilotError::AgentExecution(format!("stream: {}", msg))
            }
            ExecutionError::ToolNotFound(tool) => {
                PilotError::AgentExecution(format!("tool not found: {}", tool))
            }
            ExecutionError::ParseError(msg) => {
                PilotError::AgentExecution(format!("parse: {}", msg))
            }
            ExecutionError::Other(msg) => PilotError::AgentExecution(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionErrorConfig;

    // ── from_message classification ──

    #[test]
    fn from_message_http_429() {
        let err = ExecutionError::from_message("HTTP 429 Too Many Requests");
        assert!(matches!(err, ExecutionError::RateLimited { .. }));
    }

    #[test]
    fn from_message_http_gateway_errors() {
        for code in ["502", "503", "504"] {
            let err = ExecutionError::from_message(&format!("HTTP {} Bad Gateway", code));
            assert!(
                matches!(err, ExecutionError::NetworkError(_)),
                "Expected NetworkError for {code}, got {err:?}"
            );
        }
    }

    #[test]
    fn from_message_context_overflow() {
        for msg in [
            "prompt is too long",
            "150000 tokens > 128000",
            "maximum context length exceeded",
            "context_length_exceeded",
        ] {
            let err = ExecutionError::from_message(msg);
            assert!(
                matches!(err, ExecutionError::ContextOverflow { .. }),
                "Expected ContextOverflow for '{msg}', got {err:?}"
            );
        }
    }

    #[test]
    fn from_message_timeout() {
        let err = ExecutionError::from_message("timed out after 30s");
        assert!(matches!(
            err,
            ExecutionError::Timeout {
                duration_secs: 30,
                ..
            }
        ));
    }

    #[test]
    fn from_message_chunk_timeout() {
        let err = ExecutionError::from_message("chunk timed out after 10s");
        assert!(matches!(
            err,
            ExecutionError::ChunkTimeout {
                duration_secs: 10,
                ..
            }
        ));
    }

    #[test]
    fn from_message_unknown_falls_through() {
        let err = ExecutionError::from_message("something unexpected happened");
        assert!(matches!(err, ExecutionError::Other(_)));
    }

    // ── extract helpers ──

    #[test]
    fn extract_duration_secs_normal() {
        assert_eq!(
            ExecutionError::extract_duration_secs("timed out after 45s"),
            Some(45)
        );
    }

    #[test]
    fn extract_duration_secs_alternate_pattern() {
        assert_eq!(
            ExecutionError::extract_duration_secs("timeout after 120 seconds"),
            Some(120)
        );
    }

    #[test]
    fn extract_duration_secs_none_when_missing() {
        assert_eq!(
            ExecutionError::extract_duration_secs("connection refused"),
            None
        );
    }

    #[test]
    fn extract_retry_after_header() {
        assert_eq!(
            ExecutionError::extract_retry_after("Retry-After: 60"),
            Some(60)
        );
    }

    #[test]
    fn extract_retry_after_prose() {
        assert_eq!(
            ExecutionError::extract_retry_after("please retry after 30 seconds"),
            Some(30)
        );
    }

    #[test]
    fn extract_retry_after_none_when_missing() {
        assert_eq!(
            ExecutionError::extract_retry_after("rate limit exceeded"),
            None
        );
    }

    #[test]
    fn extract_token_counts_pair() {
        let (estimated, max) = ExecutionError::extract_token_counts("212893 tokens > 200000");
        assert_eq!(estimated, 212893);
        assert_eq!(max, 200000);
    }

    #[test]
    fn extract_token_counts_single() {
        let (estimated, max) = ExecutionError::extract_token_counts("prompt has 150000 tokens");
        assert_eq!(estimated, 150000);
        assert_eq!(max, 0);
    }

    #[test]
    fn extract_token_counts_filters_small_numbers() {
        // Numbers below MIN_TOKEN_COUNT (1000) should be filtered out
        let (estimated, max) = ExecutionError::extract_token_counts("line 42: 150000 tokens");
        assert_eq!(estimated, 150000);
        assert_eq!(max, 0);
    }

    // ── transient/permanent classification ──

    #[test]
    fn transient_errors() {
        let cases: Vec<ExecutionError> = vec![
            ExecutionError::Timeout {
                operation: "test".into(),
                duration_secs: 30,
            },
            ExecutionError::ChunkTimeout { duration_secs: 10 },
            ExecutionError::RateLimited {
                retry_after_secs: None,
            },
            ExecutionError::NetworkError("conn reset".into()),
            ExecutionError::StreamError("broken pipe".into()),
        ];
        for err in &cases {
            assert!(err.is_transient(), "Expected transient: {err:?}");
            assert!(!err.is_permanent(), "Expected not permanent: {err:?}");
        }
    }

    #[test]
    fn permanent_errors() {
        let cases: Vec<ExecutionError> = vec![
            ExecutionError::ContextOverflow {
                message: "too long".into(),
            },
            ExecutionError::ToolNotFound("missing".into()),
            ExecutionError::ParseError("bad json".into()),
            ExecutionError::Other("unknown".into()),
        ];
        for err in &cases {
            assert!(err.is_permanent(), "Expected permanent: {err:?}");
            assert!(!err.is_transient(), "Expected not transient: {err:?}");
        }
    }

    // ── suggested_delay with backoff ──

    #[test]
    fn suggested_delay_increases_with_attempts() {
        let config = ExecutionErrorConfig::default();
        let err = ExecutionError::Timeout {
            operation: "test".into(),
            duration_secs: 30,
        };

        let d0 = err.suggested_delay(&config, 0);
        let d1 = err.suggested_delay(&config, 1);
        let d2 = err.suggested_delay(&config, 2);

        // Each attempt should roughly double (with jitter, so check order of magnitude)
        assert!(d1.as_secs_f64() > d0.as_secs_f64(), "d1 > d0");
        assert!(d2.as_secs_f64() > d1.as_secs_f64(), "d2 > d1");
    }

    #[test]
    fn suggested_delay_capped_at_max() {
        let config = ExecutionErrorConfig::default();
        let err = ExecutionError::NetworkError("test".into());

        // Attempt 100 should still be capped at max_delay_secs
        let d = err.suggested_delay(&config, 100);
        let max = config.network_error.max_delay_secs as f64;
        let tolerance = max * config.network_error.jitter_ratio;
        assert!(d.as_secs_f64() <= max + tolerance + 1.0);
    }

    #[test]
    fn suggested_delay_rate_limited_uses_server_value() {
        let config = ExecutionErrorConfig::default();
        let err = ExecutionError::RateLimited {
            retry_after_secs: Some(60),
        };

        // Server-specified value should be used regardless of attempt
        assert_eq!(err.suggested_delay(&config, 0).as_secs(), 60);
        assert_eq!(err.suggested_delay(&config, 5).as_secs(), 60);
    }

    #[test]
    fn suggested_delay_permanent_is_zero() {
        let config = ExecutionErrorConfig::default();
        let err = ExecutionError::ContextOverflow {
            message: "too long".into(),
        };
        assert_eq!(err.suggested_delay(&config, 0), Duration::from_secs(0));
    }

    // ── PilotError conversion ──

    #[test]
    fn from_execution_error_timeout() {
        let err: PilotError = ExecutionError::Timeout {
            operation: "build".into(),
            duration_secs: 60,
        }
        .into();
        match err {
            PilotError::Timeout(msg) => {
                assert!(msg.contains("build"), "msg: {msg}");
                assert!(msg.contains("60"), "msg: {msg}");
            }
            other => panic!("Expected Timeout, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_rate_limited() {
        let err: PilotError = ExecutionError::RateLimited {
            retry_after_secs: Some(30),
        }
        .into();
        match err {
            PilotError::AgentExecution(msg) => {
                assert!(msg.contains("rate limited"), "msg: {msg}");
                assert!(msg.contains("30"), "msg: {msg}");
            }
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_context_overflow() {
        let err: PilotError = ExecutionError::ContextOverflow {
            message: "212893 tokens > 200000".into(),
        }
        .into();
        match err {
            PilotError::ContextOverflow { estimated, max } => {
                assert_eq!(estimated, 212893);
                assert_eq!(max, 200000);
            }
            other => panic!("Expected ContextOverflow, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_chunk_timeout() {
        let err: PilotError = ExecutionError::ChunkTimeout { duration_secs: 45 }.into();
        match err {
            PilotError::Timeout(msg) => assert!(msg.contains("45"), "msg: {msg}"),
            other => panic!("Expected Timeout, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_network() {
        let err: PilotError = ExecutionError::NetworkError("connection reset".into()).into();
        match err {
            PilotError::AgentExecution(msg) => assert!(msg.contains("network"), "msg: {msg}"),
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_stream() {
        let err: PilotError = ExecutionError::StreamError("broken pipe".into()).into();
        match err {
            PilotError::AgentExecution(msg) => assert!(msg.contains("stream"), "msg: {msg}"),
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_tool_not_found() {
        let err: PilotError = ExecutionError::ToolNotFound("missing_tool".into()).into();
        match err {
            PilotError::AgentExecution(msg) => {
                assert!(msg.contains("tool not found"), "msg: {msg}");
                assert!(msg.contains("missing_tool"), "msg: {msg}");
            }
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_parse() {
        let err: PilotError = ExecutionError::ParseError("bad json".into()).into();
        match err {
            PilotError::AgentExecution(msg) => assert!(msg.contains("parse"), "msg: {msg}"),
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_execution_error_other() {
        let err: PilotError = ExecutionError::Other("something failed".into()).into();
        match err {
            PilotError::AgentExecution(msg) => {
                assert_eq!(msg, "something failed");
            }
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    // ── Edge cases ──

    #[test]
    fn extract_token_counts_at_min_boundary() {
        // 999 is below MIN_TOKEN_COUNT (1000), should be filtered
        let (estimated, max) = ExecutionError::extract_token_counts("999 tokens > 500");
        assert_eq!(estimated, 0);
        assert_eq!(max, 0);

        // 1001 is above MIN_TOKEN_COUNT, should be included
        let (estimated, max) = ExecutionError::extract_token_counts("1001 tokens");
        assert_eq!(estimated, 1001);
        assert_eq!(max, 0);
    }

    #[test]
    fn extract_token_counts_no_numbers() {
        let (estimated, max) = ExecutionError::extract_token_counts("no numbers here");
        assert_eq!(estimated, 0);
        assert_eq!(max, 0);
    }

    #[test]
    fn extract_retry_after_query_param() {
        assert_eq!(
            ExecutionError::extract_retry_after("retry_after=120"),
            Some(120)
        );
    }

    #[test]
    fn from_execution_error_rate_limited_without_retry_after() {
        let err: PilotError = ExecutionError::RateLimited {
            retry_after_secs: None,
        }
        .into();
        match err {
            PilotError::AgentExecution(msg) => assert_eq!(msg, "rate limited"),
            other => panic!("Expected AgentExecution, got: {other:?}"),
        }
    }

    #[test]
    fn from_message_overlapping_429_and_timeout() {
        // 429 check comes first in from_message — should classify as RateLimited
        let err = ExecutionError::from_message("429 timed out after 30s");
        assert!(matches!(err, ExecutionError::RateLimited { .. }));
    }

    #[test]
    fn from_message_empty_string() {
        let err = ExecutionError::from_message("");
        assert!(matches!(err, ExecutionError::Other(_)));
    }

    #[test]
    fn extract_duration_secs_decimal_truncates() {
        // Implementation takes digits only, so "30.5s" extracts 30
        assert_eq!(
            ExecutionError::extract_duration_secs("timed out after 30.5s"),
            Some(30)
        );
    }

    #[test]
    fn max_retries_for_all_variants() {
        let config = ExecutionErrorConfig::default();

        assert_eq!(
            ExecutionError::Timeout { operation: "x".into(), duration_secs: 1 }.max_retries(&config),
            config.timeout.retries
        );
        assert_eq!(
            ExecutionError::ChunkTimeout { duration_secs: 1 }.max_retries(&config),
            config.chunk_timeout.retries
        );
        assert_eq!(
            ExecutionError::RateLimited { retry_after_secs: None }.max_retries(&config),
            config.rate_limit.retries
        );
        assert_eq!(
            ExecutionError::NetworkError("x".into()).max_retries(&config),
            config.network_error.retries
        );
        assert_eq!(
            ExecutionError::StreamError("x".into()).max_retries(&config),
            config.stream_error.retries
        );
        // Permanent errors have 0 retries
        assert_eq!(ExecutionError::ContextOverflow { message: "x".into() }.max_retries(&config), 0);
        assert_eq!(ExecutionError::ToolNotFound("x".into()).max_retries(&config), 0);
        assert_eq!(ExecutionError::ParseError("x".into()).max_retries(&config), 0);
        assert_eq!(ExecutionError::Other("x".into()).max_retries(&config), 0);
    }
}
