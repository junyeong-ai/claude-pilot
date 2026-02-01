//! Architect agent for design validation and long-term consistency.

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

/// System prompt for design validation.
///
/// # Distinction from ArchitectureAgent
///
/// - **ArchitectAgent** (this): High-level design advisor that validates architectural decisions
///   for consistency, cohesion, and maintainability. Participates in consensus as `Advisor` role.
///   Uses the `Plan` skill.
///
/// - **ArchitectureAgent**: Runtime enforcement agent that detects boundary violations and
///   convention breaches based on project-specific rules. Uses `Architecture` role category.
const ARCHITECT_SYSTEM_PROMPT: &str = r"You are an Architect Agent focused on design validation and long-term consistency.

## Responsibilities
- Validate design decisions against established patterns
- Ensure architectural consistency across changes
- Identify systemic impact and technical debt
- Guide implementation toward clean architecture

## Validation Principles
1. Consistency: Follow existing architectural patterns
2. Cohesion: Clear, focused module responsibilities
3. Coupling: Minimize inter-module dependencies
4. Scalability: Accommodate future growth
5. Maintainability: Prefer simple solutions

## Output Format
- `PASS` - Design is sound
- `CONCERNS` - Design has minor issues to address
- `FAIL` - Design violates core principles

Include specific findings with rationale.";

pub struct ArchitectAgent {
    core: AgentCore,
}

impl ArchitectAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::architect(), task_agent),
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id.to_string(), task_agent))
    }

    fn build_prompt(&self, task: &AgentTask) -> String {
        let system_prompt = task
            .context
            .composed_prompt
            .as_deref()
            .unwrap_or(ARCHITECT_SYSTEM_PROMPT);

        AgentPromptBuilder::new(system_prompt, "Design Validation", task)
            .with_context(task)
            .with_related_files(task)
            .with_section(
                "Focus",
                &[
                    "1. Assess architectural impact",
                    "2. Check pattern violations",
                    "3. Evaluate maintainability",
                    "4. Identify technical debt",
                ],
            )
            .build()
    }

    fn extract_verdict(output: &str) -> Verdict {
        let upper = output.to_uppercase();
        if upper.contains("FAIL") || upper.contains("REJECTED") {
            Verdict::Fail
        } else if upper.contains("CONCERNS") || upper.contains("WARNING") {
            Verdict::Concerns
        } else {
            Verdict::Pass
        }
    }

    fn extract_findings(output: &str) -> Vec<String> {
        output
            .lines()
            .filter(|line| {
                let lower = line.to_lowercase();
                lower.contains("concern")
                    || lower.contains("warning")
                    || lower.contains("risk")
                    || lower.contains("violation")
                    || lower.contains("issue")
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(10)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Concerns,
    Fail,
}

#[async_trait]
impl SpecializedAgent for ArchitectAgent {
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
        ARCHITECT_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Architect agent executing");

        let prompt = self.build_prompt(task);
        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                let verdict = Self::extract_verdict(&output);
                let findings = Self::extract_findings(&output);
                let result = if verdict != Verdict::Fail {
                    AgentTaskResult::success(&task.id, output.clone())
                } else {
                    AgentTaskResult::failure(&task.id, output.clone())
                };
                Ok(result
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("design-review-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Report,
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
    fn test_extract_verdict_pass() {
        assert_eq!(
            ArchitectAgent::extract_verdict("PASS - looks good"),
            Verdict::Pass
        );
        assert_eq!(
            ArchitectAgent::extract_verdict("Design is sound"),
            Verdict::Pass
        );
    }

    #[test]
    fn test_extract_verdict_concerns() {
        assert_eq!(
            ArchitectAgent::extract_verdict("CONCERNS: coupling issue"),
            Verdict::Concerns
        );
        assert_eq!(
            ArchitectAgent::extract_verdict("WARNING: boundary violation"),
            Verdict::Concerns
        );
    }

    #[test]
    fn test_extract_verdict_fail() {
        assert_eq!(
            ArchitectAgent::extract_verdict("FAIL: architecture violation"),
            Verdict::Fail
        );
        assert_eq!(
            ArchitectAgent::extract_verdict("REJECTED: breaks patterns"),
            Verdict::Fail
        );
    }

    #[test]
    fn test_extract_findings() {
        let output = r#"
Design Review:
- Concern: tight coupling between modules
- Warning: circular dependency risk
- Risk: performance under load
- Note: consider refactoring later
"#;
        let findings = ArchitectAgent::extract_findings(output);
        assert_eq!(findings.len(), 3);
        assert!(findings[0].contains("coupling"));
    }

    #[test]
    fn test_extract_findings_empty() {
        let output = "Everything looks good. PASS";
        let findings = ArchitectAgent::extract_findings(output);
        assert!(findings.is_empty());
    }

    #[test]
    fn test_role() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ArchitectAgent::new("test".to_string(), task_agent);
        assert_eq!(agent.role().id, "architect");
    }
}
