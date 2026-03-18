//! Result types for agent task execution.

use serde::{Deserialize, Serialize};

/// Status indicating task execution outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    #[default]
    Completed,
    Failed,
    Deferred,
}

impl ExecutionOutcome {
    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// Result from agent task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskResult {
    pub task_id: String,
    pub output: String,
    #[serde(default)]
    pub findings: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifact>,
    #[serde(default)]
    pub status: ExecutionOutcome,
}

impl AgentTaskResult {
    pub fn success(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: ExecutionOutcome::Completed,
        }
    }

    pub fn failure(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: ExecutionOutcome::Failed,
        }
    }

    pub fn deferred(task_id: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            output: output.into(),
            findings: vec![],
            artifacts: vec![],
            status: ExecutionOutcome::Deferred,
        }
    }

    pub fn is_success(&self) -> bool {
        self.status == ExecutionOutcome::Completed
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
pub struct AgentConvergenceResult {
    pub converged: bool,
    pub clean_rounds: usize,
    pub total_rounds: usize,
    pub issues_found: Vec<String>,
    pub issues_fixed: Vec<String>,
    pub final_verdict: VerificationVerdict,
}

impl AgentConvergenceResult {
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
        assert!(success.is_success());
        assert_eq!(success.task_id, "task-1");
        assert_eq!(success.status, ExecutionOutcome::Completed);
        assert!(!success.is_deferred());

        let failure = AgentTaskResult::failure("task-2", "Error");
        assert!(!failure.is_success());
        assert_eq!(failure.status, ExecutionOutcome::Failed);
        assert!(!failure.is_deferred());

        let deferred = AgentTaskResult::deferred("task-3", "Yielded to other agent");
        assert!(!deferred.is_success());
        assert_eq!(deferred.status, ExecutionOutcome::Deferred);
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
        let passed = AgentConvergenceResult::passed(2, 3);
        assert!(passed.converged);
        assert_eq!(passed.final_verdict, VerificationVerdict::Passed);

        let failed = AgentConvergenceResult::failed(5, vec!["issue1".to_string()]);
        assert!(!failed.converged);
        assert_eq!(failed.final_verdict, VerificationVerdict::Failed);
    }
}
