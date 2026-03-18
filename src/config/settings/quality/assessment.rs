use serde::{Deserialize, Serialize};

/// Complexity assessment configuration.
/// All complexity decisions are LLM-based - no heuristic gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ComplexityAssessmentConfig {
    pub assessment_timeout_secs: u64,
}

impl Default for ComplexityAssessmentConfig {
    fn default() -> Self {
        Self {
            assessment_timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceBudgetConfig {
    pub max_relevant_files: usize,
    pub max_dependencies_display: usize,
    pub file_budget_percent: usize,
    pub pattern_budget_percent: usize,
    pub dependency_budget_percent: usize,
    pub knowledge_budget_percent: usize,
    pub min_budget_threshold: usize,
    pub min_confidence_display: f32,
    pub spec_evidence_budget: usize,
    pub plan_evidence_budget: usize,
    pub token_char_ratio: usize,
    pub skill_confidence: f32,
    pub rule_confidence: f32,
    pub markdown_truncation_buffer: usize,
    pub exclude_dirs: Vec<String>,
}

impl Default for EvidenceBudgetConfig {
    fn default() -> Self {
        Self {
            max_relevant_files: 20,
            max_dependencies_display: 10,
            file_budget_percent: 40,
            pattern_budget_percent: 20,
            dependency_budget_percent: 20,
            knowledge_budget_percent: 20,
            min_budget_threshold: 100,
            min_confidence_display: 0.6,
            spec_evidence_budget: 8000,
            plan_evidence_budget: 50_000,
            token_char_ratio: 4,
            skill_confidence: 0.8,
            rule_confidence: 0.85,
            markdown_truncation_buffer: 50,
            exclude_dirs: super::super::DEFAULT_EXCLUDE_DIRS
                .iter()
                .map(|s| (*s).into())
                .collect(),
        }
    }
}

/// Configuration for Pattern Learning (Phase 2).
/// 4-stage pipeline: RETRIEVE -> JUDGE -> DISTILL -> CONSOLIDATE
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternBankConfig {
    /// Enable pattern learning.
    pub enabled: bool,
    /// Minimum success rate for pattern retention (0.0-1.0).
    pub min_success_rate: f32,
    /// Minimum confidence for pattern extraction (0.0-1.0).
    pub min_confidence: f32,
    /// Interval in seconds for consolidation (cleanup) operation.
    pub consolidation_interval_secs: u64,
    /// Maximum patterns to retain in memory cache.
    pub max_patterns: usize,
    /// Days of inactivity before pattern is eligible for removal.
    pub inactive_days_threshold: u32,
    /// Maximum patterns to return from RETRIEVE.
    pub max_retrieve_results: usize,
    /// Minimum Jaccard similarity for pattern matching.
    pub min_similarity_threshold: f32,
    /// Weight for similarity in pattern retrieve scoring.
    pub similarity_weight: f32,
    /// Weight for success rate in pattern retrieve scoring.
    pub success_rate_weight: f32,
    /// Weight for recency in pattern retrieve scoring.
    pub recency_weight: f32,
    /// Weight for confidence in pattern retrieve scoring.
    pub confidence_weight: f32,
    /// Similarity threshold for merging patterns during consolidation (0.0-1.0).
    pub merge_similarity_threshold: f32,
    /// Penalty per additional attempt in trajectory scoring (0.0-1.0).
    pub trajectory_attempt_penalty: f32,
    /// Penalty per side effect in trajectory scoring (0.0-1.0).
    pub trajectory_side_effect_penalty: f32,
    /// Initial confidence for successful patterns (0.0-1.0).
    pub initial_success_confidence: f32,
    /// Initial confidence for failed patterns (0.0-1.0).
    pub initial_failure_confidence: f32,
    /// Weight of success rate in confidence calculation (0.0-1.0).
    pub confidence_success_rate_weight: f32,
    /// Days before pattern confidence starts decaying.
    pub recency_decay_days: u32,
    /// Decay rate per day after recency threshold (0.0-1.0).
    pub recency_decay_rate: f32,
}

impl Default for PatternBankConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_success_rate: 0.3,
            min_confidence: 0.6,
            consolidation_interval_secs: 3600,
            max_patterns: 500,
            inactive_days_threshold: 30,
            max_retrieve_results: 5,
            min_similarity_threshold: 0.3,
            similarity_weight: 0.3,
            success_rate_weight: 0.3,
            recency_weight: 0.2,
            confidence_weight: 0.2,
            merge_similarity_threshold: 0.9,
            trajectory_attempt_penalty: 0.1,
            trajectory_side_effect_penalty: 0.1,
            initial_success_confidence: 0.7,
            initial_failure_confidence: 0.3,
            confidence_success_rate_weight: 0.7,
            recency_decay_days: 7,
            recency_decay_rate: 0.02,
        }
    }
}
