//! Individual phase runner methods for the Coordinator.
//!
//! Contains research, planning, code review, architecture review, and
//! single-pass verification phase execution.

use std::path::Path;

use tracing::{debug, info, warn};

use super::super::traits::{
    AgentRole, AgentTask, AgentTaskResult, TaskContext, TaskPriority, extract_files_from_output,
};
use super::Coordinator;
use crate::error::Result;

impl Coordinator {
    pub(super) async fn run_code_review(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        if self.pool.agent_count(&AgentRole::reviewer()) == 0 {
            debug!("No reviewer agent registered, skipping code review");
            return Ok(vec![]);
        }

        let mut task = AgentTask {
            id: "code-review".to_string(),
            description: format!(
                "Review code changes for quality. Files: {}",
                context.related_files.join(", ")
            ),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::reviewer()),
        };

        self.enrich_task_and_emit(&mut task).await;

        match self.pool.execute(&task, working_dir).await {
            Ok(result) => Ok(vec![result]),
            Err(e) => {
                warn!(error = %e, "Code review failed");
                Ok(vec![])
            }
        }
    }

    pub(super) async fn run_architecture_review(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<Option<AgentTaskResult>> {
        if !self.pool.has_agent("architecture") {
            debug!("No architecture agent registered, skipping architecture review");
            return Ok(None);
        }

        let mut task = AgentTask {
            id: "architecture-review".to_string(),
            description: format!(
                "Validate architecture boundaries and conventions for changes in: {}",
                context.related_files.join(", ")
            ),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::architect()),
        };

        self.enrich_task_and_emit(&mut task).await;

        match self.pool.execute(&task, working_dir).await {
            Ok(result) => Ok(Some(result)),
            Err(e) => {
                warn!(error = %e, "Architecture review failed");
                Ok(None)
            }
        }
    }

    pub(super) async fn run_research_phase(
        &self,
        context: &mut TaskContext,
        description: &str,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase 1: Research");

        let mut task = AgentTask {
            id: "research-1".to_string(),
            description: format!("Analyze codebase for: {}", description),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::core_research()),
        };

        self.enrich_task_and_emit(&mut task).await;

        let result = self.pool.execute(&task, working_dir).await?;

        if result.is_success() {
            context.key_findings.extend(result.findings.clone());
            context.related_files = extract_files_from_output(&result.output, 20, Some(working_dir));
        } else {
            context
                .blockers
                .push(format!("Research failed: {}", result.output));
        }

        Ok(result)
    }

    pub(super) async fn run_planning_phase(
        &self,
        context: &mut TaskContext,
        description: &str,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase 2: Planning");

        let mut task = AgentTask {
            id: "planning-1".to_string(),
            description: format!("Create implementation plan for: {}", description),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::core_planning()),
        };

        self.enrich_task_and_emit(&mut task).await;

        let result = self.pool.execute(&task, working_dir).await?;

        if result.is_success() {
            context.key_findings.extend(result.findings.clone());
        } else {
            context
                .blockers
                .push(format!("Planning failed: {}", result.output));
        }

        Ok(result)
    }

    pub(super) async fn run_verification_phase(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase: Verification");

        let mut task = AgentTask {
            id: "verification-1".to_string(),
            description: "Verify all changes and run quality checks".to_string(),
            context: context.clone(),
            priority: TaskPriority::Critical,
            role: Some(AgentRole::core_verifier()),
        };

        self.enrich_task_and_emit(&mut task).await;

        self.pool.execute(&task, working_dir).await
    }
}
