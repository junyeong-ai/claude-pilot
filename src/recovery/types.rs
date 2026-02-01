use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    ContextOverflow,
    PlanningTimeout,
    ExecutionTimeout,
    BuildFailure,
    TestFailure,
    LintFailure,
    VerificationLoop,
    DependencyConflict,
    CircularDependency,
    GitError,
    ToolNotFound,
    NetworkError,
    AgentError,
    InvalidOutput,
    ResourceExhaustion,
    Unknown,
}

impl FailureCategory {
    /// Default retry count from RecoveryConfig.max_retries_per_failure.
    /// Use `max_retries_with_config()` when config is available.
    pub const DEFAULT_MAX_RETRIES: u32 = 3;

    /// Maximum retry attempts for this failure category.
    pub fn max_retries(self) -> u32 {
        self.max_retries_with_default(Self::DEFAULT_MAX_RETRIES)
    }

    /// Maximum retry attempts with a custom default.
    /// Use this when RecoveryConfig is available.
    pub fn max_retries_with_default(self, default: u32) -> u32 {
        // Only fundamentally unrecoverable categories get 0
        if self.is_fundamentally_unrecoverable() {
            return 0;
        }
        default
    }

    /// Categories that are fundamentally unrecoverable regardless of configuration.
    /// These require structural changes, not retries.
    fn is_fundamentally_unrecoverable(self) -> bool {
        matches!(
            self,
            Self::VerificationLoop | Self::CircularDependency | Self::ToolNotFound | Self::Unknown
        )
    }

    /// Whether this failure category is recoverable.
    pub fn is_recoverable(self) -> bool {
        !self.is_fundamentally_unrecoverable()
    }
}

