use serde::{Deserialize, Serialize};

/// Configuration for task decomposition quality validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskDecompositionConfig {
    /// Minimum tasks per user story.
    pub min_tasks_per_story: usize,
    /// Maximum tasks per user story.
    pub max_tasks_per_story: usize,
    /// Task quality threshold (0.0-1.0).
    pub quality_threshold: f32,
    /// Maximum retry attempts on quality failure.
    pub max_retry_attempts: u32,
    /// Require affected_files to be non-empty.
    pub require_affected_files: bool,
}

impl Default for TaskDecompositionConfig {
    fn default() -> Self {
        Self {
            min_tasks_per_story: 2,
            max_tasks_per_story: 8,
            quality_threshold: 0.7,
            max_retry_attempts: 3,
            require_affected_files: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskScoringConfig {
    pub enabled: bool,
    pub weights: TaskScoringWeights,
    pub max_file_threshold: usize,
    pub max_dependency_threshold: usize,
    pub min_similarity_threshold: f32,
    pub max_history_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskScoringWeights {
    pub file: f32,
    pub dependency: f32,
    pub pattern: f32,
    pub confidence: f32,
    pub history: f32,
}

impl Default for TaskScoringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weights: TaskScoringWeights::default(),
            max_file_threshold: 20,
            max_dependency_threshold: 30,
            min_similarity_threshold: 0.3,
            max_history_tasks: 500,
        }
    }
}

impl Default for TaskScoringWeights {
    fn default() -> Self {
        Self {
            file: 0.20,
            dependency: 0.20,
            pattern: 0.25,
            confidence: 0.15,
            history: 0.20,
        }
    }
}
