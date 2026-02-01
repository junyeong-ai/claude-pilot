//! Planning agent specialized for task planning and decomposition.
//!
//! # Purpose
//!
//! The `PlanningAgent` creates detailed implementation plans by decomposing complex tasks
//! into ordered, verifiable steps. It serves as a **legacy fallback** for sequential
//! pipeline mode when the dynamic consensus system is not used.
//!
//! # Responsibilities
//!
//! - **Design implementation strategies**: Choose the best approach based on gathered evidence
//! - **Create specifications**: Define clear requirements and acceptance criteria
//! - **Decompose tasks**: Break complex work into atomic, focused subtasks
//! - **Identify dependencies**: Determine task ordering and prerequisite relationships
//! - **Assess risks**: Evaluate complexity, potential blockers, and mitigation strategies
//!
//! # Key Features
//!
//! - **Evidence-based planning**: All plans must reference verified research findings
//! - **Incremental approach**: Prefers small, independently verifiable steps
//! - **Testability focus**: Each step includes clear verification criteria
//! - **Risk assessment**: Identifies potential issues and rollback strategies
//! - **Structured output**: Provides ordered tasks with dependencies and acceptance criteria
//!
//! # Planning Principles
//!
//! 1. **Evidence-based**: Plans reference specific files, patterns, and research findings
//! 2. **Incremental**: Small steps reduce risk and enable continuous verification
//! 3. **Testable**: Each task has clear, objective acceptance criteria
//! 4. **Reversible**: Includes rollback strategies for failed steps
//!
//! # Integration
//!
//! The Planning Agent operates in two modes:
//!
//! ## Sequential Pipeline Mode (Legacy)
//! ```text
//! Research → Planning → Implementation → Verification
//! ```
//! Used when consensus is disabled or for simple, single-path tasks.
//!
//! ## Dynamic Consensus Mode (Preferred)
//! ```text
//! Research → Consensus → Implementation → Verification
//! ```
//! Planning Agent may participate in consensus but is not the sole planner.
//!
//! # Output Format
//!
//! Planning artifacts (`Plan` type) typically include:
//!
//! 1. **Overview**: Implementation approach and rationale
//! 2. **Ordered tasks**: Step-by-step breakdown with dependencies
//! 3. **Affected files**: Components to create/modify per task
//! 4. **Verification criteria**: How to validate each step
//! 5. **Risk assessment**: Potential issues and mitigations
//!
//! # Note on Architecture
//!
//! While still functional, the Planning Agent represents an older sequential approach.
//! Modern workflows prefer the `ConsensusPlanner` which achieves better results through
//! multi-agent collaboration and evidence-weighted voting.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::error::Result;

const PLANNING_SYSTEM_PROMPT: &str = r"# Planning Agent

You are a specialized planning agent focused on architecture and design.

## Primary Responsibilities
- Design implementation strategies
- Create specifications and plans
- Decompose complex tasks into subtasks
- Identify dependencies and ordering
- Assess complexity and risks

## Planning Principles
1. Evidence-based: All plans must reference gathered evidence
2. Incremental: Prefer small, verifiable steps
3. Testable: Each step should be independently verifiable
4. Reversible: Consider rollback strategies

## Output Format
Provide structured plans:
1. Overview and approach
2. Step-by-step tasks with dependencies
3. Affected files and components
4. Verification criteria for each step
5. Risk assessment and mitigations

## Constraints
- Plans must be based on verified evidence
- Each task should be atomic and focused
- Include clear acceptance criteria
";

pub struct PlanningAgent {
    core: AgentCore,
}

impl PlanningAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::core_planning(), task_agent),
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id.to_string(), task_agent))
    }

    fn build_planning_prompt(&self, task: &AgentTask) -> String {
        AgentPromptBuilder::new(PLANNING_SYSTEM_PROMPT, "Planning", task)
            .with_context(task)
            .with_related_files(task)
            .with_blockers(task)
            .with_section(
                "Expected Output",
                &[
                    "Create a detailed implementation plan:",
                    "1. Implementation approach and rationale",
                    "2. Ordered list of tasks with dependencies",
                    "3. Files to create/modify per task",
                    "4. Verification criteria for each task",
                    "5. Risk assessment",
                ],
            )
            .build()
    }

    fn extract_tasks(&self, output: &str) -> Vec<String> {
        // Minimum length to filter out section headers, bullet points, and other noise.
        // Tasks should be meaningful sentences (e.g., "Create authentication module").
        const MIN_TASK_LENGTH: usize = 10;

        output
            .split("\n\n")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.len() > MIN_TASK_LENGTH)
            .take(20)
            .collect()
    }
}

#[async_trait]
impl SpecializedAgent for PlanningAgent {
    fn role(&self) -> &AgentRole {
        self.core.role()
    }

    fn id(&self) -> &str {
        self.core.id()
    }

    fn identifier(&self) -> AgentIdentifier {
        self.core.identifier().clone()
    }

    fn system_prompt(&self) -> &str {
        PLANNING_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Planning agent executing");

        let prompt = self.build_planning_prompt(task);
        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                let findings = self.extract_tasks(&output);
                Ok(AgentTaskResult::success(&task.id, output.clone())
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("plan-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Plan,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tasks() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = PlanningAgent::new("test".to_string(), task_agent);

        let output = r#"## Implementation Plan

### Tasks:
1. Create the authentication module with JWT support
2. Implement user registration endpoint
- Add password hashing utility
- Set up database migrations for users table
"#;

        let tasks = agent.extract_tasks(output);
        assert!(!tasks.is_empty());
    }
}
