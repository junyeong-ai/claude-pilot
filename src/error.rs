use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

use crate::config::ExecutionErrorConfig;

#[derive(Debug, Clone)]
pub enum ExecutionError {
    Timeout {
        operation: String,
        duration_secs: u64,
    },
    ChunkTimeout {
        duration_secs: u64,
    },
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    ContextOverflow {
        message: String,
    },
    NetworkError(String),
    StreamError(String),
    ToolNotFound(String),
    ParseError(String),
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

    pub fn suggested_delay(&self, config: &ExecutionErrorConfig) -> Duration {
        match self {
            Self::Timeout { .. } => Duration::from_secs(config.timeout_delay_secs),
            Self::ChunkTimeout { .. } => Duration::from_secs(config.chunk_timeout_delay_secs),
            Self::RateLimited { retry_after_secs } => Duration::from_secs(
                retry_after_secs.unwrap_or(config.rate_limit_default_delay_secs),
            ),
            Self::NetworkError(_) => Duration::from_secs(config.network_error_delay_secs),
            Self::StreamError(_) => Duration::from_secs(config.stream_error_delay_secs),
            _ => Duration::from_secs(0),
        }
    }

    pub fn max_retries(&self, config: &ExecutionErrorConfig) -> u32 {
        match self {
            Self::Timeout { .. } => config.timeout_retries,
            Self::ChunkTimeout { .. } => config.chunk_timeout_retries,
            Self::RateLimited { .. } => config.rate_limit_retries,
            Self::NetworkError(_) => config.network_error_retries,
            Self::StreamError(_) => config.stream_error_retries,
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
            if msg.contains("chunk") {
                return Self::ChunkTimeout { duration_secs: 60 };
            }
            return Self::Timeout {
                operation: "execution".to_string(),
                duration_secs: 60,
            };
        }

        // Everything else - don't guess, let LLM or retry analyzer decide
        Self::Other(msg.to_string())
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
        let numbers: Vec<usize> = msg
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .filter(|&n| n > 1000) // Only large numbers likely to be token counts
            .collect();

        match numbers.as_slice() {
            [estimated, max, ..] => (*estimated, *max),
            [single] => (*single, 0),
            _ => (0, 0),
        }
    }
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout {
                operation,
                duration_secs,
            } => {
                write!(f, "Timeout after {}s: {}", duration_secs, operation)
            }
            Self::ChunkTimeout { duration_secs } => {
                write!(
                    f,
                    "Chunk timeout after {}s (no data received)",
                    duration_secs
                )
            }
            Self::RateLimited { retry_after_secs } => {
                if let Some(secs) = retry_after_secs {
                    write!(f, "Rate limited, retry after {}s", secs)
                } else {
                    write!(f, "Rate limited")
                }
            }
            Self::ContextOverflow { message } => write!(f, "Context overflow: {}", message),
            Self::NetworkError(msg) => write!(f, "Network error: {}", msg),
            Self::StreamError(msg) => write!(f, "Stream error: {}", msg),
            Self::ToolNotFound(tool) => write!(f, "Tool not found: {}", tool),
            Self::ParseError(msg) => write!(f, "Parse error: {}", msg),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for ExecutionError {}

#[derive(Error, Debug)]
pub enum PilotError {
    #[error("Mission not found: {0}")]
    MissionNotFound(String),

    #[error("Mission already exists: {0}")]
    MissionAlreadyExists(String),

    #[error("Invalid mission state: expected {expected}, got {actual}")]
    InvalidMissionState { expected: String, actual: String },

    #[error("Task not found: {mission_id}/{task_id}")]
    TaskNotFound { mission_id: String, task_id: String },

    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[error("Worktree error: {message}")]
    Worktree { message: String, path: PathBuf },

    #[error("Branch already exists: {0}")]
    BranchExists(String),

    #[error("Verification failed: {message}")]
    VerificationFailed {
        message: String,
        checks: Vec<FailedCheck>,
    },

    #[error("Build failed: {0}")]
    BuildFailed(String),

    #[error("Test failed: {0}")]
    TestFailed(String),

    #[error("Agent execution failed: {0}")]
    AgentExecution(String),

    #[error("Max retries exceeded for task: {0}")]
    MaxRetriesExceeded(String),

    #[error("Max iterations exceeded for mission: {0}")]
    MaxIterationsExceeded(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Model resolution failed: {0}")]
    ModelResolution(String),

    #[error("Not in a git repository")]
    NotInGitRepo,

    #[error("Project not initialized. Run 'claude-pilot init' first.")]
    NotInitialized,

    #[error("Claude CLI not found. Please install Claude Code.")]
    ClaudeCliNotFound,

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

    #[error("User clarification required: {0}")]
    ClarificationRequired(String),

    #[error("Mission paused")]
    MissionPaused,

    #[error("Mission cancelled")]
    MissionCancelled,

    #[error("Mission {mission_id} is already running (PID: {pid}). Use --force to override.")]
    MissionAlreadyRunning { mission_id: String, pid: u32 },

    #[error("Failed to acquire lock for mission: {mission_id}")]
    LockAcquisitionFailed { mission_id: String },

    #[error("Session error: {0}")]
    Session(String),

    #[error("Recovery error: {0}")]
    Recovery(String),

    #[error("Invalid state transition: {from} → {to} (allowed: {allowed})")]
    InvalidStateTransition {
        from: String,
        to: String,
        allowed: String,
    },

    #[error("State persistence failed: {0}")]
    StatePersistence(String),

    #[error("WAL corrupted: {0}")]
    WalCorrupted(String),

    #[error("State not initialized")]
    StateNotInitialized,

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

    #[error("SDK initialization failed: {0}")]
    SdkInit(String),

    #[error("SDK build failed: {0}")]
    SdkBuild(String),

    #[error("State error: {0}")]
    State(String),

    #[error("Event store error: {0}")]
    EventStore(String),

    #[error("Pattern bank error: {0}")]
    PatternBank(String),

    #[error("Agent coordination error: {0}")]
    AgentCoordination(String),

    #[error("Agent error: {0}")]
    Agent(String),

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
