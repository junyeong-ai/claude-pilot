//! Result types for agent task execution.

use serde::{Deserialize, Serialize};

/// Status indicating task execution outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Completed,
    Failed,
    Deferred,
}

impl TaskStatus {
    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// Result from agent task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskResult {
    pub task_id: String,
    pub success: bool,
    pub output: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifact>,
    #[serde(default)]
    pub status: TaskStatus,
}

impl AgentTaskResult {
    pub fn success(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            success: true,
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: TaskStatus::Completed,
        }
    }

    pub fn failure(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            success: false,
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: TaskStatus::Failed,
        }
    }

    pub fn deferred(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            success: false,
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: TaskStatus::Deferred,
        }
    }

    pub fn with_findings(mut self, findings: Vec<String>) -> Self {
        self.findings = findings;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Vec<TaskArtifact>) -> Self {
        self.artifacts = artifacts;
        self
    }

    pub fn is_deferred(&self) -> bool {
        self.status.is_deferred()
    }
}

/// Artifact produced by agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskArtifact {
    pub name: String,
    pub content: String,
    pub artifact_type: ArtifactType,
}

impl TaskArtifact {
    pub fn new(
        name: impl Into<String>,
        content: impl Into<String>,
        artifact_type: ArtifactType,
    ) -> Self {
        Self {
            name: name.into(),
            content: content.into(),
            artifact_type,
        }
    }

    pub fn evidence(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(name, content, ArtifactType::Evidence)
    }

    pub fn plan(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(name, content, ArtifactType::Plan)
    }

    pub fn code(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(name, content, ArtifactType::Code)
    }

    pub fn report(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(name, content, ArtifactType::Report)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    Evidence,
    Plan,
    Code,
    Report,
}

/// Result of convergent verification (2-pass requirement).
#[derive(Debug, Clone)]
pub struct ConvergenceResult {
    pub converged: bool,
    pub clean_rounds: usize,
    pub total_rounds: usize,
    pub issues_found: Vec<String>,
    pub issues_fixed: Vec<String>,
    pub final_verdict: VerificationVerdict,
}

impl ConvergenceResult {
    pub fn passed(clean_rounds: usize, total_rounds: usize) -> Self {
        Self {
            converged: true,
            clean_rounds,
            total_rounds,
            issues_found: vec![],
            issues_fixed: vec![],
            final_verdict: VerificationVerdict::Passed,
        }
    }

    pub fn failed(total_rounds: usize, issues: Vec<String>) -> Self {
        Self {
            converged: false,
            clean_rounds: 0,
            total_rounds,
            issues_found: issues,
            issues_fixed: vec![],
            final_verdict: VerificationVerdict::Failed,
        }
    }

    pub fn timeout(total_rounds: usize) -> Self {
        Self {
            converged: false,
            clean_rounds: 0,
            total_rounds,
            issues_found: vec![],
            issues_fixed: vec![],
            final_verdict: VerificationVerdict::Timeout,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationVerdict {
    Passed,
    PartialPass,
    Failed,
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_task_result_builders() {
        let success = AgentTaskResult::success("task-1", "Done");
        assert!(success.success);
        assert_eq!(success.task_id, "task-1");
        assert_eq!(success.status, TaskStatus::Completed);
        assert!(!success.is_deferred());

        let failure = AgentTaskResult::failure("task-2", "Error");
        assert!(!failure.success);
        assert_eq!(failure.status, TaskStatus::Failed);
        assert!(!failure.is_deferred());

        let deferred = AgentTaskResult::deferred("task-3", "Yielded to other agent");
        assert!(!deferred.success);
        assert_eq!(deferred.status, TaskStatus::Deferred);
        assert!(deferred.is_deferred());
    }

    #[test]
    fn test_task_artifact_builders() {
        let evidence = TaskArtifact::evidence("findings", "content");
        assert_eq!(evidence.artifact_type, ArtifactType::Evidence);

        let plan = TaskArtifact::plan("implementation", "steps");
        assert_eq!(plan.artifact_type, ArtifactType::Plan);
    }

    #[test]
    fn test_convergence_result() {
        let passed = ConvergenceResult::passed(2, 3);
        assert!(passed.converged);
        assert_eq!(passed.final_verdict, VerificationVerdict::Passed);

        let failed = ConvergenceResult::failed(5, vec!["issue1".to_string()]);
        assert!(!failed.converged);
        assert_eq!(failed.final_verdict, VerificationVerdict::Failed);
    }
}
