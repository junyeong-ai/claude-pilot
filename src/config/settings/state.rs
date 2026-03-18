use serde::{Deserialize, Serialize};

use crate::config::model::ModelConfig;

/// Configuration for event-sourced state management.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    /// Enable event store for event sourcing.
    pub event_store_enabled: bool,
    /// Interval for creating snapshots (number of events).
    pub snapshot_interval: u32,
    /// Maximum snapshots retained per aggregate.
    pub max_snapshots_per_aggregate: usize,
    /// Maximum tracked tasks in TaskStatusProjection.
    pub max_tracked_tasks: usize,
    /// Maximum proposal history entries in ConsensusProjection.
    pub max_proposal_history: usize,
    /// Maximum active conflicts tracked in ConsensusProjection.
    pub max_active_conflicts: usize,
    /// Read pool size for concurrent event store queries.
    pub read_pool_size: usize,
    /// Default query limit for global event queries.
    pub default_query_limit: usize,
    /// Archive events to a separate table after snapshot creation.
    pub archive_after_snapshot: bool,
    /// Run VACUUM after this many events have been archived.
    pub vacuum_interval_events: u64,
    /// SQLite synchronous mode ("NORMAL", "FULL", "OFF").
    pub synchronous_mode: String,
    /// Timeout for individual event store operations in seconds.
    pub operation_timeout_secs: u64,
    /// Capacity of the writer thread's command queue.
    pub write_queue_capacity: usize,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            event_store_enabled: true,
            snapshot_interval: 100,
            max_snapshots_per_aggregate: 3,
            max_tracked_tasks: 1000,
            max_proposal_history: 20,
            max_active_conflicts: 50,
            read_pool_size: 4,
            default_query_limit: 10_000,
            archive_after_snapshot: false,
            vacuum_interval_events: 10_000,
            synchronous_mode: "NORMAL".into(),
            operation_timeout_secs: 30,
            write_queue_capacity: 10_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    pub max_tokens_override: Option<usize>,
    pub enable_auto_compaction: bool,
    pub limits: ContextLimitsConfig,
    pub compaction: CompactionConfig,
    pub task_budget: TaskBudgetConfig,
    pub budget_allocation: BudgetAllocationConfig,
    pub phase_complexity: PhaseComplexityConfig,
    pub estimator: ContextEstimatorConfig,
}

