//! Configuration types and loading.
//!
//! Provides all configuration structures for claude-pilot:
//! - `PilotConfig`: Top-level configuration with validation
//! - `AgentConfig`, `ModelConfig`: Agent and model settings
//! - Various domain configs: verification, recovery, planning, etc.

mod model;
mod settings;

pub use model::{DEFAULT_MODEL, ModelConfig};
pub use settings::{
    AgentConfig, AgentModelsConfig, AuthMode, BudgetAllocationConfig, BuildSystem,
    CheckpointConfig, ChunkedPlanningConfig, CompactionConfig, CompactionLevelConfig,
    ComplexityConfig, ConsensusConfig, ContextConfig, ContextEstimatorConfig, ContextLimitsConfig,
    ConvergentVerificationConfig, DiscoveryConfig, DisplayConfig, EvidenceConfig,
    ExecutionErrorConfig, FocusConfig, GitConfig, IsolationConfig, LearningConfig, ModelAgentType,
    ModuleConfig, MultiAgentConfig, NotificationConfig, OrchestratorConfig, PatternBankConfig,
    PhaseComplexityConfig, PilotConfig, ProjectPaths, QualityConfig, QuorumType, ReasoningConfig,
    RecoveryConfig, RetryAnalyzerConfig, RetryHistoryConfig, SearchConfig, SelectionPolicy,
    StateConfig, TaskBudgetConfig, TaskDecompositionConfig, TaskScoringConfig, TaskScoringWeights,
    TierCrossVisibilityConfig, TierQuorumConfig, TokenEncoding, TokenizerConfig,
    VerificationCommands, VerificationConfig,
};
