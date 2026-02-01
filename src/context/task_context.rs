use crate::config::TaskBudgetConfig;

/// Token budget allocation for task context components.
///
/// Used by `PromptBuilder` to ensure prompts stay within token limits.
#[derive(Debug, Clone)]
pub struct TaskContextBudget {
    pub mission_summary: usize,
    pub current_task: usize,
    pub direct_dependencies: usize,
    pub learnings: usize,
}

impl Default for TaskContextBudget {
    fn default() -> Self {
        Self::from_config(&TaskBudgetConfig::default())
    }
}

impl From<&TaskBudgetConfig> for TaskContextBudget {
    fn from(config: &TaskBudgetConfig) -> Self {
        Self {
            mission_summary: config.mission_summary_tokens,
            current_task: config.current_task_tokens,
            direct_dependencies: config.direct_dependencies_tokens,
            learnings: config.learnings_tokens,
        }
    }
}

impl TaskContextBudget {
    pub fn from_config(config: &TaskBudgetConfig) -> Self {
        Self::from(config)
    }

    /// Total budget across all components.
    pub fn total(&self) -> usize {
        self.mission_summary + self.current_task + self.direct_dependencies + self.learnings
    }
}
