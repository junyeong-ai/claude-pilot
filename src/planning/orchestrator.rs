use std::path::Path;

use tracing::{debug, info, warn};

use super::Evidence;
use super::artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
use super::plan_agent::PlanAgent;
use super::spec::SpecificationAgent;
use super::tasks::TaskDecomposer;
use super::validation::{PlanValidator, ValidationResult};
use crate::config::{AgentConfig, EvidenceConfig, QualityConfig, TaskDecompositionConfig};
use crate::error::Result;
use crate::mission::RiskLevel;
use crate::mission::{IsolationMode, Mission, Phase};

#[derive(Debug)]
pub struct PlanningResult {
    pub spec: SpecArtifact,
    pub plan: PlanArtifact,
    pub tasks: TasksArtifact,
    pub evidence: Evidence,
    pub validation: ValidationResult,
    pub recommended_isolation: IsolationMode,
}

pub struct PlanningOrchestrator {
    working_dir: std::path::PathBuf,
    executor: PlanAgent,
    spec_agent: SpecificationAgent,
    task_decomposer: TaskDecomposer,
    validator: PlanValidator,
    ai_validation_enabled: bool,
    evidence_config: EvidenceConfig,
    quality_config: QualityConfig,
}

impl PlanningOrchestrator {
    pub fn new(
        working_dir: &Path,
        ai_validation: bool,
        agent_config: &AgentConfig,
        quality_config: QualityConfig,
        evidence_config: EvidenceConfig,
        task_decomposition_config: TaskDecompositionConfig,
    ) -> Result<Self> {
        Ok(Self {
            working_dir: working_dir.to_path_buf(),
            executor: PlanAgent::new(working_dir, agent_config)?,
            spec_agent: SpecificationAgent::with_config(
                working_dir,
                agent_config,
                evidence_config.clone(),
                quality_config.clone(),
            )?,
            task_decomposer: TaskDecomposer::new(
                working_dir,
                agent_config,
                &task_decomposition_config,
            )?,
            validator: PlanValidator::with_config(
                working_dir,
                agent_config,
                quality_config.clone(),
            )?,
            ai_validation_enabled: ai_validation,
            evidence_config,
            quality_config,
        })
    }

    /// Plans mission based on gathered evidence.
    /// Evidence is required (NON-NEGOTIABLE: fact-based development).
    pub async fn plan(&self, description: &str, evidence: Evidence) -> Result<PlanningResult> {
        debug!(
            "Planning with pre-gathered evidence: {} relevant files",
            evidence.codebase_analysis.relevant_files.len()
        );

        // Tier-based quality assessment - Red tier fails, Yellow warns
        let assessment = evidence.get_quality_tier(&self.quality_config);
        match assessment.tier {
            super::validation::QualityTier::Red => {
                warn!(
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "RED",
                    "Evidence quality insufficient"
                );
                return Err(crate::error::PilotError::EvidenceGathering(format!(
                    "Evidence RED tier: {} - cannot proceed safely",
                    assessment.format_for_llm()
                )));
            }
            super::validation::QualityTier::Yellow => {
                warn!(
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "YELLOW",
                    "Proceeding with caution - evidence quality is moderate"
                );
            }
            super::validation::QualityTier::Green => {}
        }

        // Log incomplete evidence as context - LLM will be informed via format_for_llm()
        if let Some(context) = evidence.completeness.format_for_llm() {
            warn!(
                files_found = evidence.completeness.files_found,
                files_limit = evidence.completeness.files_limit,
                "{}",
                context
            );
        }

        let spec = self.spec_agent.create_spec(description, &evidence).await?;
        debug!("Created spec with {} user stories", spec.user_stories.len());

        let plan = self.create_plan(&spec, &evidence).await?;
        debug!("Created implementation plan");

        let tasks = self
            .task_decomposer
            .decompose(&spec, &plan, &evidence)
            .await?;
        debug!("Decomposed into {} tasks", tasks.total_tasks());

        let mut validation = self
            .validator
            .validate(&spec, &plan, &tasks, &evidence)
            .await?;

        if self.ai_validation_enabled && validation.passed {
            let ai_issues = self
                .validator
                .validate_with_ai(&spec, &plan, &tasks)
                .await?;
            if !ai_issues.is_empty() {
                warn!("AI validation found {} issues", ai_issues.len());
                for issue in &ai_issues {
                    validation.warnings.push(issue.clone());
                }
            }
        }

        let recommended_isolation = self.determine_isolation(&plan, &tasks, &evidence);

        info!(
            "Planning complete: {} tasks, {} risk, validation {}",
            tasks.total_tasks(),
            plan.risk_assessment.overall_risk,
            if validation.passed {
                "PASSED"
            } else {
                "FAILED"
            }
        );

        Ok(PlanningResult {
            spec,
            plan,
            tasks,
            evidence,
            validation,
            recommended_isolation,
        })
    }

