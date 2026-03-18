use serde::{Deserialize, Serialize};

use crate::config::model::ModelConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorConfig {
    pub max_iterations: u32,
    pub max_parallel_tasks: usize,
    pub auto_commit: bool,
    /// When true, retry limits are dynamically increased based on failure patterns.
    /// Task max_retries increases after each failure category change.
    pub adaptive_limits: bool,
    /// Threshold in seconds before a lock is considered stale (default: 300 = 5 minutes)
    pub lock_stale_threshold_secs: u64,
    /// Number of retry attempts for lock acquisition
    pub lock_retry_attempts: u32,
    /// Delay in milliseconds between lock retry attempts (multiplied by attempt number)
    pub lock_retry_delay_ms: u64,
    /// Heartbeat update interval in seconds (default: 30)
    pub heartbeat_interval_secs: u64,
    /// Maximum mission duration in seconds (0 = unlimited, default: 604800 = 7 days)
    pub mission_timeout_secs: u64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            max_parallel_tasks: 10,
            auto_commit: true,
            adaptive_limits: true,
            lock_stale_threshold_secs: 300,
            lock_retry_attempts: 3,
            lock_retry_delay_ms: 100,
            heartbeat_interval_secs: 30,
            mission_timeout_secs: 604800, // 7 days
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IsolationConfig {
    pub worktrees_dir: String,
    pub small_change_threshold: usize,
    pub large_change_threshold: usize,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            worktrees_dir: String::from(".worktrees"),
            small_change_threshold: 2,
            large_change_threshold: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    pub default_branch: String,
    pub commit_prefix: String,
    pub branch_prefix: String,
    pub auto_push: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            default_branch: String::from("main"),
            commit_prefix: String::from("wip"),
            branch_prefix: String::from("pilot"),
            auto_push: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub desktop: bool,
    pub event_log: bool,
    pub hook_command: Option<String>,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            desktop: true,
            event_log: true,
            hook_command: None,
        }
    }
}

/// Configuration for display limits and presentation.
/// Controls how many items are shown in various contexts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Maximum recent items to show (learnings, phases, tasks, etc.)
    pub max_recent_items: usize,
    /// Maximum samples to display (errors, validation results, etc.)
    pub max_display_samples: usize,
    /// Maximum path depth for directory analysis
    pub max_path_depth: usize,
    /// Git commit hash display length
    pub commit_display_length: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_recent_items: 3,
            max_display_samples: 5,
            max_path_depth: 2,
            commit_display_length: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChunkedPlanningConfig {
    pub enabled: bool,
    pub max_user_stories_per_chunk: usize,
    pub max_requirements_per_chunk: usize,
    pub auto_chunk: bool,
    pub max_tokens_divisor: usize,
    pub complexity_divisor: usize,
    /// Minimum number of phases for mission outline.
    pub min_phases: usize,
    /// Maximum number of phases for mission outline.
    pub max_phases: usize,
    /// Maximum user stories per phase.
    pub max_stories_per_phase: usize,
}

impl ChunkedPlanningConfig {
    pub fn max_tokens_per_chunk(&self, model_config: &ModelConfig) -> usize {
        model_config.usable_context() as usize / self.max_tokens_divisor
    }

    pub fn complexity_threshold(&self, model_config: &ModelConfig) -> usize {
        model_config.usable_context() as usize / self.complexity_divisor
    }
}

impl Default for ChunkedPlanningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_user_stories_per_chunk: 3,
            max_requirements_per_chunk: 10,
            auto_chunk: true,
            max_tokens_divisor: 5,
            complexity_divisor: 3,
            min_phases: 2,
            max_phases: 5,
            max_stories_per_phase: 4,
        }
    }
}
