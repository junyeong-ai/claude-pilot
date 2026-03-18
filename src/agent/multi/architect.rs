//! Architect agent for design validation and long-term consistency.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
/// # Distinction from BoundaryEnforcementAgent
///
/// - **ArchitectAgent** (this): High-level design advisor that validates architectural decisions
///   for consistency, cohesion, and maintainability. Participates in consensus as `Advisor` role.
///   Uses the `Plan` skill.
///
/// - **BoundaryEnforcementAgent**: Runtime enforcement agent that detects boundary violations and
///   convention breaches based on project-specific rules. Uses `Architecture` role category.
const ARCHITECT_SYSTEM_PROMPT: &str = r#"You are an Architect Agent focused on design validation and long-term consistency.

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

## Response Format
Respond with a JSON object containing:
- "verdict": one of "pass", "concerns", or "fail"
- "findings": array of objects with "description" and "category" fields

Categories: "consistency", "cohesion", "coupling", "scalability", "maintainability", "security", "other"

Example:
{"verdict": "concerns", "findings": [{"description": "Module X has tight coupling with Y", "category": "coupling"}]}
"#;

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
        let system_prompt = match &task.context.manifest_context {
            Some(ctx) if !ctx.is_empty() => {
                format!("{}\n\n---\n\n{}", ARCHITECT_SYSTEM_PROMPT, ctx)
            }
            _ => ARCHITECT_SYSTEM_PROMPT.to_string(),
        };
        let system_prompt = system_prompt.as_str();

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
}

/// Verdict for architect review — uses structured output, not string matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    Pass,
    Concerns,
    Fail,
}

/// A single finding from the architect review.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ArchitectFinding {
    pub description: String,
    #[serde(default = "default_category")]
    pub category: String,
}

fn default_category() -> String {
    "other".to_string()
}

/// Structured response from the architect agent.
/// Uses `schemars::JsonSchema` for LLM structured output (no English keyword matching).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ArchitectReviewResponse {
    pub verdict: Verdict,
    #[serde(default)]
    pub findings: Vec<ArchitectFinding>,
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
        let response: std::result::Result<ArchitectReviewResponse, _> = self
            .core
            .task_agent
            .run_prompt_review(&prompt, working_dir)
            .await;

        match response {
            Ok(review) => {
                let output = serde_json::to_string_pretty(&review)
                    .unwrap_or_else(|_| format!("{:?}", review));
                let findings: Vec<String> = review
                    .findings
                    .iter()
                    .map(|f| format!("[{}] {}", f.category, f.description))
                    .collect();
                let result = if review.verdict != Verdict::Fail {
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
    fn test_deserialize_pass() {
        let json = r#"{"verdict": "pass", "findings": []}"#;
        let resp: ArchitectReviewResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.verdict, Verdict::Pass);
        assert!(resp.findings.is_empty());
    }

    #[test]
    fn test_deserialize_concerns_with_findings() {
        let json = r#"{
            "verdict": "concerns",
            "findings": [
                {"description": "tight coupling between modules", "category": "coupling"},
                {"description": "circular dependency risk", "category": "cohesion"}
            ]
        }"#;
        let resp: ArchitectReviewResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.verdict, Verdict::Concerns);
        assert_eq!(resp.findings.len(), 2);
        assert!(resp.findings[0].description.contains("coupling"));
        assert_eq!(resp.findings[1].category, "cohesion");
    }

    #[test]
    fn test_deserialize_fail() {
        let json = r#"{"verdict": "fail", "findings": [{"description": "violates core principles"}]}"#;
        let resp: ArchitectReviewResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.verdict, Verdict::Fail);
        assert_eq!(resp.findings.len(), 1);
        // category defaults to "other" when omitted
        assert_eq!(resp.findings[0].category, "other");
    }

    #[test]
    fn test_deserialize_missing_findings_defaults_empty() {
        let json = r#"{"verdict": "pass"}"#;
        let resp: ArchitectReviewResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.verdict, Verdict::Pass);
        assert!(resp.findings.is_empty());
    }

    #[test]
    fn test_roundtrip_serialization() {
        let resp = ArchitectReviewResponse {
            verdict: Verdict::Concerns,
            findings: vec![ArchitectFinding {
                description: "test finding".into(),
                category: "maintainability".into(),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: ArchitectReviewResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.verdict, Verdict::Concerns);
        assert_eq!(parsed.findings.len(), 1);
    }

    #[test]
    fn test_role() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ArchitectAgent::new("test".to_string(), task_agent);
        assert_eq!(agent.role().id, "architect");
    }
}
