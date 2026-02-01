use std::path::Path;

use tracing::{debug, info, warn};

use super::Evidence;
use super::artifacts::{
    AcceptanceScenario, Clarification, ClarificationOption, Requirement, SpecArtifact, UserStory,
};
use super::plan_agent::PlanAgent;
use crate::config::{AgentConfig, EvidenceConfig, QualityConfig};
use crate::error::Result;
use crate::mission::Priority;

pub struct SpecificationAgent {
    executor: PlanAgent,
    evidence_config: EvidenceConfig,
    quality_config: QualityConfig,
}

impl SpecificationAgent {
    pub fn with_config(
        working_dir: &Path,
        agent_config: &AgentConfig,
        evidence_config: EvidenceConfig,
        quality_config: QualityConfig,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            executor: PlanAgent::new(working_dir, agent_config)?,
            evidence_config,
            quality_config,
        })
    }

    pub async fn create_spec(
        &self,
        description: &str,
        evidence: &Evidence,
    ) -> Result<SpecArtifact> {
        info!("Creating specification for: {}", description);

        let prompt = self.build_spec_prompt(description, evidence);

        match self.executor.run_structured::<SpecArtifact>(&prompt).await {
            Ok(spec) => {
                debug!(
                    "Created spec with {} user stories, {} requirements",
                    spec.user_stories.len(),
                    spec.requirements.len()
                );
                Ok(spec)
            }
            Err(e) => {
                warn!(error = %e, "Structured spec generation failed, using default");
                Ok(self.create_default_spec(description))
            }
        }
    }

    fn build_spec_prompt(&self, description: &str, evidence: &Evidence) -> String {
        let budget = self.evidence_config.spec_evidence_budget;
        let evidence_md = if evidence.exceeds_budget(budget, &self.quality_config) {
            warn!(
                estimated = evidence.estimated_tokens(&self.quality_config),
                budget = budget,
                "Evidence exceeds budget, using truncated version"
            );
            evidence.to_markdown_with_budget(budget, &self.evidence_config, &self.quality_config)
        } else {
            evidence.to_markdown(&self.quality_config)
        };

        format!(
            r"## Specification Creation Task

Create a feature specification for the following request.

**User Request**: {}

**Evidence from Codebase Analysis**:
{}

---

Create a specification with:

1. **User Stories** (id, title, priority p1/p2/p3/p4, description, acceptance scenarios, independent test)
   - p1 = MVP, p2 = important, p3+ = nice to have
   - Each story must be independently testable

2. **Requirements** (id, description, needs_clarification flag, optional clarification_note)
   - Mark ambiguous requirements with needs_clarification: true

3. **Clarifications** (id, question, context, options with label/description/recommended)
   - Include specific questions for unclear aspects
   - Each option should have a recommended flag

4. **Success Criteria** (list of measurable outcomes)

Follow the evidence to align with existing patterns in the codebase.",
            description, evidence_md
        )
    }

    fn create_default_spec(&self, description: &str) -> SpecArtifact {
        SpecArtifact {
            user_stories: vec![UserStory {
                id: "US1".to_string(),
                title: description.chars().take(50).collect(),
                priority: Priority::P1,
                description: description.to_string(),
                acceptance_scenarios: vec![AcceptanceScenario {
                    given: "the system is ready".to_string(),
                    when: "the feature is used".to_string(),
                    then: "the expected behavior occurs".to_string(),
                }],
                independent_test: "Manual verification of the feature".to_string(),
            }],
            requirements: vec![Requirement {
                id: "FR-001".to_string(),
                description: format!("System MUST {}", description),
                needs_clarification: true,
                clarification_note: Some("Details need to be specified".to_string()),
            }],
            clarifications: vec![Clarification {
                id: "C1".to_string(),
                question: "Please provide more details about the requirements".to_string(),
                context: "The initial description may be incomplete".to_string(),
                options: vec![
                    ClarificationOption {
                        label: "Minimal implementation".to_string(),
                        description: "Basic functionality only".to_string(),
                        recommended: true,
                    },
                    ClarificationOption {
                        label: "Full implementation".to_string(),
                        description: "Complete feature set".to_string(),
                        recommended: false,
                    },
                ],
                resolved: false,
                answer: None,
            }],
            success_criteria: vec!["Feature works as expected".to_string()],
        }
    }

    pub fn resolve_clarification(
        spec: &mut SpecArtifact,
        clarification_id: &str,
        answer: &str,
    ) -> bool {
        for clarification in &mut spec.clarifications {
            if clarification.id == clarification_id {
                clarification.resolved = true;
                clarification.answer = Some(answer.to_string());
                return true;
            }
        }
        false
    }
}
