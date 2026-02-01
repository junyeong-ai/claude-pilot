use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mission::{Priority, RiskLevel};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SpecArtifact {
    pub user_stories: Vec<UserStory>,
    pub requirements: Vec<Requirement>,
    pub clarifications: Vec<Clarification>,
    pub success_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UserStory {
    pub id: String,
    pub title: String,
    pub priority: Priority,
    pub description: String,
    pub acceptance_scenarios: Vec<AcceptanceScenario>,
    pub independent_test: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AcceptanceScenario {
    pub given: String,
    pub when: String,
    pub then: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Requirement {
    pub id: String,
    pub description: String,
    pub needs_clarification: bool,
    pub clarification_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Clarification {
    pub id: String,
    pub question: String,
    pub context: String,
    pub options: Vec<ClarificationOption>,
    pub resolved: bool,
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ClarificationOption {
    pub label: String,
    pub description: String,
    pub recommended: bool,
}

impl SpecArtifact {
    pub fn has_clarifications(&self) -> bool {
        self.clarifications.iter().any(|c| !c.resolved)
    }

    pub fn pending_clarifications(&self) -> Vec<&Clarification> {
        self.clarifications.iter().filter(|c| !c.resolved).collect()
    }

    pub fn to_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("# Feature Specification\n\n");

        output.push_str("## User Stories\n\n");
        for story in &self.user_stories {
            output.push_str(&format!(
                "### {} - {} (Priority: {})\n\n",
                story.id, story.title, story.priority
            ));
            output.push_str(&format!("{}\n\n", story.description));
            output.push_str(&format!(
                "**Independent Test**: {}\n\n",
                story.independent_test
            ));
            output.push_str("**Acceptance Scenarios**:\n\n");
            for (i, scenario) in story.acceptance_scenarios.iter().enumerate() {
                output.push_str(&format!(
                    "{}. **Given** {}, **When** {}, **Then** {}\n",
                    i + 1,
                    scenario.given,
                    scenario.when,
                    scenario.then
                ));
            }
            output.push_str("\n---\n\n");
        }

        output.push_str("## Requirements\n\n");
        for req in &self.requirements {
            if req.needs_clarification {
                output.push_str(&format!(
                    "- **{}**: {} [NEEDS CLARIFICATION: {}]\n",
                    req.id,
                    req.description,
                    req.clarification_note
                        .as_deref()
                        .unwrap_or("Details needed")
                ));
            } else {
                output.push_str(&format!("- **{}**: {}\n", req.id, req.description));
            }
        }

        output.push_str("\n## Success Criteria\n\n");
        for (i, criteria) in self.success_criteria.iter().enumerate() {
            output.push_str(&format!("- **SC-{:03}**: {}\n", i + 1, criteria));
        }

        output
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanArtifact {
    pub summary: String,
    pub technical_context: TechnicalContext,
    pub project_structure: ProjectStructure,
    pub constitution_check: ConstitutionCheck,
    pub risk_assessment: RiskAssessment,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TechnicalContext {
    pub language: String,
    pub primary_dependencies: Vec<String>,
    pub storage: Option<String>,
    pub testing: String,
    pub target_platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectStructure {
    pub structure_type: StructureType,
    pub affected_paths: Vec<String>,
    pub new_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StructureType {
    SingleProject,
    WebApp,
    Mobile,
    Monorepo,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConstitutionCheck {
    pub fact_based: PrincipleCheckResult,
    pub code_consistency: PrincipleCheckResult,
    pub dry_principle: PrincipleCheckResult,
    pub work_verification: PrincipleCheckResult,
    pub minimal_scope: PrincipleCheckResult,
    pub intellectual_honesty: PrincipleCheckResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PrincipleCheckResult {
    pub passed: bool,
    pub note: Option<String>,
}

impl ConstitutionCheck {
    fn checks(&self) -> [(&'static str, &PrincipleCheckResult); 6] {
        [
            ("Fact-Based", &self.fact_based),
            ("Code Consistency", &self.code_consistency),
            ("DRY", &self.dry_principle),
            ("Work Verification", &self.work_verification),
            ("Minimal Scope", &self.minimal_scope),
            ("Intellectual Honesty", &self.intellectual_honesty),
        ]
    }

    pub fn issues(&self) -> Vec<String> {
        self.checks()
            .into_iter()
            .filter(|(_, r)| !r.passed)
            .map(|(name, r)| format!("{}: {}", name, r.note.as_deref().unwrap_or("Issue")))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub overall_risk: RiskLevel,
    pub factors: Vec<RiskFactor>,
    pub recommended_isolation: crate::mission::IsolationMode,
    pub rollback_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskFactor {
    pub factor: String,
    pub impact: RiskLevel,
    pub mitigation: String,
}

impl PlanArtifact {
    pub fn to_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("# Implementation Plan\n\n");

        output.push_str("## Summary\n\n");
        output.push_str(&format!("{}\n\n", self.summary));

        output.push_str("## Technical Context\n\n");
        output.push_str(&format!(
            "- **Language**: {}\n",
            self.technical_context.language
        ));
        output.push_str(&format!(
            "- **Dependencies**: {}\n",
            self.technical_context.primary_dependencies.join(", ")
        ));
        if let Some(ref storage) = self.technical_context.storage {
            output.push_str(&format!("- **Storage**: {}\n", storage));
        }
        output.push_str(&format!(
            "- **Testing**: {}\n",
            self.technical_context.testing
        ));
        output.push_str(&format!(
            "- **Platform**: {}\n\n",
            self.technical_context.target_platform
        ));

        output.push_str("## Constitution Check\n\n");
        let checks = [
            (
                "Fact-Based Development",
                &self.constitution_check.fact_based,
            ),
            (
                "Code Consistency",
                &self.constitution_check.code_consistency,
            ),
            ("DRY Principle", &self.constitution_check.dry_principle),
            (
                "Work Verification",
                &self.constitution_check.work_verification,
            ),
            ("Minimal Scope", &self.constitution_check.minimal_scope),
            (
                "Intellectual Honesty",
                &self.constitution_check.intellectual_honesty,
            ),
        ];
        for (name, check) in checks {
            let status = if check.passed { "PASS" } else { "FAIL" };
            let note = check.note.as_deref().unwrap_or("");
            output.push_str(&format!("- {} {} {}\n", status, name, note));
        }

        output.push_str("\n## Risk Assessment\n\n");
        output.push_str(&format!(
            "**Overall Risk**: {}\n\n",
            self.risk_assessment.overall_risk
        ));
        for factor in &self.risk_assessment.factors {
            output.push_str(&format!(
                "- **{}** ({}): {}\n",
                factor.factor, factor.impact, factor.mitigation
            ));
        }
        output.push_str(&format!(
            "\n**Rollback Strategy**: {}\n",
            self.risk_assessment.rollback_strategy
        ));

        output
    }
}

/// Task decomposition artifact with JSON Schema support for structured output.
///
/// This type is used with Claude's structured output feature to guarantee
/// valid JSON responses during task decomposition. The schema is automatically
/// generated via schemars and enforced at the API level.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TasksArtifact {
    /// Implementation phases, ordered by execution sequence.
    /// Phase 1: Setup (no dependencies)
    /// Phase 2: Foundational (blocks all user stories)
    /// Phase 3+: One phase per user story
    /// Final Phase: Polish & Cross-Cutting
    pub phases: Vec<TaskPhase>,
}

/// A phase groups related tasks for checkpoint-based verification.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TaskPhase {
    /// Unique phase identifier (e.g., "Phase1", "Phase2")
    pub id: String,
    /// Human-readable phase name (e.g., "Setup", "User Story 1 - Auth")
    pub name: String,
    /// Brief description of what this phase accomplishes
    #[serde(default)]
    pub purpose: String,
    /// Tasks within this phase
    pub tasks: Vec<PlannedTask>,
    /// Verification checkpoint for this phase (optional)
    #[serde(default)]
    pub checkpoint: Option<String>,
}

/// A single planned task with execution metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlannedTask {
    /// Unique task identifier (e.g., "T001", "T002")
    pub id: String,
    /// Specific, actionable task description
    pub description: String,
    /// Associated user story ID (e.g., "US1")
    #[serde(default)]
    pub user_story: Option<String>,
    /// Whether this task can run in parallel with others
    #[serde(default)]
    pub parallel: bool,
    /// Task IDs that must complete before this task
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Files that will be created or modified
    #[serde(default)]
    pub affected_files: Vec<String>,
    /// Test patterns to verify this task
    #[serde(default)]
    pub test_patterns: Vec<String>,
}

impl TasksArtifact {
    pub fn total_tasks(&self) -> usize {
        self.phases.iter().map(|p| p.tasks.len()).sum()
    }

    pub fn parallel_tasks(&self) -> usize {
        self.phases
            .iter()
            .flat_map(|p| &p.tasks)
            .filter(|t| t.parallel)
            .count()
    }

    pub fn to_dependency_map(&self) -> HashMap<String, Vec<String>> {
        self.phases
            .iter()
            .flat_map(|p| &p.tasks)
            .map(|t| (t.id.clone(), t.dependencies.clone()))
            .collect()
    }

    /// Normalizes task IDs to a consistent P{phase}T{task} format.
    /// Returns a list of dropped dependencies (dependencies that referenced non-existent task IDs).
    /// These should be investigated as potential LLM errors or typos.
    pub fn normalize_task_ids(&mut self) -> Vec<DroppedDependency> {
        let mut id_map: HashMap<String, String> = HashMap::new();
        let mut dropped = Vec::new();

        for (pi, phase) in self.phases.iter_mut().enumerate() {
            for (ti, task) in phase.tasks.iter_mut().enumerate() {
                let old_id = task.id.clone();
                let new_id = format!("P{}T{}", pi + 1, ti + 1);
                id_map.insert(old_id, new_id.clone());
                task.id = new_id;
            }
        }

        for phase in &mut self.phases {
            for task in &mut phase.tasks {
                let mut valid_deps = Vec::new();
                for dep in &task.dependencies {
                    if let Some(new_id) = id_map.get(dep) {
                        valid_deps.push(new_id.clone());
                    } else {
                        dropped.push(DroppedDependency {
                            task_id: task.id.clone(),
                            invalid_dep: dep.clone(),
                        });
                    }
                }
                task.dependencies = valid_deps;
            }
        }

        dropped
    }

    pub fn to_markdown(&self) -> String {
        let mut output = String::new();
        output.push_str("# Tasks\n\n");

        for phase in &self.phases {
            output.push_str(&format!("## {} - {}\n\n", phase.id, phase.name));
            output.push_str(&format!("**Purpose**: {}\n\n", phase.purpose));

            for task in &phase.tasks {
                let parallel_mark = if task.parallel { "[P]" } else { "" };
                let story_mark = task
                    .user_story
                    .as_ref()
                    .map(|s| format!("[{}]", s))
                    .unwrap_or_default();

                output.push_str(&format!(
                    "- [ ] {} {} {} {}\n",
                    task.id, parallel_mark, story_mark, task.description
                ));
            }

            if let Some(ref checkpoint) = phase.checkpoint {
                output.push_str(&format!("\n**Checkpoint**: {}\n", checkpoint));
            }

            output.push_str("\n---\n\n");
        }

        output
    }
}

/// Dependency dropped during task ID normalization due to referencing non-existent task.
#[derive(Debug, Clone)]
pub struct DroppedDependency {
    pub task_id: String,
    pub invalid_dep: String,
}
