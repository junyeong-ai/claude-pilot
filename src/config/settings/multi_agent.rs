use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::recovery::ConvergentVerificationConfig;
use super::state::CompactionConfig;
use crate::domain::Quorum;

/// Configuration for Multi-Agent Architecture (Phase 5).
/// Specialized agents with parallel execution and coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiAgentConfig {
    pub enabled: bool,
    pub dynamic_mode: bool,
    pub selection_policy: SelectionPolicy,
    pub parallel_execution: bool,
    pub instances: HashMap<String, usize>,
    pub coordinator_enabled: bool,
    pub max_concurrent_agents: usize,
    pub communication_timeout_secs: u64,
    pub consensus: ConsensusConfig,
    pub messaging: MessagingConfig,
    pub research: ResearchConfig,
    pub discovery: DiscoveryConfig,
    /// Maximum load per agent for scoring calculations.
    pub max_agent_load: u32,
    pub selection_weights: SelectionWeightsConfig,
    /// Convergent verification settings (reuses ConvergentVerificationConfig from recovery).
    pub convergent: ConvergentVerificationConfig,
    /// Context compaction settings for long-running missions.
    pub context_compaction: CompactionConfig,
    /// Maximum number of deferred task retry rounds.
    /// Default: 3.
    pub max_deferred_retries: usize,
    /// Delay in milliseconds between deferred task retry attempts.
    /// Default: 500.
    pub deferred_retry_delay_ms: u64,
}

impl Default for MultiAgentConfig {
    fn default() -> Self {
        let mut instances = HashMap::new();
        instances.insert("research".to_string(), 1);
        instances.insert("planning".to_string(), 1);
        instances.insert("coder".to_string(), 2);
        instances.insert("verifier".to_string(), 1);

        Self {
            enabled: false,
            dynamic_mode: false,
            selection_policy: SelectionPolicy::RoundRobin,
            parallel_execution: true,
            instances,
            coordinator_enabled: true,
            max_concurrent_agents: 4,
            communication_timeout_secs: 30,
            consensus: ConsensusConfig::default(),
            messaging: MessagingConfig::default(),
            research: ResearchConfig::default(),
            discovery: DiscoveryConfig::default(),
            max_agent_load: 10,
            selection_weights: SelectionWeightsConfig::default(),
            convergent: ConvergentVerificationConfig::default(),
            context_compaction: CompactionConfig::default(),
            max_deferred_retries: 3,
            deferred_retry_delay_ms: 500,
        }
    }
}

impl MultiAgentConfig {
    pub fn instance_count(&self, role_id: &str) -> usize {
        self.instances.get(role_id).copied().unwrap_or(1)
    }
}

/// Per-tier quorum configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierQuorumConfig {
    pub module: Quorum,
    pub group: Quorum,
    pub domain: Quorum,
    pub workspace: Quorum,
    pub cross_workspace: Quorum,
}

impl Default for TierQuorumConfig {
    fn default() -> Self {
        Self {
            module: Quorum::Supermajority,
            group: Quorum::Majority,
            domain: Quorum::Majority,
            workspace: Quorum::UnanimousWithFallback,
            cross_workspace: Quorum::UnanimousWithFallback,
        }
    }
}

/// Per-tier cross-visibility configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierCrossVisibilityConfig {
    /// Enable cross-visibility for Module tier (default: true)
    pub module: bool,
    /// Enable cross-visibility for Group tier (default: false)
    pub group: bool,
    /// Enable cross-visibility for Domain tier (default: false)
    pub domain: bool,
    /// Enable cross-visibility for Workspace tier (default: true)
    pub workspace: bool,
    /// Enable cross-visibility for CrossWorkspace tier (default: true)
    pub cross_workspace: bool,
}

