use serde::{Deserialize, Serialize};

use crate::config::{BudgetAllocationConfig, ModelConfig, PhaseComplexityConfig};

/// Actual token usage from API responses.
/// Compatible with claude-agent-rs Usage/TokenUsage types.
/// This provides ground truth for context management decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActualUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl ActualUsage {
    /// Create from values (typically from claude-agent-rs TokenUsage).
    pub fn new(input: u64, output: u64) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    }

    /// Create with cache information.
    pub fn with_cache(input: u64, output: u64, cache_read: u64, cache_create: u64) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_create,
        }
    }

    /// Total tokens consumed.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// Add another usage record (cumulative).
    pub fn add(&mut self, other: &ActualUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }

    /// Cache hit rate (0.0 to 1.0, clamped).
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        (self.cache_read_input_tokens as f64 / self.input_tokens as f64).min(1.0)
    }

    /// Effective input tokens (excluding cache reads).
    pub fn effective_input(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cache_read_input_tokens)
    }
}

impl From<claude_agent::types::Usage> for ActualUsage {
    fn from(u: claude_agent::types::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens as u64,
            output_tokens: u.output_tokens as u64,
            cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0) as u64,
            cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0) as u64,
        }
    }
}

impl From<claude_agent::types::TokenUsage> for ActualUsage {
    fn from(u: claude_agent::types::TokenUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseComplexity {
    Simple,
    Moderate,
    Complex,
    Critical,
}

impl std::fmt::Display for PhaseComplexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Simple => write!(f, "simple"),
            Self::Moderate => write!(f, "moderate"),
            Self::Complex => write!(f, "complex"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

impl PhaseComplexity {
    pub fn estimate(
        task_count: usize,
        dependency_depth: usize,
        files_touched: usize,
        config: &PhaseComplexityConfig,
    ) -> Self {
        let score = task_count * config.task_count_weight
            + dependency_depth * config.dependency_depth_weight
            + files_touched * config.files_touched_weight;
        if score <= config.simple_threshold {
            Self::Simple
        } else if score <= config.moderate_threshold {
            Self::Moderate
        } else if score <= config.complex_threshold {
            Self::Complex
        } else {
            Self::Critical
        }
    }

    fn multiplier(&self, config: &PhaseComplexityConfig) -> f32 {
        match self {
            Self::Simple => config.simple_multiplier,
            Self::Moderate => config.moderate_multiplier,
            Self::Complex => config.complex_multiplier,
            Self::Critical => config.critical_multiplier,
        }
    }

    fn upgrade(&self) -> Self {
        match self {
            Self::Simple => Self::Moderate,
            Self::Moderate => Self::Complex,
            Self::Complex | Self::Critical => Self::Critical,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    pub max_total: usize,
    pub allocation: BudgetAllocation,
    pub current_usage: BudgetUsage,
    pub compaction_threshold: f32,
    pub phase_complexity: PhaseComplexity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_usage: Option<ActualUsage>,
    #[serde(default)]
    pub allocation_config: BudgetAllocationConfig,
    #[serde(default)]
    pub phase_config: PhaseComplexityConfig,
}

impl TokenBudget {
    pub fn with_config(
        max_total: usize,
        compaction_threshold: f32,
        allocation_config: BudgetAllocationConfig,
        phase_config: PhaseComplexityConfig,
    ) -> Self {
        Self {
            max_total,
            allocation: BudgetAllocation::for_budget_with_config(max_total, &allocation_config),
            current_usage: BudgetUsage::default(),
            compaction_threshold,
            phase_complexity: PhaseComplexity::Moderate,
            actual_usage: None,
            allocation_config,
            phase_config,
        }
    }
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self::with_config(
            ModelConfig::default().usable_context() as usize,
            0.8,
            BudgetAllocationConfig::default(),
            PhaseComplexityConfig::default(),
        )
    }
}

impl TokenBudget {
    pub fn usage_ratio(&self) -> f32 {
        self.current_usage.total as f32 / self.max_total as f32
    }

    pub fn exceeds_threshold(&self) -> bool {
        self.usage_ratio() > self.compaction_threshold
    }

    pub fn available(&self) -> usize {
        self.max_total.saturating_sub(self.current_usage.total)
    }

    pub fn effective_allocation(&self, category: BudgetCategory) -> usize {
        let base = self.allocation.get(category);
        let multiplier = self.phase_complexity.multiplier(&self.phase_config);

        match category {
            BudgetCategory::CurrentPhase | BudgetCategory::CurrentTask => {
                (base as f32 * multiplier) as usize
            }
            BudgetCategory::RecentTasks => {
                let adjusted = (base as f32 * multiplier) as usize;
                adjusted
                    .min(self.available() / self.allocation_config.recent_tasks_available_divisor)
            }
            _ => base,
        }
    }

    pub fn set_phase_complexity(&mut self, complexity: PhaseComplexity) {
        self.phase_complexity = complexity;
        self.rebalance_allocations();
    }

    pub fn update_complexity_from_phase(
        &mut self,
        task_count: usize,
        dependency_depth: usize,
        files_touched: usize,
    ) {
        let complexity = PhaseComplexity::estimate(
            task_count,
            dependency_depth,
            files_touched,
            &self.phase_config,
        );
        self.set_phase_complexity(complexity);
    }

    /// Upgrade complexity if failure rate exceeds threshold.
    ///
    /// High failure rates indicate the phase needs more context budget.
    /// Upgrade is one-way (never decreases) to prevent oscillation.
    ///
    /// Returns true if complexity was upgraded.
    pub fn upgrade_on_high_failure_rate(
        &mut self,
        retry_count: u32,
        task_count: u32,
        threshold: f32,
    ) -> bool {
        if task_count == 0 {
            return false;
        }

        let failure_rate = retry_count as f32 / task_count as f32;
        if failure_rate > threshold {
            let upgraded = self.phase_complexity.upgrade();
            if upgraded != self.phase_complexity {
                self.set_phase_complexity(upgraded);
                return true;
            }
        }
        false
    }

    fn rebalance_allocations(&mut self) {
        let multiplier = self.phase_complexity.multiplier(&self.phase_config);
        let base =
            BudgetAllocation::for_budget_with_config(self.max_total, &self.allocation_config);

        let phase_boost =
            ((base.current_phase as f32 * multiplier) as usize).saturating_sub(base.current_phase);
        let task_boost =
            ((base.current_task as f32 * multiplier) as usize).saturating_sub(base.current_task);

        let total_boost = phase_boost + task_boost;
        let reduction_per_category =
            total_boost / self.allocation_config.rebalance_reduction_divisor;

        self.allocation.current_phase = (base.current_phase as f32 * multiplier) as usize;
        self.allocation.current_task = (base.current_task as f32 * multiplier) as usize;

        self.allocation.recent_tasks = base.recent_tasks.saturating_sub(reduction_per_category);
        self.allocation.learnings = base.learnings.saturating_sub(reduction_per_category);
        self.allocation.evidence = base.evidence.saturating_sub(reduction_per_category);
    }

    // ========== Actual Usage Methods ==========

    /// Check if actual usage tracking is enabled.
    pub fn has_actual_usage(&self) -> bool {
        self.actual_usage.is_some()
    }

    /// Update with actual usage from API response.
    /// This provides ground truth for compaction decisions.
    /// The actual usage is cumulative - call `reset_actual_usage()` after compaction.
    pub fn update_actual(&mut self, usage: ActualUsage) {
        match &mut self.actual_usage {
            Some(existing) => existing.add(&usage),
            None => self.actual_usage = Some(usage),
        }
    }

    /// Update with actual usage from claude-agent-rs types.
    pub fn update_from_api(&mut self, usage: impl Into<ActualUsage>) {
        self.update_actual(usage.into());
    }

    /// Get the effective usage ratio for compaction decisions.
    /// Uses actual usage if available, otherwise falls back to estimation.
    pub fn effective_usage_ratio(&self) -> f32 {
        if let Some(ref actual) = self.actual_usage {
            actual.input_tokens as f32 / self.max_total as f32
        } else {
            self.usage_ratio()
        }
    }

    /// Check threshold using effective (best available) data.
    pub fn exceeds_threshold_effective(&self) -> bool {
        self.effective_usage_ratio() > self.compaction_threshold
    }

    /// Get current input tokens (actual if available, otherwise estimated).
    pub fn current_input_tokens(&self) -> usize {
        if let Some(ref actual) = self.actual_usage {
            actual.input_tokens as usize
        } else {
            self.current_usage.total
        }
    }

    /// Reset actual usage tracking.
    /// Call this after compaction to start fresh cumulative tracking.
    pub fn reset_actual_usage(&mut self) {
        self.actual_usage = None;
    }

    /// Clear actual usage counts but keep tracking enabled.
    /// Use when you want to reset counts without losing the "actual tracking active" state.
    pub fn clear_actual_counts(&mut self) {
        if self.actual_usage.is_some() {
            self.actual_usage = Some(ActualUsage::default());
        }
    }

    /// Get cache efficiency metrics if actual usage is available.
    pub fn cache_efficiency(&self) -> Option<f64> {
        self.actual_usage
            .as_ref()
            .filter(|u| u.input_tokens > 0)
            .map(|u| u.cache_hit_rate())
    }

    /// Returns the actual usage data if available.
    pub fn actual_usage(&self) -> Option<&ActualUsage> {
        self.actual_usage.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetCategory {
    MissionSummary,
    CurrentPhase,
    CurrentTask,
    RecentTasks,
    Learnings,
    Evidence,
    ReservedOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetAllocation {
    pub mission_summary: usize,
    pub current_phase: usize,
    pub current_task: usize,
    pub recent_tasks: usize,
    pub learnings: usize,
    pub evidence: usize,
    pub reserved_output: usize,
}

impl BudgetAllocation {
    pub fn for_budget_with_config(max_total: usize, config: &BudgetAllocationConfig) -> Self {
        let (
            mission_summary,
            current_phase,
            current_task,
            recent_tasks,
            learnings,
            evidence,
            reserved_output,
        ) = config.for_budget(max_total);
        Self {
            mission_summary,
            current_phase,
            current_task,
            recent_tasks,
            learnings,
            evidence,
            reserved_output,
        }
    }
}

impl Default for BudgetAllocation {
    fn default() -> Self {
        Self::for_budget_with_config(
            ModelConfig::default().usable_context() as usize,
            &BudgetAllocationConfig::default(),
        )
    }
}

impl BudgetAllocation {
    pub fn get(&self, category: BudgetCategory) -> usize {
        match category {
            BudgetCategory::MissionSummary => self.mission_summary,
            BudgetCategory::CurrentPhase => self.current_phase,
            BudgetCategory::CurrentTask => self.current_task,
            BudgetCategory::RecentTasks => self.recent_tasks,
            BudgetCategory::Learnings => self.learnings,
            BudgetCategory::Evidence => self.evidence,
            BudgetCategory::ReservedOutput => self.reserved_output,
        }
    }

    pub fn total_allocated(&self) -> usize {
        self.mission_summary
            + self.current_phase
            + self.current_task
            + self.recent_tasks
            + self.learnings
            + self.evidence
            + self.reserved_output
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub total: usize,
    pub mission_summary: usize,
    pub current_phase: usize,
    pub current_task: usize,
    pub recent_tasks: usize,
    pub learnings: usize,
    pub evidence: usize,
    pub reserved_output: usize,
}

impl BudgetUsage {
    pub fn get(&self, category: BudgetCategory) -> usize {
        match category {
            BudgetCategory::MissionSummary => self.mission_summary,
            BudgetCategory::CurrentPhase => self.current_phase,
            BudgetCategory::CurrentTask => self.current_task,
            BudgetCategory::RecentTasks => self.recent_tasks,
            BudgetCategory::Learnings => self.learnings,
            BudgetCategory::Evidence => self.evidence,
            BudgetCategory::ReservedOutput => self.reserved_output,
        }
    }

    pub fn add(&mut self, category: BudgetCategory, tokens: usize) {
        match category {
            BudgetCategory::MissionSummary => self.mission_summary += tokens,
            BudgetCategory::CurrentPhase => self.current_phase += tokens,
            BudgetCategory::CurrentTask => self.current_task += tokens,
            BudgetCategory::RecentTasks => self.recent_tasks += tokens,
            BudgetCategory::Learnings => self.learnings += tokens,
            BudgetCategory::Evidence => self.evidence += tokens,
            BudgetCategory::ReservedOutput => self.reserved_output += tokens,
        }
        self.total += tokens;
    }

    pub fn set(&mut self, category: BudgetCategory, tokens: usize) {
        let old = self.get(category);
        self.total = self.total.saturating_sub(old) + tokens;
        match category {
            BudgetCategory::MissionSummary => self.mission_summary = tokens,
            BudgetCategory::CurrentPhase => self.current_phase = tokens,
            BudgetCategory::CurrentTask => self.current_task = tokens,
            BudgetCategory::RecentTasks => self.recent_tasks = tokens,
            BudgetCategory::Learnings => self.learnings = tokens,
            BudgetCategory::Evidence => self.evidence = tokens,
            BudgetCategory::ReservedOutput => self.reserved_output = tokens,
        }
    }
}