impl ContextConfig {
    pub fn effective_max_tokens(&self, model_config: &ModelConfig) -> usize {
        self.max_tokens_override
            .unwrap_or_else(|| model_config.usable_context() as usize)
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens_override: None,
            enable_auto_compaction: true,
            limits: ContextLimitsConfig::default(),
            compaction: CompactionConfig::default(),
            task_budget: TaskBudgetConfig::default(),
            budget_allocation: BudgetAllocationConfig::default(),
            phase_complexity: PhaseComplexityConfig::default(),
            estimator: ContextEstimatorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    // MissionContext compaction settings
    pub compaction_threshold: f32,
    pub aggressive_ratio: f32,
    pub preserve_recent_tasks: usize,
    pub preserve_recent_learnings: usize,
    /// Maximum critical learnings to preserve during emergency compaction
    pub emergency_max_critical_learnings: usize,
    pub min_phase_age_for_compression: usize,
    pub enable_semantic_dedup: bool,
    pub phase_summary_max_chars: usize,
    pub task_summary_max_chars: usize,
    pub max_archived_snapshots: usize,
    pub dedup_similarity_threshold: f32,

    // TaskContext compaction settings
    /// Estimated token threshold for TaskContext compaction (default: 50_000)
    pub task_context_threshold: usize,
    /// Maximum key findings to retain in TaskContext (default: 100)
    pub max_key_findings: usize,
    /// Maximum blockers to retain in TaskContext (default: 20)
    pub max_blockers: usize,
    /// Maximum related files to retain in TaskContext (default: 200)
    pub max_related_files: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: 0.8,
            aggressive_ratio: 0.5,
            preserve_recent_tasks: 5,
            preserve_recent_learnings: 10,
            emergency_max_critical_learnings: 3,
            min_phase_age_for_compression: 2,
            enable_semantic_dedup: true,
            phase_summary_max_chars: 50,
            task_summary_max_chars: 30,
            max_archived_snapshots: 3,
            dedup_similarity_threshold: 0.8,
            // TaskContext defaults
            task_context_threshold: 50_000,
            max_key_findings: 100,
            max_blockers: 20,
            max_related_files: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextLimitsConfig {
    pub max_objective_chars: usize,
    pub max_critical_learnings: usize,
    pub max_key_decisions: usize,
    pub max_phase_files: usize,
    pub max_phase_learnings: usize,
    pub max_phase_changes: usize,
}

impl Default for ContextLimitsConfig {
    fn default() -> Self {
        Self {
            max_objective_chars: 200,
            max_critical_learnings: 5,
            max_key_decisions: 3,
            max_phase_files: 10,
            max_phase_learnings: 3,
            max_phase_changes: 3,
        }
    }
}

/// Configuration for context size estimation.
/// Controls pre-flight checks for MCP resource loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextEstimatorConfig {
    /// Estimated base context overhead from Claude Code SDK.
    /// Includes system prompt, tool definitions, and MCP server instructions.
    pub base_overhead_tokens: usize,
    /// Minimum available budget for actual work (prompt + evidence + output).
    pub min_working_budget: usize,
    /// Safety margin ratio to avoid hitting exact limits (0.0-1.0).
    pub safety_margin: f32,
    /// Ratio threshold for switching to selective loading (0.0-1.0).
    pub selective_loading_ratio: f32,
    /// Maximum directory depth for resource scanning.
    pub max_directory_depth: usize,
    /// Token threshold for warning about large files.
    pub large_file_warning_threshold: usize,
    /// Fallback chars per token when content is unreadable.
    pub fallback_chars_per_token: usize,
    /// Maximum files to scan per directory before extrapolating.
    /// Prevents slow scanning on large directories.
    pub max_files_per_directory: usize,
}

impl Default for ContextEstimatorConfig {
    fn default() -> Self {
        Self {
            base_overhead_tokens: 40_000,
            min_working_budget: 30_000,
            safety_margin: 0.9,
            selective_loading_ratio: 0.7,
            max_directory_depth: 5,
            large_file_warning_threshold: 10_000,
            fallback_chars_per_token: 4,
            max_files_per_directory: 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskBudgetConfig {
    pub mission_summary_tokens: usize,
    pub current_task_tokens: usize,
    pub direct_dependencies_tokens: usize,
    pub learnings_tokens: usize,
}

impl Default for TaskBudgetConfig {
    fn default() -> Self {
        Self {
            mission_summary_tokens: 200,
            current_task_tokens: 500,
            direct_dependencies_tokens: 1000,
            learnings_tokens: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetAllocationConfig {
    pub mission_summary_ratio: f32,
    pub current_phase_ratio: f32,
    pub current_task_ratio: f32,
    pub recent_tasks_ratio: f32,
    pub learnings_ratio: f32,
    pub evidence_ratio: f32,
    pub reserved_output_ratio: f32,
    pub recent_tasks_available_divisor: usize,
    pub rebalance_reduction_divisor: usize,
}

impl Default for BudgetAllocationConfig {
    fn default() -> Self {
        Self {
            mission_summary_ratio: 0.025,
            current_phase_ratio: 0.10,
            current_task_ratio: 0.15,
            recent_tasks_ratio: 0.10,
            learnings_ratio: 0.05,
            evidence_ratio: 0.15,
            reserved_output_ratio: 0.20,
            recent_tasks_available_divisor: 4,
            rebalance_reduction_divisor: 3,
        }
    }
}

impl BudgetAllocationConfig {
    pub fn for_budget(
        &self,
        max_total: usize,
    ) -> (usize, usize, usize, usize, usize, usize, usize) {
        (
            (max_total as f32 * self.mission_summary_ratio) as usize,
            (max_total as f32 * self.current_phase_ratio) as usize,
            (max_total as f32 * self.current_task_ratio) as usize,
            (max_total as f32 * self.recent_tasks_ratio) as usize,
            (max_total as f32 * self.learnings_ratio) as usize,
            (max_total as f32 * self.evidence_ratio) as usize,
            (max_total as f32 * self.reserved_output_ratio) as usize,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PhaseComplexityConfig {
    /// Weight for task count in complexity scoring
    pub task_count_weight: usize,
    /// Weight for dependency depth in complexity scoring
    pub dependency_depth_weight: usize,
    /// Weight for files touched in complexity scoring
    pub files_touched_weight: usize,
    /// Score threshold for Simple complexity (inclusive)
    pub simple_threshold: usize,
    /// Score threshold for Moderate complexity (inclusive)
    pub moderate_threshold: usize,
    /// Score threshold for Complex complexity (inclusive)
    pub complex_threshold: usize,
    /// Multiplier for Simple complexity budget adjustment
    pub simple_multiplier: f32,
    /// Multiplier for Moderate complexity budget adjustment
    pub moderate_multiplier: f32,
    /// Multiplier for Complex complexity budget adjustment
    pub complex_multiplier: f32,
    /// Multiplier for Critical complexity budget adjustment
    pub critical_multiplier: f32,
}

impl Default for PhaseComplexityConfig {
    fn default() -> Self {
        Self {
            task_count_weight: 2,
            dependency_depth_weight: 3,
            files_touched_weight: 1,
            simple_threshold: 10,
            moderate_threshold: 25,
            complex_threshold: 50,
            simple_multiplier: 0.7,
            moderate_multiplier: 1.0,
            complex_multiplier: 1.4,
            critical_multiplier: 1.8,
        }
    }
}

/// Token encoding strategy for context size estimation.
///
/// Note: Claude uses a proprietary tokenizer. These OpenAI-based
/// encodings are approximations. For most use cases, the difference
/// is negligible (< 5% error).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEncoding {
    /// cl100k_base: Used by GPT-4, ChatGPT. Generally closer to modern LLMs.
    /// Recommended for Claude estimation.
    #[default]
    Cl100kBase,
    /// o200k_base: Used by GPT-4o, o1 series. Newest encoding.
    O200kBase,
    /// p50k_base: Used by GPT-3.
    P50kBase,
    /// Heuristic: Fast approximation using chars_per_token ratio.
    /// Use when exact counting isn't critical or tiktoken fails.
    Heuristic,
}

/// Configuration for token counting and estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenizerConfig {
    /// Encoding strategy for token estimation.
    pub encoding: TokenEncoding,
    /// Characters per token for heuristic mode.
    /// Also used as fallback when tiktoken fails.
    pub heuristic_chars_per_token: usize,
    /// Whether to use heuristic as fallback on tiktoken errors.
    pub fallback_to_heuristic: bool,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            encoding: TokenEncoding::Cl100kBase,
            heuristic_chars_per_token: 4,
            fallback_to_heuristic: true,
        }
    }
}

/// Configuration for the rule system (artifact-architecture v3.0).
/// Rules represent domain knowledge (WHAT) that gets auto-injected based on context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Enable the rule system for context-aware knowledge injection.
    pub enabled: bool,
    /// Directory containing rules (relative to .claude/).
    pub rules_dir: String,
    /// Directory containing skills (relative to .claude/).
    pub skills_dir: String,
    /// Maximum rules to inject per request.
    pub max_rules_per_request: usize,
    /// Maximum combined rule content size (characters).
    pub max_rule_content_chars: usize,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules_dir: "rules".into(),
            skills_dir: "skills".into(),
            max_rules_per_request: 10,
            max_rule_content_chars: 50_000,
        }
    }
}
