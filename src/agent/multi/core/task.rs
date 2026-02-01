//! Task types for agent execution.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::agent::AgentRole;
use crate::agent::multi::shared::TaskPriority;

/// Task for specialized agents.
///
/// Note: Session-aware context (participant awareness, notifications, coordination)
/// is provided via `InvocationInput.context: AgentContext` during agent invocation,
/// not through this task struct. This follows the stateless agent invocation pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: String,
    pub description: String,
    pub context: TaskContext,
    pub priority: TaskPriority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
}

impl AgentTask {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            context: TaskContext::default(),
            priority: TaskPriority::default(),
            role: None,
        }
    }

    pub fn with_context(mut self, context: TaskContext) -> Self {
        self.context = context;
        self
    }

    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_role(mut self, role: AgentRole) -> Self {
        self.role = Some(role);
        self
    }
}

/// Context shared between agents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskContext {
    pub mission_id: String,
    pub working_dir: PathBuf,
    #[serde(default)]
    pub key_findings: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub related_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composed_prompt: Option<String>,
}

impl TaskContext {
    pub fn new(mission_id: impl Into<String>, working_dir: PathBuf) -> Self {
        Self {
            mission_id: mission_id.into(),
            working_dir,
            ..Default::default()
        }
    }

    pub fn with_findings(mut self, findings: Vec<String>) -> Self {
        self.key_findings = findings;
        self
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.related_files = files;
        self
    }

    pub fn with_blockers(mut self, blockers: Vec<String>) -> Self {
        self.blockers = blockers;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_context_builder() {
        let ctx = TaskContext::new("mission-1", PathBuf::from("/tmp"))
            .with_findings(vec!["finding1".to_string()])
            .with_files(vec!["file1.rs".to_string()]);

        assert_eq!(ctx.mission_id, "mission-1");
        assert_eq!(ctx.key_findings.len(), 1);
        assert_eq!(ctx.related_files.len(), 1);
    }

    #[test]
    fn test_agent_task_builder() {
        let task = AgentTask::new("task-1", "Test task").with_priority(TaskPriority::High);

        assert_eq!(task.id, "task-1");
        assert_eq!(task.priority, TaskPriority::High);
    }
}
