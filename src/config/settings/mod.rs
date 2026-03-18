use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{PilotError, Result};
use crate::quality::{CoherenceConfig, EvidenceSufficiencyConfig};

// ── Validation helpers (used by PilotConfig::validate and child module validators) ──

pub(crate) fn validate_unit_f32(value: f32, name: &str, errors: &mut Vec<String>) {
    if !(0.0..=1.0).contains(&value) {
        errors.push(format!("{} must be between 0.0 and 1.0, got {}", name, value));
    }
}

fn validate_unit_f64(value: f64, name: &str, errors: &mut Vec<String>) {
    if !(0.0..=1.0).contains(&value) {
        errors.push(format!("{} must be between 0.0 and 1.0, got {}", name, value));
    }
}

mod agent;
mod multi_agent;
mod orchestrator;
mod quality;
mod recovery;
mod state;

pub const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    "node_modules", "target", ".git", "dist", "build",
    "__pycache__", ".venv", "vendor", "coverage",
    ".next", ".nuxt", ".gradle", "bin", "obj",
];

pub use agent::{
    AgentConfig, AgentModelsConfig, AuthMode, LearningConfig, ModelAgentType, RetryHistoryConfig,
};
pub use multi_agent::{
    ConsensusConfig, ConsensusEscalationConfig, ConsensusScoringConfig, DiscoveryConfig,
    MessagingConfig, MultiAgentConfig, ResearchConfig, SelectionPolicy, SelectionWeightsConfig,
    TierCrossVisibilityConfig, TierQuorumConfig,
};
pub use orchestrator::{
    ChunkedPlanningConfig, DisplayConfig, GitConfig, IsolationConfig, NotificationConfig,
    OrchestratorConfig,
};
pub use quality::{
    BuildSystem, ComplexityAssessmentConfig, EvidenceBudgetConfig, FocusConfig, ModuleConfig,
    PatternBankConfig, QualityConfig, SearchConfig, TaskDecompositionConfig, TaskScoringConfig,
    TaskScoringWeights, VerificationCommands, VerificationConfig,
};
pub use recovery::{
    CheckpointConfig, CompactionLevelConfig, ConvergentVerificationConfig,
    ExecutionErrorConfig, ReasoningConfig, RecoveryConfig, RetryAnalyzerConfig,
};
pub use state::{
    BudgetAllocationConfig, CompactionConfig, ContextConfig, ContextEstimatorConfig,
    ContextLimitsConfig, PhaseComplexityConfig, RulesConfig, StateConfig, TaskBudgetConfig,
    TokenEncoding, TokenizerConfig,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PilotConfig {
    pub orchestrator: OrchestratorConfig,
    pub isolation: IsolationConfig,
    pub verification: VerificationConfig,
    pub git: GitConfig,
    pub learning: LearningConfig,
    pub agent: AgentConfig,
    pub notification: NotificationConfig,
    pub context: ContextConfig,
    pub recovery: RecoveryConfig,
    pub chunked_planning: ChunkedPlanningConfig,
    pub quality: QualityConfig,
    pub complexity: ComplexityAssessmentConfig,
    pub evidence: EvidenceBudgetConfig,
    pub display: DisplayConfig,
    pub task_decomposition: TaskDecompositionConfig,
    pub search: SearchConfig,
    pub evidence_sufficiency: EvidenceSufficiencyConfig,
    pub coherence: CoherenceConfig,
    pub pattern_bank: PatternBankConfig,
    pub focus: FocusConfig,
    pub multi_agent: MultiAgentConfig,
    pub tokenizer: TokenizerConfig,
    pub task_scoring: TaskScoringConfig,
    pub state: StateConfig,
    pub rules: RulesConfig,
}

impl PilotConfig {
    pub async fn load(pilot_dir: &Path) -> Result<Self> {
        let config_path = pilot_dir.join("config.toml");
        let config = if config_path.exists() {
            let content = fs::read_to_string(&config_path).await?;
            toml::from_str(&content)?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub async fn save(&self, pilot_dir: &Path) -> Result<()> {
        self.validate()?;
        let config_path = pilot_dir.join("config.toml");
        let content =
            toml::to_string_pretty(self).map_err(|e| PilotError::Config(e.to_string()))?;
        fs::write(&config_path, content).await?;
        Ok(())
    }

    /// Validate configuration values for consistency and safety.
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        // ── Delegate to sub-config validators ──
        if let Err(e) = self.quality.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.recovery.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.multi_agent.convergent.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.coherence.thresholds.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.evidence_sufficiency.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.multi_agent.selection_weights.validate() {
            errors.push(e.to_string());
        }
        if let Err(e) = self.multi_agent.consensus.scoring.validate() {
            errors.push(e.to_string());
        }

        // ── Orchestrator ──
        if self.orchestrator.max_iterations == 0 {
            errors.push("max_iterations must be greater than 0".into());
        }
        if self.orchestrator.max_parallel_tasks == 0 {
            errors.push("max_parallel_tasks must be greater than 0".into());
        }

        // ── Isolation ──
        if self.isolation.small_change_threshold >= self.isolation.large_change_threshold {
            errors.push("small_change_threshold must be less than large_change_threshold".into());
        }

        // ── Agent ──
        if self.agent.timeout_secs == 0 {
            errors.push("agent timeout_secs must be greater than 0".into());
        }
        if self.agent.model.is_empty() {
            errors.push("agent model must not be empty".into());
        }

        // ── Context compaction ──
        let cc = &self.context.compaction;
        validate_unit_f32(cc.compaction_threshold, "compaction.compaction_threshold", &mut errors);
        validate_unit_f32(cc.aggressive_ratio, "compaction.aggressive_ratio", &mut errors);
        validate_unit_f32(cc.dedup_similarity_threshold, "compaction.dedup_similarity_threshold", &mut errors);

        // ── Context estimator ──
        validate_unit_f32(self.context.estimator.safety_margin, "context.estimator.safety_margin", &mut errors);
        validate_unit_f32(self.context.estimator.selective_loading_ratio, "context.estimator.selective_loading_ratio", &mut errors);

        // ── Budget allocation ──
        let ba = &self.context.budget_allocation;
        for (name, value) in [
            ("budget_allocation.mission_summary_ratio", ba.mission_summary_ratio),
            ("budget_allocation.current_phase_ratio", ba.current_phase_ratio),
            ("budget_allocation.current_task_ratio", ba.current_task_ratio),
            ("budget_allocation.recent_tasks_ratio", ba.recent_tasks_ratio),
            ("budget_allocation.learnings_ratio", ba.learnings_ratio),
            ("budget_allocation.evidence_ratio", ba.evidence_ratio),
            ("budget_allocation.reserved_output_ratio", ba.reserved_output_ratio),
        ] {
            validate_unit_f32(value, name, &mut errors);
        }
        {
            let ratio_sum = ba.mission_summary_ratio
                + ba.current_phase_ratio
                + ba.current_task_ratio
                + ba.recent_tasks_ratio
                + ba.learnings_ratio
                + ba.evidence_ratio
                + ba.reserved_output_ratio;
            if ratio_sum > 1.0 + f32::EPSILON {
                errors.push(format!(
                    "budget_allocation ratios must sum to at most 1.0, got {:.3}",
                    ratio_sum
                ));
            }
        }
        if ba.recent_tasks_available_divisor == 0 {
            errors.push("context.budget_allocation.recent_tasks_available_divisor must be greater than 0".into());
        }
        if ba.rebalance_reduction_divisor == 0 {
            errors.push("context.budget_allocation.rebalance_reduction_divisor must be greater than 0".into());
        }

        // ── Chunked planning (division-by-zero prevention) ──
        if self.chunked_planning.max_tokens_divisor == 0 {
            errors.push("chunked_planning.max_tokens_divisor must be greater than 0".into());
        }
        if self.chunked_planning.complexity_divisor == 0 {
            errors.push("chunked_planning.complexity_divisor must be greater than 0".into());
        }
        if self.chunked_planning.min_phases == 0 {
            errors.push("chunked_planning.min_phases must be greater than 0".into());
        }
        if self.chunked_planning.min_phases > self.chunked_planning.max_phases {
            errors.push("chunked_planning.min_phases must be <= max_phases".into());
        }

        // ── Phase complexity multipliers ──
        for (name, value) in [
            ("phase_complexity.simple_multiplier", self.context.phase_complexity.simple_multiplier),
            ("phase_complexity.moderate_multiplier", self.context.phase_complexity.moderate_multiplier),
            ("phase_complexity.complex_multiplier", self.context.phase_complexity.complex_multiplier),
            ("phase_complexity.critical_multiplier", self.context.phase_complexity.critical_multiplier),
        ] {
            if value <= 0.0 {
                errors.push(format!("{} must be greater than 0.0, got {}", name, value));
            }
        }

        // ── Complexity ──
        if self.complexity.assessment_timeout_secs == 0 {
            errors.push("complexity.assessment_timeout_secs must be greater than 0".into());
        }

        // ── Evidence budget ──
        let budget_sum = self.evidence.file_budget_percent
            + self.evidence.pattern_budget_percent
            + self.evidence.dependency_budget_percent
            + self.evidence.knowledge_budget_percent;
        if budget_sum != 100 {
            errors.push("evidence budget percentages must sum to 100".into());
        }
        validate_unit_f32(self.evidence.min_confidence_display, "evidence.min_confidence_display", &mut errors);
        validate_unit_f32(self.evidence.skill_confidence, "evidence.skill_confidence", &mut errors);
        validate_unit_f32(self.evidence.rule_confidence, "evidence.rule_confidence", &mut errors);

        // ── Cross-config consistency: evidence persistence ──
        if self.quality.require_verifiable_evidence && !self.recovery.checkpoint.persist_evidence {
            errors.push(
                "checkpoint.persist_evidence must be true when require_verifiable_evidence is true \
                 (evidence gathering is expensive; persistence enables durable recovery)".into(),
            );
        }

        // ── Learning ──
        validate_unit_f32(self.learning.min_confidence, "learning.min_confidence", &mut errors);
        if self.learning.retry_history.retention_days <= 0 {
            errors.push("learning.retry_history.retention_days must be positive".into());
        }
        validate_unit_f32(self.learning.retry_history.min_success_rate, "learning.retry_history.min_success_rate", &mut errors);

        // ── Pattern bank ──
        let pb = &self.pattern_bank;
        for (name, value) in [
            ("pattern_bank.min_success_rate", pb.min_success_rate),
            ("pattern_bank.min_confidence", pb.min_confidence),
            ("pattern_bank.merge_similarity_threshold", pb.merge_similarity_threshold),
            ("pattern_bank.min_similarity_threshold", pb.min_similarity_threshold),
            ("pattern_bank.trajectory_attempt_penalty", pb.trajectory_attempt_penalty),
            ("pattern_bank.trajectory_side_effect_penalty", pb.trajectory_side_effect_penalty),
            ("pattern_bank.initial_success_confidence", pb.initial_success_confidence),
            ("pattern_bank.initial_failure_confidence", pb.initial_failure_confidence),
            ("pattern_bank.confidence_success_rate_weight", pb.confidence_success_rate_weight),
            ("pattern_bank.recency_decay_rate", pb.recency_decay_rate),
        ] {
            validate_unit_f32(value, name, &mut errors);
        }
        if pb.max_patterns == 0 {
            errors.push("pattern_bank.max_patterns must be greater than 0".into());
        }
        {
            let weight_sum = pb.similarity_weight
                + pb.success_rate_weight
                + pb.recency_weight
                + pb.confidence_weight;
            if (weight_sum - 1.0).abs() > 0.01 {
                errors.push("pattern_bank scoring weights must sum to 1.0".into());
            }
        }

        // ── Focus tracker ──
        if self.focus.max_context_switches == 0 {
            errors.push("focus.max_context_switches must be greater than 0".into());
        }
        validate_unit_f32(self.focus.evidence_confidence_threshold, "focus.evidence_confidence_threshold", &mut errors);

        // ── Multi-agent ──
        let ma = &self.multi_agent;
        if ma.max_concurrent_agents == 0 {
            errors.push("multi_agent.max_concurrent_agents must be greater than 0".into());
        }
        if ma.communication_timeout_secs == 0 {
            errors.push("multi_agent.communication_timeout_secs must be greater than 0".into());
        }
        if ma.max_agent_load == 0 {
            errors.push("multi_agent.max_agent_load must be greater than 0".into());
        }
        if ma.convergent.max_fix_attempts_per_issue == 0 {
            errors.push("multi_agent.convergent.max_fix_attempts_per_issue must be greater than 0".into());
        }
        if ma.messaging.channel_capacity == 0 {
            errors.push("messaging.channel_capacity must be greater than 0".into());
        }

        // Consensus validation
        if ma.consensus.max_rounds == 0 {
            errors.push("consensus.max_rounds must be greater than 0".into());
        }
        validate_unit_f64(ma.consensus.convergence_threshold, "consensus.convergence_threshold", &mut errors);
        validate_unit_f64(ma.consensus.min_participation_ratio, "consensus.min_participation_ratio", &mut errors);
        validate_unit_f64(ma.consensus.max_timeout_ratio, "consensus.max_timeout_ratio", &mut errors);
        validate_unit_f64(ma.consensus.high_agreement_threshold, "consensus.high_agreement_threshold", &mut errors);
        validate_unit_f64(ma.consensus.escalation.conflict_threshold, "consensus.escalation.conflict_threshold", &mut errors);
        if ma.consensus.flat_threshold >= ma.consensus.hierarchical_threshold {
            errors.push("consensus.flat_threshold must be less than hierarchical_threshold".into());
        }

        // ── Task scoring ──
        let ts = &self.task_scoring;
        {
            let w = &ts.weights;
            let weight_sum = w.file + w.dependency + w.pattern + w.confidence + w.history;
            if (weight_sum - 1.0).abs() > 0.01 {
                errors.push("task_scoring.weights must sum to 1.0".into());
            }
        }
        if ts.max_file_threshold == 0 {
            errors.push("task_scoring.max_file_threshold must be greater than 0".into());
        }
        validate_unit_f32(ts.min_similarity_threshold, "task_scoring.min_similarity_threshold", &mut errors);

        // ── Task decomposition ──
        validate_unit_f32(self.task_decomposition.quality_threshold, "task_decomposition.quality_threshold", &mut errors);
        if self.task_decomposition.min_tasks_per_story > self.task_decomposition.max_tasks_per_story {
            errors.push("task_decomposition.min_tasks_per_story must be <= max_tasks_per_story".into());
        }

        // ── State config ──
        if self.state.snapshot_interval == 0 {
            errors.push("state.snapshot_interval must be greater than 0".into());
        }
        if self.state.read_pool_size == 0 {
            errors.push("state.read_pool_size must be greater than 0".into());
        }
        if self.state.operation_timeout_secs < 5 {
            errors.push("state.operation_timeout_secs must be at least 5".into());
        }
        if self.state.write_queue_capacity < 100 {
            errors.push("state.write_queue_capacity must be at least 100".into());
        }

        // ── Verification ──
        if self.verification.allowed_commands_exclusive
            && self.verification.additional_allowed_commands.is_empty()
        {
            errors.push(
                "verification.additional_allowed_commands must not be empty when \
                 allowed_commands_exclusive is true (no commands would be allowed)".into(),
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "Configuration validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

#[derive(Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub claude_dir: PathBuf,
    pub pilot_dir: PathBuf,
    pub missions_dir: PathBuf,
    pub wisdom_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl ProjectPaths {
    pub fn new(root: PathBuf, config: &PilotConfig) -> Self {
        let claude_dir = root.join(".claude");
        let pilot_dir = claude_dir.join("pilot");

        Self {
            worktrees_dir: root.join(&config.isolation.worktrees_dir),
            root,
            missions_dir: pilot_dir.join("missions"),
            wisdom_dir: pilot_dir.join("wisdom"),
            logs_dir: pilot_dir.join("logs"),
            claude_dir,
            pilot_dir,
        }
    }

    pub async fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            &self.pilot_dir,
            &self.missions_dir,
            &self.wisdom_dir,
            &self.logs_dir,
        ];

        for dir in dirs {
            fs::create_dir_all(dir).await?;
        }

        Ok(())
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.claude_dir.join("agents")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.claude_dir.join("skills")
    }

    pub fn rules_dir(&self) -> PathBuf {
        self.claude_dir.join("rules")
    }

    pub fn mission_log(&self, mission_id: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.log", mission_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Smoke: default config always validates ──

    #[test]
    fn default_config_validates() {
        PilotConfig::default().validate().unwrap();
    }

    // ── Non-negotiable convergent verification rules ──

    #[test]
    fn convergent_clean_rounds_below_2() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.required_clean_rounds = 1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("required_clean_rounds must be >= 2"));
    }

    #[test]
    fn convergent_ai_review_disabled() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.include_ai_review = false;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("include_ai_review must be true"));
    }

    // ── Range validations (0.0-1.0 boundaries) ──

    #[test]
    fn confidence_exactly_0_and_1_are_valid() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.persistent_issue_confidence = 0.0;
        config.multi_agent.convergent.pattern_confidence_threshold = 1.0;
        config.learning.min_confidence = 0.0;
        config.validate().unwrap();
    }

    #[test]
    fn confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.persistent_issue_confidence = 1.01;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("persistent_issue_confidence"));

        let mut config = PilotConfig::default();
        config.multi_agent.convergent.pattern_confidence_threshold = -0.01;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("pattern_confidence_threshold"));
    }

    #[test]
    fn recent_decisions_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.recovery.reasoning.recent_decisions_ratio = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("recent_decisions_ratio"));
    }

    #[test]
    fn compaction_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.compaction.compaction_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("compaction_threshold"));
    }

    // ── Weight sum validations ──

    #[test]
    fn pattern_bank_weights_not_sum_to_1() {
        let mut config = PilotConfig::default();
        config.pattern_bank.similarity_weight = 0.1;
        config.pattern_bank.success_rate_weight = 0.1;
        config.pattern_bank.recency_weight = 0.1;
        config.pattern_bank.confidence_weight = 0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("pattern_bank scoring weights must sum to 1.0"));
    }

    #[test]
    fn task_scoring_weights_not_sum_to_1() {
        let mut config = PilotConfig::default();
        config.task_scoring.weights.file = 0.5;
        config.task_scoring.weights.dependency = 0.5;
        config.task_scoring.weights.pattern = 0.5;
        config.task_scoring.weights.confidence = 0.5;
        config.task_scoring.weights.history = 0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("task_scoring.weights must sum to 1.0"));
    }

    #[test]
    fn evidence_budget_not_sum_to_100() {
        let mut config = PilotConfig::default();
        config.evidence.file_budget_percent = 10;
        config.evidence.pattern_budget_percent = 10;
        config.evidence.dependency_budget_percent = 10;
        config.evidence.knowledge_budget_percent = 10;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence budget percentages must sum to 100"));
    }

    // ── Cross-config dependency ──

    #[test]
    fn require_verifiable_evidence_without_persist() {
        let mut config = PilotConfig::default();
        config.quality.require_verifiable_evidence = true;
        config.recovery.checkpoint.persist_evidence = false;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("persist_evidence must be true"));
    }

    // ── Zero-value guards ──

    #[test]
    fn max_iterations_zero() {
        let mut config = PilotConfig::default();
        config.orchestrator.max_iterations = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_iterations"));
    }

    #[test]
    fn agent_timeout_zero() {
        let mut config = PilotConfig::default();
        config.agent.timeout_secs = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("agent timeout_secs"));
    }

    #[test]
    fn state_snapshot_interval_zero() {
        let mut config = PilotConfig::default();
        config.state.snapshot_interval = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("snapshot_interval"));
    }

    #[test]
    fn state_read_pool_size_zero() {
        let mut config = PilotConfig::default();
        config.state.read_pool_size = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("read_pool_size must be greater than 0"));
    }

    #[test]
    fn state_operation_timeout_too_low() {
        let mut config = PilotConfig::default();
        config.state.operation_timeout_secs = 3;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("operation_timeout_secs must be at least 5"));
    }

    #[test]
    fn state_write_queue_capacity_too_low() {
        let mut config = PilotConfig::default();
        config.state.write_queue_capacity = 50;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("write_queue_capacity must be at least 100"));
    }

    // ── Isolation threshold ordering ──

    #[test]
    fn small_threshold_not_less_than_large() {
        let mut config = PilotConfig::default();
        config.isolation.small_change_threshold = 100;
        config.isolation.large_change_threshold = 50;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("small_change_threshold must be less than large_change_threshold"));
    }

    // ── Verification command exclusivity ──

    #[test]
    fn exclusive_without_commands_fails() {
        let mut config = PilotConfig::default();
        config.verification.allowed_commands_exclusive = true;
        config.verification.additional_allowed_commands = vec![];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("additional_allowed_commands must not be empty"));
    }

    #[test]
    fn exclusive_with_commands_passes() {
        let mut config = PilotConfig::default();
        config.verification.allowed_commands_exclusive = true;
        config.verification.additional_allowed_commands = vec!["custom".into()];
        config.validate().unwrap();
    }

    // ── Multiple errors are aggregated ──

    #[test]
    fn multiple_validation_errors_aggregated() {
        let mut config = PilotConfig::default();
        config.orchestrator.max_iterations = 0;
        config.agent.timeout_secs = 0;
        config.state.snapshot_interval = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_iterations"));
        assert!(err.contains("agent timeout_secs"));
        assert!(err.contains("snapshot_interval"));
    }

    // ── Convergent verification cross-field ──

    #[test]
    fn convergent_max_rounds_below_clean_rounds() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.max_rounds = 1;
        config.multi_agent.convergent.required_clean_rounds = 2;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_rounds must be >= required_clean_rounds"));
    }

    // ── Zero-value guards (remaining) ──

    #[test]
    fn max_parallel_tasks_zero() {
        let mut config = PilotConfig::default();
        config.orchestrator.max_parallel_tasks = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_parallel_tasks"));
    }

    #[test]
    fn complexity_timeout_zero() {
        let mut config = PilotConfig::default();
        config.complexity.assessment_timeout_secs = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("assessment_timeout_secs"));
    }

    #[test]
    fn max_concurrent_agents_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.max_concurrent_agents = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_concurrent_agents"));
    }

    #[test]
    fn communication_timeout_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.communication_timeout_secs = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("communication_timeout_secs"));
    }

    #[test]
    fn max_agent_load_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.max_agent_load = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_agent_load"));
    }

    #[test]
    fn max_fix_attempts_per_issue_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.max_fix_attempts_per_issue = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_fix_attempts_per_issue"));
    }

    #[test]
    fn max_file_threshold_zero() {
        let mut config = PilotConfig::default();
        config.task_scoring.max_file_threshold = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_file_threshold"));
    }

    #[test]
    fn max_context_switches_zero() {
        let mut config = PilotConfig::default();
        config.focus.max_context_switches = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_context_switches"));
    }

    #[test]
    fn max_patterns_zero() {
        let mut config = PilotConfig::default();
        config.pattern_bank.max_patterns = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_patterns"));
    }

    // ── Range validations (remaining) ──

    #[test]
    fn estimator_safety_margin_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.estimator.safety_margin = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("safety_margin"));
    }

    #[test]
    fn estimator_selective_loading_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.estimator.selective_loading_ratio = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("selective_loading_ratio"));
    }

    #[test]
    fn learning_min_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.learning.min_confidence = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("learning.min_confidence"));
    }

    #[test]
    fn retry_history_retention_days_zero() {
        let mut config = PilotConfig::default();
        config.learning.retry_history.retention_days = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("retention_days must be positive"));
    }

    #[test]
    fn retry_history_min_success_rate_out_of_range() {
        let mut config = PilotConfig::default();
        config.learning.retry_history.min_success_rate = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_success_rate"));
    }

    #[test]
    fn pattern_bank_min_success_rate_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.min_success_rate = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("pattern_bank.min_success_rate"));
    }

    #[test]
    fn pattern_bank_min_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.min_confidence = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("pattern_bank.min_confidence"));
    }

    #[test]
    fn pattern_bank_merge_similarity_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.merge_similarity_threshold = 2.0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("merge_similarity_threshold"));
    }

    #[test]
    fn focus_evidence_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.focus.evidence_confidence_threshold = -0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence_confidence_threshold"));
    }

    // ── Boundary value: exact minimum is valid ──

    #[test]
    fn state_operation_timeout_exactly_5_is_valid() {
        let mut config = PilotConfig::default();
        config.state.operation_timeout_secs = 5;
        config.validate().unwrap();
    }

    #[test]
    fn state_write_queue_capacity_exactly_100_is_valid() {
        let mut config = PilotConfig::default();
        config.state.write_queue_capacity = 100;
        config.validate().unwrap();
    }

    // ── Recovery sub-config delegation ──

    #[test]
    fn checkpoint_interval_tasks_zero() {
        let mut config = PilotConfig::default();
        config.recovery.checkpoint.interval_tasks = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("interval_tasks must be greater than 0"));
    }

    #[test]
    fn checkpoint_max_checkpoints_zero() {
        let mut config = PilotConfig::default();
        config.recovery.checkpoint.max_checkpoints = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_checkpoints must be greater than 0"));
    }

    #[test]
    fn reasoning_retention_days_zero_when_enabled() {
        let mut config = PilotConfig::default();
        config.recovery.reasoning.enabled = true;
        config.recovery.reasoning.retention_days = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("retention_days must be positive"));
    }

    #[test]
    fn reasoning_max_hypotheses_zero() {
        let mut config = PilotConfig::default();
        config.recovery.reasoning.max_hypotheses_in_context = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_hypotheses_in_context"));
    }

    #[test]
    fn reasoning_max_decisions_zero() {
        let mut config = PilotConfig::default();
        config.recovery.reasoning.max_decisions_in_context = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_decisions_in_context"));
    }

    #[test]
    fn convergent_fix_timeout_too_low() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.fix_timeout_secs = 10;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("fix_timeout_secs must be at least 30"));
    }

    #[test]
    fn convergent_total_timeout_below_fix_timeout() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.total_timeout_secs = 100;
        config.multi_agent.convergent.fix_timeout_secs = 200;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("total_timeout_secs must be >= fix_timeout_secs"));
    }

    #[test]
    fn agent_model_empty() {
        let mut config = PilotConfig::default();
        config.agent.model = String::new();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("agent model must not be empty"));
    }

    // ── QualityConfig delegate validation ──

    #[test]
    fn quality_min_evidence_quality_below_non_negotiable() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_quality = 0.4;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_evidence_quality must be >= 0.5"));
    }

    #[test]
    fn quality_min_evidence_quality_out_of_range() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_quality = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_evidence_quality must be between 0.0 and 1.0"));
    }

    #[test]
    fn quality_min_evidence_quality_exactly_0_5_is_valid() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_quality = 0.5;
        config.validate().unwrap();
    }

    #[test]
    fn quality_min_evidence_confidence_below_non_negotiable() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_confidence = 0.2;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_evidence_confidence must be >= 0.3"));
    }

    #[test]
    fn quality_min_evidence_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_confidence = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_evidence_confidence must be between 0.0 and 1.0"));
    }

    #[test]
    fn quality_require_verifiable_evidence_false() {
        let mut config = PilotConfig::default();
        config.quality.require_verifiable_evidence = false;
        config.recovery.checkpoint.persist_evidence = false;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("require_verifiable_evidence must be true"));
    }

    #[test]
    fn quality_evidence_coverage_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.quality.evidence_coverage_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence_coverage_threshold must be between 0.0 and 1.0"));
    }

    // ── SelectionWeightsConfig delegate validation ──

    #[test]
    fn selection_weights_sum_not_1_0() {
        let mut config = PilotConfig::default();
        config.multi_agent.selection_weights.capability = 0.1;
        config.multi_agent.selection_weights.load = 0.1;
        config.multi_agent.selection_weights.performance = 0.1;
        config.multi_agent.selection_weights.health = 0.1;
        config.multi_agent.selection_weights.availability = 0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("selection weights must sum to 1.0"));
    }

    #[test]
    fn selection_weights_individual_out_of_range() {
        let mut config = PilotConfig::default();
        // Sum must be ~1.0 to pass sum check, but one value negative to trigger range check
        config.multi_agent.selection_weights.capability = -0.1;
        config.multi_agent.selection_weights.load = 0.40;
        config.multi_agent.selection_weights.availability = 0.30;
        // Sum: -0.1 + 0.40 + 0.25 + 0.15 + 0.30 = 1.00
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("capability"), "err: {err}");
    }

    // ── Convergent verification boundary values ──

    #[test]
    fn convergent_max_rounds_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.max_rounds = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_rounds must be greater than 0"));
    }

    #[test]
    fn convergent_fix_timeout_exactly_30_is_valid() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.fix_timeout_secs = 30;
        config.validate().unwrap();
    }

    // ── Boundary equality cases ──

    #[test]
    fn convergent_max_rounds_equals_clean_rounds() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.max_rounds = 2;
        config.multi_agent.convergent.required_clean_rounds = 2;
        config.validate().unwrap();
    }

    #[test]
    fn convergent_total_timeout_equals_fix_timeout() {
        let mut config = PilotConfig::default();
        config.multi_agent.convergent.total_timeout_secs = 300;
        config.multi_agent.convergent.fix_timeout_secs = 300;
        config.validate().unwrap();
    }

    #[test]
    fn quality_min_evidence_confidence_exactly_0_3_is_valid() {
        let mut config = PilotConfig::default();
        config.quality.min_evidence_confidence = 0.3;
        config.validate().unwrap();
    }

    // ── New QualityConfig validations ──

    #[test]
    fn quality_risky_coverage_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.quality.risky_coverage_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("risky_coverage_threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn quality_risky_coverage_not_less_than_evidence() {
        let mut config = PilotConfig::default();
        config.quality.risky_coverage_threshold = 0.5;
        config.quality.evidence_coverage_threshold = 0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("risky_coverage_threshold must be less than evidence_coverage_threshold"));
    }

    #[test]
    fn quality_weights_not_sum_to_1() {
        let mut config = PilotConfig::default();
        config.quality.verifiable_weight = 0.3;
        config.quality.confidence_weight = 0.3;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("verifiable_weight + confidence_weight must sum to 1.0"));
    }

    // ── CoherenceThresholds delegate validation ──

    #[test]
    fn coherence_thresholds_out_of_range() {
        let mut config = PilotConfig::default();
        config.coherence.thresholds.contract_consistency = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("contract_consistency"));
    }

    #[test]
    fn coherence_thresholds_new_fields_validated() {
        let mut config = PilotConfig::default();
        config.coherence.thresholds.word_overlap_fallback = -0.1;
        config.coherence.thresholds.critical_severity_weight = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("word_overlap_fallback"), "err: {err}");
        assert!(err.contains("critical_severity_weight"), "err: {err}");
    }

    // ── ConsensusConfig validation ──

    #[test]
    fn consensus_max_rounds_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.max_rounds = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("consensus.max_rounds must be greater than 0"));
    }

    #[test]
    fn consensus_convergence_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.convergence_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("convergence_threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn consensus_min_participation_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.min_participation_ratio = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_participation_ratio must be between 0.0 and 1.0"));
    }

    #[test]
    fn consensus_max_timeout_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.max_timeout_ratio = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_timeout_ratio must be between 0.0 and 1.0"));
    }

    #[test]
    fn consensus_high_agreement_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.high_agreement_threshold = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("high_agreement_threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn consensus_escalation_conflict_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.escalation.conflict_threshold = 2.0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("conflict_threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn consensus_flat_not_less_than_hierarchical() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.flat_threshold = 20;
        config.multi_agent.consensus.hierarchical_threshold = 10;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("flat_threshold must be less than hierarchical_threshold"));
    }

    // ── CompactionConfig ratio validation ──

    #[test]
    fn compaction_aggressive_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.compaction.aggressive_ratio = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("aggressive_ratio must be between 0.0 and 1.0"));
    }

    #[test]
    fn compaction_dedup_similarity_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.compaction.dedup_similarity_threshold = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("dedup_similarity_threshold must be between 0.0 and 1.0"));
    }

    #[test]
    fn compaction_aggressive_ratio_boundary_values() {
        let mut config = PilotConfig::default();
        config.context.compaction.aggressive_ratio = 0.0;
        config.validate().unwrap();
        config.context.compaction.aggressive_ratio = 1.0;
        config.validate().unwrap();
    }

    // ── MessagingConfig validation ──

    #[test]
    fn messaging_channel_capacity_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.messaging.channel_capacity = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("channel_capacity must be greater than 0"));
    }

    // ── BudgetAllocationConfig divisor validation ──

    #[test]
    fn budget_allocation_recent_tasks_divisor_zero() {
        let mut config = PilotConfig::default();
        config.context.budget_allocation.recent_tasks_available_divisor = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("recent_tasks_available_divisor must be greater than 0"));
    }

    #[test]
    fn budget_allocation_rebalance_divisor_zero() {
        let mut config = PilotConfig::default();
        config.context.budget_allocation.rebalance_reduction_divisor = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("rebalance_reduction_divisor must be greater than 0"));
    }

    // ── ErrorRetryPolicy jitter_ratio validation ──

    #[test]
    fn retry_policy_jitter_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.recovery.execution_error.timeout.jitter_ratio = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("timeout.jitter_ratio must be between 0.0 and 1.0"));
    }

    #[test]
    fn retry_policy_jitter_ratio_negative() {
        let mut config = PilotConfig::default();
        config.recovery.execution_error.network_error.jitter_ratio = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("network_error.jitter_ratio must be between 0.0 and 1.0"));
    }

    // ── Recovery aggregation (multiple errors reported at once) ──

    #[test]
    fn recovery_aggregates_multiple_errors() {
        let mut config = PilotConfig::default();
        config.recovery.checkpoint.interval_tasks = 0;
        config.recovery.checkpoint.max_checkpoints = 0;
        config.recovery.reasoning.max_hypotheses_in_context = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("interval_tasks"), "err: {err}");
        assert!(err.contains("max_checkpoints"), "err: {err}");
        assert!(err.contains("max_hypotheses_in_context"), "err: {err}");
    }

    // ── ChunkedPlanningConfig divisor validation (CRITICAL: panic prevention) ──

    #[test]
    fn chunked_planning_max_tokens_divisor_zero() {
        let mut config = PilotConfig::default();
        config.chunked_planning.max_tokens_divisor = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("max_tokens_divisor must be greater than 0"));
    }

    #[test]
    fn chunked_planning_complexity_divisor_zero() {
        let mut config = PilotConfig::default();
        config.chunked_planning.complexity_divisor = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("complexity_divisor must be greater than 0"));
    }

    #[test]
    fn chunked_planning_min_phases_zero() {
        let mut config = PilotConfig::default();
        config.chunked_planning.min_phases = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_phases must be greater than 0"));
    }

    #[test]
    fn chunked_planning_min_phases_exceeds_max() {
        let mut config = PilotConfig::default();
        config.chunked_planning.min_phases = 10;
        config.chunked_planning.max_phases = 5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_phases must be <= max_phases"));
    }

    // ── ConsensusScoringConfig validation ──

    #[test]
    fn consensus_scoring_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.approve_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("scoring.approve_threshold"));
    }

    #[test]
    fn consensus_scoring_cross_field_approve_threshold() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.approve_with_changes_threshold = 0.9;
        config.multi_agent.consensus.scoring.approve_threshold = 0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("approve_with_changes_threshold must be <= approve_threshold"));
    }

    #[test]
    fn consensus_scoring_cross_field_convergence() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.partial_convergence_threshold = 0.9;
        config.multi_agent.consensus.scoring.convergence_threshold = 0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("partial_convergence_threshold must be <= convergence_threshold"));
    }

    #[test]
    fn consensus_scoring_cross_field_evidence() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.min_evidence_score = 0.8;
        config.multi_agent.consensus.scoring.base_evidence_score = 0.3;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_evidence_score must be <= base_evidence_score"));
    }

    #[test]
    fn consensus_scoring_history_prune_below_max() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.max_agent_history_entries = 1000;
        config.multi_agent.consensus.scoring.history_prune_threshold = 500;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("history_prune_threshold must be >= max_agent_history_entries"));
    }

    #[test]
    fn consensus_scoring_timeout_zero() {
        let mut config = PilotConfig::default();
        config.multi_agent.consensus.scoring.consensus_timeout_secs = 0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("scoring.consensus_timeout_secs must be greater than 0"));
    }

    // ── BudgetAllocationConfig ratio validation ──

    #[test]
    fn budget_allocation_ratio_out_of_range() {
        let mut config = PilotConfig::default();
        config.context.budget_allocation.mission_summary_ratio = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("budget_allocation.mission_summary_ratio"));
    }

    #[test]
    fn budget_allocation_sum_exceeds_1() {
        let mut config = PilotConfig::default();
        config.context.budget_allocation.mission_summary_ratio = 0.5;
        config.context.budget_allocation.current_phase_ratio = 0.5;
        config.context.budget_allocation.current_task_ratio = 0.5;
        // Sum = 0.5+0.5+0.5+0.10+0.05+0.15+0.20 = 2.0 (> 1.0)
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("budget_allocation ratios must sum to at most 1.0"));
    }

    #[test]
    fn budget_allocation_default_sum_is_valid() {
        // Default sum is 0.775 which is < 1.0
        let config = PilotConfig::default();
        let ba = &config.context.budget_allocation;
        let sum = ba.mission_summary_ratio
            + ba.current_phase_ratio
            + ba.current_task_ratio
            + ba.recent_tasks_ratio
            + ba.learnings_ratio
            + ba.evidence_ratio
            + ba.reserved_output_ratio;
        assert!(sum <= 1.0, "Default ratio sum {} exceeds 1.0", sum);
        config.validate().unwrap();
    }

    // ── EvidenceBudgetConfig confidence validation ──

    #[test]
    fn evidence_min_confidence_display_out_of_range() {
        let mut config = PilotConfig::default();
        config.evidence.min_confidence_display = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence.min_confidence_display"));
    }

    #[test]
    fn evidence_skill_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.evidence.skill_confidence = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence.skill_confidence"));
    }

    #[test]
    fn evidence_rule_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.evidence.rule_confidence = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("evidence.rule_confidence"));
    }

    // ── PatternBankConfig remaining field validation ──

    #[test]
    fn pattern_bank_trajectory_penalty_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.trajectory_attempt_penalty = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("trajectory_attempt_penalty"));
    }

    #[test]
    fn pattern_bank_initial_confidence_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.initial_success_confidence = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("initial_success_confidence"));
    }

    #[test]
    fn pattern_bank_recency_decay_rate_out_of_range() {
        let mut config = PilotConfig::default();
        config.pattern_bank.recency_decay_rate = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("recency_decay_rate"));
    }

    // ── TaskDecompositionConfig validation ──

    #[test]
    fn task_decomposition_quality_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.task_decomposition.quality_threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("task_decomposition.quality_threshold"));
    }

    #[test]
    fn task_decomposition_min_exceeds_max() {
        let mut config = PilotConfig::default();
        config.task_decomposition.min_tasks_per_story = 10;
        config.task_decomposition.max_tasks_per_story = 5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("min_tasks_per_story must be <= max_tasks_per_story"));
    }

    // ── TaskScoringConfig validation ──

    #[test]
    fn task_scoring_min_similarity_out_of_range() {
        let mut config = PilotConfig::default();
        config.task_scoring.min_similarity_threshold = -0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("task_scoring.min_similarity_threshold"));
    }

    // ── PhaseComplexityConfig multiplier validation ──

    #[test]
    fn phase_complexity_multiplier_zero() {
        let mut config = PilotConfig::default();
        config.context.phase_complexity.simple_multiplier = 0.0;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("simple_multiplier must be greater than 0.0"));
    }

    #[test]
    fn phase_complexity_multiplier_negative() {
        let mut config = PilotConfig::default();
        config.context.phase_complexity.critical_multiplier = -0.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("critical_multiplier must be greater than 0.0"));
    }

    #[test]
    fn phase_complexity_multiplier_above_1_is_valid() {
        let mut config = PilotConfig::default();
        config.context.phase_complexity.critical_multiplier = 2.5;
        config.validate().unwrap();
    }

    // ── CompactionStrategy validation (recovery) ──

    #[test]
    fn compaction_strategy_threshold_out_of_range() {
        let mut config = PilotConfig::default();
        config.recovery.compaction_levels.light.threshold = 1.5;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("compaction_levels.light.threshold"));
    }

    #[test]
    fn compaction_strategy_aggressive_out_of_range() {
        let mut config = PilotConfig::default();
        config.recovery.compaction_levels.emergency.aggressive = -0.1;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("compaction_levels.emergency.aggressive"));
    }
}
