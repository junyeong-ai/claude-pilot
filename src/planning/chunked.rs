use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::Evidence;
use super::PlanningOrchestrator;
use super::artifacts::{SpecArtifact, TasksArtifact};
use super::orchestrator::PlanningResult;
use crate::config::{
    AgentConfig, ChunkedPlanningConfig, EvidenceConfig, QualityConfig, TaskDecompositionConfig,
};
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MissionOutline {
    #[serde(default)]
    pub phases: Vec<OutlinePhase>,
    #[serde(default)]
    pub total_estimated_stories: usize,
    #[serde(default)]
    pub dependencies: Vec<PhaseDependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OutlinePhase {
    pub name: String,
    pub description: String,
    #[serde(default = "default_stories")]
    pub estimated_stories: usize,
    #[serde(default)]
    pub key_features: Vec<String>,
}

fn default_stories() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PhaseDependency {
    pub phase: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Chunked planner that breaks large missions into phases.
/// Delegates all component access to PlanningOrchestrator to eliminate duplication.
pub struct ChunkedPlanner {
    orchestrator: PlanningOrchestrator,
    config: ChunkedPlanningConfig,
}

impl ChunkedPlanner {
    pub fn new(
        working_dir: &Path,
        agent_config: AgentConfig,
        quality_config: QualityConfig,
        evidence_config: EvidenceConfig,
        task_decomposition_config: TaskDecompositionConfig,
        chunked_config: ChunkedPlanningConfig,
    ) -> Result<Self> {
        Ok(Self {
            orchestrator: PlanningOrchestrator::new(
                working_dir,
                true,
                &agent_config,
                quality_config,
                evidence_config,
                task_decomposition_config,
            )?,
            config: chunked_config,
        })
    }

    /// Plans mission based on gathered evidence using chunked approach.
    /// Evidence is required (NON-NEGOTIABLE: fact-based development).
    pub async fn plan(&self, description: &str, evidence: Evidence) -> Result<PlanningResult> {
        debug!(
            description = %description,
            files = evidence.codebase_analysis.relevant_files.len(),
            "Chunked planning with pre-gathered evidence"
        );

        let outline = self.generate_outline(description, &evidence).await?;
        info!(
            phases = outline.phases.len(),
            total_stories = outline.total_estimated_stories,
            "Generated mission outline"
        );

        let mut all_specs = Vec::new();
        let mut all_tasks = TasksArtifact { phases: Vec::new() };

        for phase in &outline.phases {
            debug!(phase = phase.name, "Processing phase");

            let spec = self.generate_phase_spec(phase, &evidence).await?;
            all_specs.push(spec.clone());

            let tasks = self.decompose_phase_tasks(&spec, &evidence).await?;
            for task_phase in tasks.phases {
                all_tasks.phases.push(task_phase);
            }
        }

        self.validate_dependencies(&all_tasks)?;

        let merged_spec = self.merge_specs(&all_specs);
        let plan = self
            .orchestrator
            .create_plan(&merged_spec, &evidence)
            .await?;

        let validation = self
            .orchestrator
            .validator()
            .validate(&merged_spec, &plan, &all_tasks, &evidence)
            .await?;

        Ok(PlanningResult {
            spec: merged_spec,
            plan,
            tasks: all_tasks,
            evidence,
            validation,
            recommended_isolation: crate::mission::IsolationMode::Branch,
        })
    }

    async fn generate_outline(
        &self,
        description: &str,
        evidence: &Evidence,
    ) -> Result<MissionOutline> {
        let prompt = format!(
            r"## Mission Outline Generation

Create a high-level outline for this mission.

**Mission**: {}

**Codebase Context**:
- Relevant files: {}
- Current dependencies: {}

Requirements:
1. Break the mission into {}-{} logical phases
2. Each phase should have {} user stories max
3. Define dependencies between phases
4. Enable incremental delivery",
            description,
            evidence.codebase_analysis.relevant_files.len(),
            evidence
                .dependency_analysis
                .current_dependencies
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            self.config.min_phases,
            self.config.max_phases,
            self.config.max_stories_per_phase,
        );

        let mut outline: MissionOutline =
            self.orchestrator.executor().run_structured(&prompt).await?;
        outline.total_estimated_stories = outline.phases.iter().map(|p| p.estimated_stories).sum();
        Ok(outline)
    }

    async fn generate_phase_spec(
        &self,
        phase: &OutlinePhase,
        evidence: &Evidence,
    ) -> Result<SpecArtifact> {
        let description = format!(
            "{}\\n\\nKey features:\\n{}",
            phase.description,
            phase
                .key_features
                .iter()
                .map(|f| format!("- {}", f))
                .collect::<Vec<_>>()
                .join("\\n")
        );

        let mut phase_evidence = evidence.clone();
        phase_evidence
            .prior_knowledge
            .similar_patterns
            .insert(0, format!("This is phase: {}", phase.name));

        self.orchestrator
            .spec_agent()
            .create_spec(&description, &phase_evidence)
            .await
    }

    async fn decompose_phase_tasks(
        &self,
        spec: &SpecArtifact,
        evidence: &Evidence,
    ) -> Result<TasksArtifact> {
        let plan = self.orchestrator.create_plan(spec, evidence).await?;
        self.orchestrator
            .task_decomposer()
            .decompose(spec, &plan, evidence)
            .await
    }

    fn validate_dependencies(&self, tasks: &TasksArtifact) -> Result<()> {
        let all_task_ids: Vec<&str> = tasks
            .phases
            .iter()
            .flat_map(|p| &p.tasks)
            .map(|t| t.id.as_str())
            .collect();

        for phase in &tasks.phases {
            for task in &phase.tasks {
                for dep in &task.dependencies {
                    if !all_task_ids.contains(&dep.as_str()) {
                        warn!(
                            task = task.id,
                            missing_dep = dep,
                            "Task has missing dependency"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn merge_specs(&self, specs: &[SpecArtifact]) -> SpecArtifact {
        let mut merged = SpecArtifact {
            user_stories: Vec::new(),
            requirements: Vec::new(),
            clarifications: Vec::new(),
            success_criteria: Vec::new(),
        };

        for spec in specs {
            merged.user_stories.extend(spec.user_stories.clone());
            merged.requirements.extend(spec.requirements.clone());
            merged.clarifications.extend(spec.clarifications.clone());
            merged
                .success_criteria
                .extend(spec.success_criteria.clone());
        }

        merged
    }
}