impl std::fmt::Display for FailureCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContextOverflow => write!(f, "Context Overflow"),
            Self::PlanningTimeout => write!(f, "Planning Timeout"),
            Self::ExecutionTimeout => write!(f, "Execution Timeout"),
            Self::BuildFailure => write!(f, "Build Failure"),
            Self::TestFailure => write!(f, "Test Failure"),
            Self::LintFailure => write!(f, "Lint Failure"),
            Self::VerificationLoop => write!(f, "Verification Loop"),
            Self::DependencyConflict => write!(f, "Dependency Conflict"),
            Self::CircularDependency => write!(f, "Circular Dependency"),
            Self::GitError => write!(f, "Git Error"),
            Self::ToolNotFound => write!(f, "Tool Not Found"),
            Self::NetworkError => write!(f, "Network Error"),
            Self::AgentError => write!(f, "Agent Error"),
            Self::InvalidOutput => write!(f, "Invalid Output"),
            Self::ResourceExhaustion => write!(f, "Resource Exhaustion"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailurePhase {
    Planning,
    Execution,
    Verification,
    Finalization,
}

impl std::fmt::Display for FailurePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planning => write!(f, "planning"),
            Self::Execution => write!(f, "execution"),
            Self::Verification => write!(f, "verification"),
            Self::Finalization => write!(f, "finalization"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureAnalysis {
    pub category: FailureCategory,
    pub phase: FailurePhase,
    pub message: String,
    pub task_id: Option<String>,
    pub retry_count: u32,
    pub is_recoverable: bool,
    /// Whether this is a transient error (from ExecutionError type system)
    pub is_transient: bool,
    pub last_good_checkpoint: Option<String>,
    pub suggested_strategy: RecoveryStrategy,
    pub timestamp: DateTime<Utc>,
}

impl FailureAnalysis {
    pub fn new(category: FailureCategory, phase: FailurePhase, message: impl Into<String>) -> Self {
        let is_recoverable = category.is_recoverable();
        Self {
            category,
            phase,
            message: message.into(),
            task_id: None,
            retry_count: 0,
            is_recoverable,
            is_transient: is_recoverable, // Default: recoverable errors are transient
            last_good_checkpoint: None,
            suggested_strategy: Self::suggest_strategy(category),
            timestamp: Utc::now(),
        }
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_retry_count(mut self, count: u32) -> Self {
        self.retry_count = count;
        self
    }

    pub fn with_checkpoint(mut self, checkpoint_id: impl Into<String>) -> Self {
        self.last_good_checkpoint = Some(checkpoint_id.into());
        self
    }

    /// Set whether this is a transient error (from ExecutionError analysis).
    pub fn with_transient(mut self, is_transient: bool) -> Self {
        self.is_transient = is_transient;
        // Transient errors are always recoverable
        if is_transient {
            self.is_recoverable = true;
        }
        self
    }

    /// Suggests a recovery strategy for the given failure category.
    fn suggest_strategy(category: FailureCategory) -> RecoveryStrategy {
        // Uniform defaults - can be overridden by config or LLM
        const DEFAULT_RETRY_DELAY_SECS: u64 = 5;
        const DEFAULT_MAX_RETRIES: u32 = 3;
        const DEFAULT_FIX_ROUNDS: u32 = 5;
        const DEFAULT_CHUNK_REDUCTION: f32 = 0.5;
        const DEFAULT_TIMEOUT_INCREASE: f32 = 1.5;

        match category {
            // Recoverable via context management
            FailureCategory::ContextOverflow => RecoveryStrategy::CompactAndRetry {
                compaction_level: CompactionLevel::Aggressive,
                max_retries: DEFAULT_MAX_RETRIES,
            },

            // Recoverable via chunking/timeout
            FailureCategory::PlanningTimeout => RecoveryStrategy::ChunkAndRetry {
                chunk_size_reduction: DEFAULT_CHUNK_REDUCTION,
                timeout_increase: DEFAULT_TIMEOUT_INCREASE,
            },

            // Recoverable via convergent fix
            FailureCategory::BuildFailure
            | FailureCategory::TestFailure
            | FailureCategory::LintFailure => RecoveryStrategy::ConvergentFix {
                max_fix_rounds: DEFAULT_FIX_ROUNDS,
            },

            // Recoverable via simple retry (transient failures)
            FailureCategory::ExecutionTimeout
            | FailureCategory::GitError
            | FailureCategory::NetworkError
            | FailureCategory::AgentError
            | FailureCategory::InvalidOutput => RecoveryStrategy::SimpleRetry {
                delay_secs: DEFAULT_RETRY_DELAY_SECS,
                max_retries: DEFAULT_MAX_RETRIES,
            },

            // Recoverable via checkpoint
            FailureCategory::ResourceExhaustion => RecoveryStrategy::RollbackToCheckpoint {
                checkpoint_id: None,
                reset_retry_counts: true,
            },

            // Requires escalation
            FailureCategory::DependencyConflict => RecoveryStrategy::Escalate {
                reason: format!("{}", category),
                suggested_actions: vec![],
            },

            // Fundamentally unrecoverable - escalate
            FailureCategory::VerificationLoop
            | FailureCategory::CircularDependency
            | FailureCategory::ToolNotFound
            | FailureCategory::Unknown => RecoveryStrategy::Escalate {
                reason: format!("{}", category),
                suggested_actions: vec![],
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecoveryStrategy {
    CompactAndRetry {
        compaction_level: CompactionLevel,
        max_retries: u32,
    },
    ChunkAndRetry {
        chunk_size_reduction: f32,
        timeout_increase: f32,
    },
    ConvergentFix {
        max_fix_rounds: u32,
    },
    RollbackToCheckpoint {
        checkpoint_id: Option<String>,
        reset_retry_counts: bool,
    },
    SimpleRetry {
        delay_secs: u64,
        max_retries: u32,
    },
    Escalate {
        reason: String,
        suggested_actions: Vec<String>,
    },
    Skip {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionLevel {
    Light,
    Moderate,
    Aggressive,
    Emergency,
}

impl std::fmt::Display for CompactionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Light => write!(f, "light"),
            Self::Moderate => write!(f, "moderate"),
            Self::Aggressive => write!(f, "aggressive"),
            Self::Emergency => write!(f, "emergency"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecoveryStatus {
    Recovered {
        message: String,
        action: RecoveryAction,
    },
    Escalated {
        reason: String,
    },
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
        analysis: FailureAnalysis,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum RecoveryAction {
    #[default]
    Retry,
    RetryWithReducedChunks {
        reduction_factor: f32,
    },
    StartConvergentVerification {
        max_rounds: u32,
    },
}

impl RecoveryStatus {
    pub fn recovered(message: impl Into<String>) -> Self {
        Self::Recovered {
            message: message.into(),
            action: RecoveryAction::Retry,
        }
    }

    pub fn recovered_with_action(message: impl Into<String>, action: RecoveryAction) -> Self {
        Self::Recovered {
            message: message.into(),
            action,
        }
    }

    pub fn escalated(reason: impl Into<String>) -> Self {
        Self::Escalated {
            reason: reason.into(),
        }
    }

    pub fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }

    pub fn failed(reason: impl Into<String>, analysis: FailureAnalysis) -> Self {
        Self::Failed {
            reason: reason.into(),
            analysis,
        }
    }

    pub fn is_recovered(&self) -> bool {
        matches!(self, Self::Recovered { .. })
    }

    pub fn is_escalated(&self) -> bool {
        matches!(self, Self::Escalated { .. })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RetryAnalysisResult {
    pub decision: RetryDecisionType,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub diagnosis: Option<String>,
    #[serde(default)]
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecisionType {
    RetryNewApproach,
    RetrySame,
    Escalate,
}