    /// Creates a plan artifact from specification and evidence.
    /// Public to allow reuse when evidence is already gathered (e.g., by ChunkedPlanner).
    pub async fn create_plan(
        &self,
        spec: &SpecArtifact,
        evidence: &Evidence,
    ) -> Result<PlanArtifact> {
        let prompt = self.build_plan_prompt(spec, evidence);
        self.executor.run_structured::<PlanArtifact>(&prompt).await
    }

    // Component accessors for ChunkedPlanner to avoid duplication
    pub fn executor(&self) -> &PlanAgent {
        &self.executor
    }

    pub fn spec_agent(&self) -> &SpecificationAgent {
        &self.spec_agent
    }

    pub fn task_decomposer(&self) -> &TaskDecomposer {
        &self.task_decomposer
    }

    pub fn validator(&self) -> &PlanValidator {
        &self.validator
    }

    fn build_plan_prompt(&self, spec: &SpecArtifact, evidence: &Evidence) -> String {
        let budget = self.evidence_config.plan_evidence_budget;
        let evidence_md = if evidence.exceeds_budget(budget, &self.quality_config) {
            warn!(
                estimated = evidence.estimated_tokens(&self.quality_config),
                budget = budget,
                "Evidence exceeds plan budget, using truncated version"
            );
            evidence.to_markdown_with_budget(budget, &self.evidence_config, &self.quality_config)
        } else {
            evidence.to_markdown(&self.quality_config)
        };

        format!(
            r"## Implementation Planning

Create an implementation plan based on the specification.

**Specification**:
{}

**Evidence**:
{}

Requirements:
- Align with existing patterns from evidence
- Adhere to minimal scope
- Provide clear risk assessment
- Include practical rollback strategy

**Project Structure**:
- `affected_paths`: List existing files that will be modified
- `new_paths`: List NEW files that will be CREATED (important for validation)",
            spec.to_markdown(),
            evidence_md
        )
    }

    /// Determine isolation level based on plan and evidence.
    ///
    /// Primary factor: LLM-recommended isolation from plan (semantic understanding).
    /// Adjustment: Cross-package changes escalate isolation (monorepo safety).
    fn determine_isolation(
        &self,
        plan: &PlanArtifact,
        tasks: &TasksArtifact,
        evidence: &Evidence,
    ) -> IsolationMode {
        // Start with LLM's recommendation (semantic understanding of changes)
        let mut isolation = plan.risk_assessment.recommended_isolation;

        // Cross-package changes in monorepos warrant more isolation
        let cross_package = self.is_cross_package_change(evidence, tasks);
        if cross_package {
            isolation = isolation.escalate();
            debug!(
                "Escalated isolation to {:?} due to cross-package changes",
                isolation
            );
        }

        // High risk always gets maximum isolation
        if plan.risk_assessment.overall_risk == RiskLevel::High {
            isolation = IsolationMode::Worktree;
        }

        isolation
    }

    /// Detect if changes span multiple packages in a workspace.
    fn is_cross_package_change(&self, _evidence: &Evidence, tasks: &TasksArtifact) -> bool {
        use std::collections::HashSet;

        // Collect all affected file paths
        let affected_files: Vec<_> = tasks
            .phases
            .iter()
            .flat_map(|p| &p.tasks)
            .flat_map(|t| &t.affected_files)
            .collect();

        // Extract unique top-level directories (proxy for packages)
        let packages: HashSet<_> = affected_files
            .iter()
            .filter_map(|path| {
                let components: Vec<_> = path.split('/').collect();
                if components.len() > 1 {
                    Some(components[0].to_string())
                } else {
                    None
                }
            })
            .filter(|dir| !matches!(dir.as_str(), "src" | "lib" | "test" | "tests" | ".github"))
            .collect();

        packages.len() > 1
    }

    pub fn apply_to_mission(&self, mission: &mut Mission, result: &PlanningResult) {
        mission.tasks = self.task_decomposer.to_mission_tasks(&result.tasks);

        mission.phases = result
            .tasks
            .phases
            .iter()
            .map(|p| Phase {
                name: p.name.clone(),
                description: p.purpose.clone(),
                task_ids: p.tasks.iter().map(|t| t.id.clone()).collect(),
            })
            .collect();

        mission.isolation = result.recommended_isolation;
    }

    pub async fn save_artifacts(&self, mission_id: &str, result: &PlanningResult) -> Result<()> {
        let mission_dir = self
            .working_dir
            .join(".claude/pilot/missions")
            .join(mission_id);
        tokio::fs::create_dir_all(&mission_dir).await?;

        tokio::fs::write(mission_dir.join("spec.md"), result.spec.to_markdown()).await?;

        tokio::fs::write(mission_dir.join("plan.md"), result.plan.to_markdown()).await?;

        tokio::fs::write(mission_dir.join("tasks.md"), result.tasks.to_markdown()).await?;

        tokio::fs::write(
            mission_dir.join("research.md"),
            result.evidence.to_markdown(&self.quality_config),
        )
        .await?;

        tokio::fs::write(
            mission_dir.join("validation.md"),
            result.validation.to_summary(),
        )
        .await?;

        info!("Saved planning artifacts to {}", mission_dir.display());

        Ok(())
    }
}