impl Default for TierCrossVisibilityConfig {
    fn default() -> Self {
        Self {
            module: true,
            group: false,
            domain: false,
            workspace: true,
            cross_workspace: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsensusConfig {
    pub max_rounds: usize,
    pub min_participants: usize,
    pub require_architecture: bool,

    // Adaptive strategy thresholds
    /// Maximum participants for flat consensus (<= flat_threshold -> flat)
    pub flat_threshold: usize,
    /// Minimum participants for hierarchical consensus (> hierarchical_threshold -> hierarchical)
    pub hierarchical_threshold: usize,

    // Hierarchical execution limits
    /// Maximum participants per tier level
    pub max_participants_per_tier: usize,
    /// Maximum concurrent consensus groups
    pub max_concurrent_groups: usize,

    // Parallel execution
    /// Maximum concurrent LLM calls per consensus round
    pub max_concurrent_llm_calls: usize,
    /// Timeout per agent LLM call in seconds
    pub per_agent_timeout_secs: u64,
    /// Timeout for synthesis step in seconds
    pub synthesis_timeout_secs: u64,
    /// Extra buffer time added to calculated timeout
    pub timeout_buffer_secs: u64,

    // Timeouts (in seconds)
    /// Timeout for each round
    pub round_timeout_secs: u64,
    /// Timeout for each tier consensus
    pub tier_timeout_secs: u64,
    /// Total timeout for entire consensus process
    pub total_timeout_secs: u64,

    // Cross-visibility
    /// Enable cross-visibility mode where agents see peer proposals
    pub enable_cross_visibility: bool,
    /// Number of cross-visibility rounds before finalizing
    pub cross_visibility_rounds: usize,

    // P2P Conflict Resolution
    /// Enable peer-to-peer conflict resolution via MessageBus
    pub enable_p2p_conflict_resolution: bool,
    /// Timeout for P2P conflict resolution in seconds
    pub conflict_resolution_timeout_secs: u64,

    // Convergence
    /// Threshold for considering consensus converged
    pub convergence_threshold: f64,
    /// Minimum ratio of participants that must participate
    pub min_participation_ratio: f64,

    // Checkpointing
    /// Create checkpoint every N rounds
    pub checkpoint_interval: usize,
    /// Also checkpoint on tier completion
    pub checkpoint_on_tier_complete: bool,
    /// Maximum checkpoints to retain
    pub max_checkpoints: usize,

    // Recovery
    /// Enable automatic recovery from checkpoints
    pub auto_recovery: bool,
    /// Maximum recovery attempts
    pub max_recovery_attempts: usize,

    // Tier-specific settings
    /// Per-tier quorum requirements
    pub tier_quorum: TierQuorumConfig,
    /// Per-tier cross-visibility settings
    pub tier_cross_visibility: TierCrossVisibilityConfig,

    // Escalation
    pub escalation: ConsensusEscalationConfig,

    // Scoring
    pub scoring: ConsensusScoringConfig,

    // Adaptive consensus
    pub default_tier_timeout_secs: u64,
    pub max_timeout_ratio: f64,

    // Convergence thresholds (semantic convergence checking)
    pub high_agreement_threshold: f64,

    /// Maximum lines of peer context to include per unit in cross-visibility mode.
    pub peer_context_max_lines: usize,

    /// Maximum backtracking attempts in hierarchical consensus.
    pub max_backtracks: usize,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            max_rounds: 3,
            min_participants: 2,
            require_architecture: true,

            // Adaptive strategy
            flat_threshold: 5,
            hierarchical_threshold: 15,

            // Hierarchical limits
            max_participants_per_tier: 10,
            max_concurrent_groups: 5,

            // Parallel execution
            max_concurrent_llm_calls: 5,
            per_agent_timeout_secs: 90,
            synthesis_timeout_secs: 60,
            timeout_buffer_secs: 120,

            // Timeouts
            round_timeout_secs: 120,
            tier_timeout_secs: 300,
            total_timeout_secs: 1800,

            // Cross-visibility
            enable_cross_visibility: true,
            cross_visibility_rounds: 2,

            // P2P Conflict Resolution
            enable_p2p_conflict_resolution: true,
            conflict_resolution_timeout_secs: 30,

            // Convergence
            convergence_threshold: 0.8,
            min_participation_ratio: 0.7,

            // Checkpointing
            checkpoint_interval: 2,
            checkpoint_on_tier_complete: true,
            max_checkpoints: 10,

            // Recovery
            auto_recovery: true,
            max_recovery_attempts: 3,

            // Tier-specific settings
            tier_quorum: TierQuorumConfig::default(),
            tier_cross_visibility: TierCrossVisibilityConfig::default(),

            // Escalation
            escalation: ConsensusEscalationConfig::default(),

            // Scoring
            scoring: ConsensusScoringConfig::default(),

            // Adaptive consensus
            default_tier_timeout_secs: 300,
            max_timeout_ratio: 0.5,

            // Convergence thresholds
            high_agreement_threshold: 0.9,

            peer_context_max_lines: 5,

            max_backtracks: 3,
        }
    }
}

impl ConsensusConfig {
    /// Calculate dynamic timeout based on participant count and config settings.
    ///
    /// Formula: (batches x per_agent_timeout + synthesis_timeout) x rounds + buffer
    /// where batches = ceil(participants / max_concurrent_llm_calls)
    pub fn calculate_dynamic_timeout(&self, participant_count: usize) -> std::time::Duration {
        let concurrent = self.max_concurrent_llm_calls.max(1);
        let batches = participant_count.div_ceil(concurrent);

        let per_round =
            (batches as u64) * self.per_agent_timeout_secs + self.synthesis_timeout_secs;
        let total = per_round * (self.max_rounds as u64) + self.timeout_buffer_secs;
        let capped = total.min(self.total_timeout_secs);

        std::time::Duration::from_secs(capped)
    }

