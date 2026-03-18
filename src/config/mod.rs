//! Configuration types and loading.
//!
//! Provides all configuration structures for claude-pilot:
//! - `PilotConfig`: Top-level configuration with validation
//! - `AgentConfig`, `ModelConfig`: Agent and model settings
//! - Various domain configs: verification, recovery, planning, etc.

mod model;
mod settings;

pub(crate) use settings::validate_unit_f32;
pub use model::{DEFAULT_MODEL, ModelConfig};
pub use settings::{
    AgentConfig, AgentModelsConfig, AuthMode, BudgetAllocationConfig, BuildSystem,
    CheckpointConfig, ChunkedPlanningConfig, CompactionConfig, CompactionLevelConfig,
    ComplexityAssessmentConfig, ConsensusConfig, ConsensusEscalationConfig,
    ConsensusScoringConfig, ContextConfig, ContextEstimatorConfig, ContextLimitsConfig,
    ConvergentVerificationConfig, DiscoveryConfig, DisplayConfig, EvidenceBudgetConfig,
    ExecutionErrorConfig, FocusConfig, GitConfig, IsolationConfig, LearningConfig,
    MessagingConfig, ModelAgentType, ModuleConfig, MultiAgentConfig, NotificationConfig,
    OrchestratorConfig, PatternBankConfig, PhaseComplexityConfig, PilotConfig, ProjectPaths,
    QualityConfig, ReasoningConfig, RecoveryConfig, ResearchConfig, RetryAnalyzerConfig,
    RetryHistoryConfig, RulesConfig, SearchConfig, SelectionPolicy, SelectionWeightsConfig,
    StateConfig, TaskBudgetConfig, TaskDecompositionConfig, TaskScoringConfig, TaskScoringWeights,
    TierCrossVisibilityConfig, TierQuorumConfig, TokenEncoding, TokenizerConfig,
    VerificationCommands, VerificationConfig,
};
pub use crate::quality::{CoherenceConfig, EvidenceGatheringConfig, EvidenceSufficiencyConfig};