    /// Calculate per-tier timeout based on unit participant count.
    pub fn calculate_tier_timeout(&self, max_unit_participants: usize) -> std::time::Duration {
        let concurrent = self.max_concurrent_llm_calls.max(1);
        let batches = max_unit_participants.div_ceil(concurrent);

        let per_round =
            (batches as u64) * self.per_agent_timeout_secs + self.synthesis_timeout_secs;
        let with_buffer = per_round * (self.max_rounds as u64) + (self.timeout_buffer_secs / 2);

        std::time::Duration::from_secs(with_buffer.min(self.tier_timeout_secs))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsensusEscalationConfig {
    /// Number of stalled rounds before escalating
    pub max_stalled_rounds: usize,
    /// Ratio of conflicting proposals that triggers escalation
    pub conflict_threshold: f64,
    /// Escalation level configurations
    pub retry_max_attempts: usize,
    pub scope_reduction_max_attempts: usize,
    pub mediation_max_attempts: usize,
    pub split_max_attempts: usize,
}

impl Default for ConsensusEscalationConfig {
    fn default() -> Self {
        Self {
            max_stalled_rounds: 3,
            conflict_threshold: 0.4,
            retry_max_attempts: 3,
            scope_reduction_max_attempts: 2,
            mediation_max_attempts: 1,
            split_max_attempts: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    pub cache_enabled: bool,
    pub cache_path: String,
    pub cache_ttl_secs: u64,
    pub exclude_dirs: Vec<String>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            cache_enabled: true,
            cache_path: ".claude-pilot/cache/discovery.json".to_string(),
            cache_ttl_secs: 3600,
            exclude_dirs: super::DEFAULT_EXCLUDE_DIRS.iter().map(|s| (*s).into()).collect(),
        }
    }
}

/// Agent selection policy for load balancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    #[default]
    RoundRobin,
    LeastLoaded,
    Random,
    /// Weighted scoring: capability * 0.30 + load * 0.20 + performance * 0.25 + health * 0.15 + availability * 0.10
    Scored,
}

/// Scoring parameters for consensus evidence evaluation and agent history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsensusScoringConfig {
    pub default_accuracy: f64,
    pub approve_threshold: f64,
    pub approve_with_changes_threshold: f64,
    pub module_match_score: f64,
    pub module_partial_score: f64,
    pub core_role_score: f64,
    pub default_role_score: f64,
    pub convergence_threshold: f64,
    pub partial_convergence_threshold: f64,
    pub base_evidence_score: f64,
    pub min_evidence_score: f64,
    pub file_reference_bonus: f64,
    pub code_snippet_bonus: f64,
    pub line_reference_bonus: f64,
    pub error_code_bonus: f64,
    pub identifier_pattern_bonus: f64,
    pub topic_match_bonus: f64,
    pub implementation_detail_bonus: f64,
    pub consistency_bonus: f64,
    pub reference_bonus: f64,
    pub max_agent_history_entries: usize,
    pub history_prune_threshold: usize,
    pub consensus_timeout_secs: u64,
}

impl Default for ConsensusScoringConfig {
    fn default() -> Self {
        Self {
            default_accuracy: 0.5,
            approve_threshold: 0.7,
            approve_with_changes_threshold: 0.4,
            module_match_score: 0.9,
            module_partial_score: 0.6,
            core_role_score: 0.7,
            default_role_score: 0.5,
            convergence_threshold: 0.6,
            partial_convergence_threshold: 0.5,
            base_evidence_score: 0.5,
            min_evidence_score: 0.1,
            file_reference_bonus: 0.1,
            code_snippet_bonus: 0.15,
            line_reference_bonus: 0.1,
            error_code_bonus: 0.1,
            identifier_pattern_bonus: 0.05,
            topic_match_bonus: 0.15,
            implementation_detail_bonus: 0.1,
            consistency_bonus: 0.15,
            reference_bonus: 0.05,
            max_agent_history_entries: 1000,
            history_prune_threshold: 1200,
            consensus_timeout_secs: 300,
        }
    }
}

impl ConsensusScoringConfig {
    pub fn validate(&self) -> crate::error::Result<()> {
        let mut errors = Vec::new();

        // Score and threshold fields (0.0-1.0)
        for (name, value) in [
            ("scoring.default_accuracy", self.default_accuracy),
            ("scoring.approve_threshold", self.approve_threshold),
            (
                "scoring.approve_with_changes_threshold",
                self.approve_with_changes_threshold,
            ),
            ("scoring.module_match_score", self.module_match_score),
            ("scoring.module_partial_score", self.module_partial_score),
            ("scoring.core_role_score", self.core_role_score),
            ("scoring.default_role_score", self.default_role_score),
            ("scoring.convergence_threshold", self.convergence_threshold),
            (
                "scoring.partial_convergence_threshold",
                self.partial_convergence_threshold,
            ),
            ("scoring.base_evidence_score", self.base_evidence_score),
            ("scoring.min_evidence_score", self.min_evidence_score),
            ("scoring.file_reference_bonus", self.file_reference_bonus),
            ("scoring.code_snippet_bonus", self.code_snippet_bonus),
            ("scoring.line_reference_bonus", self.line_reference_bonus),
            ("scoring.error_code_bonus", self.error_code_bonus),
            (
                "scoring.identifier_pattern_bonus",
                self.identifier_pattern_bonus,
            ),
            ("scoring.topic_match_bonus", self.topic_match_bonus),
            (
                "scoring.implementation_detail_bonus",
                self.implementation_detail_bonus,
            ),
            ("scoring.consistency_bonus", self.consistency_bonus),
            ("scoring.reference_bonus", self.reference_bonus),
        ] {
            super::validate_unit_f64(value, name, &mut errors);
        }

        // Cross-field consistency
        if self.approve_with_changes_threshold > self.approve_threshold {
            errors.push(
                "scoring.approve_with_changes_threshold must be <= approve_threshold".into(),
            );
        }
        if self.partial_convergence_threshold > self.convergence_threshold {
            errors.push(
                "scoring.partial_convergence_threshold must be <= convergence_threshold".into(),
            );
        }
        if self.min_evidence_score > self.base_evidence_score {
            errors.push(
                "scoring.min_evidence_score must be <= base_evidence_score".into(),
            );
        }

        // Positive integer fields
        if self.max_agent_history_entries == 0 {
            errors.push("scoring.max_agent_history_entries must be greater than 0".into());
        }
        if self.consensus_timeout_secs == 0 {
            errors.push("scoring.consensus_timeout_secs must be greater than 0".into());
        }
        if self.history_prune_threshold < self.max_agent_history_entries {
            errors.push(
                "scoring.history_prune_threshold must be >= max_agent_history_entries".into(),
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::error::PilotError::Config(format!(
                "ConsensusScoringConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

/// Messaging bus and persistent message store configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MessagingConfig {
    pub channel_capacity: usize,
    pub message_ttl_secs: u64,
    pub cleanup_write_interval: u64,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 256,
            message_ttl_secs: 3600,
            cleanup_write_interval: 100,
        }
    }
}

/// Research agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResearchConfig {
    pub max_findings: usize,
    pub timeout_secs: u64,
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self {
            max_findings: 20,
            timeout_secs: 300,
        }
    }
}

/// Agent selection scoring weights (must sum to 1.0).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SelectionWeightsConfig {
    pub capability: f64,
    pub load: f64,
    pub performance: f64,
    pub health: f64,
    pub availability: f64,
    /// Latency threshold below which agents get full health score (ms).
    pub health_good_latency_ms: u64,
    /// Latency threshold below which agents get degraded health score (ms).
    pub health_degraded_latency_ms: u64,
    /// Default performance score for agents with no execution history.
    pub default_new_agent_score: f64,
}

impl Default for SelectionWeightsConfig {
    fn default() -> Self {
        Self {
            capability: 0.30,
            load: 0.20,
            performance: 0.25,
            health: 0.15,
            availability: 0.10,
            health_good_latency_ms: 30_000,
            health_degraded_latency_ms: 60_000,
            default_new_agent_score: 0.8,
        }
    }
}

impl SelectionWeightsConfig {
    pub fn validate(&self) -> crate::error::Result<()> {
        let mut errors = Vec::new();

        let sum = self.capability + self.load + self.performance + self.health + self.availability;
        if (sum - 1.0).abs() > 0.001 {
            errors.push(format!(
                "selection weights must sum to 1.0, got {:.3}",
                sum
            ));
        }
        for (name, value) in [
            ("capability", self.capability),
            ("load", self.load),
            ("performance", self.performance),
            ("health", self.health),
            ("availability", self.availability),
        ] {
            if !(0.0..=1.0).contains(&value) {
                errors.push(format!(
                    "selection weight '{}' must be between 0.0 and 1.0, got {}",
                    name, value
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(crate::error::PilotError::Config(format!(
                "SelectionWeightsConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}
