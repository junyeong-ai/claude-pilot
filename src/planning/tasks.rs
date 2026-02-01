use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::config::{AgentConfig, TaskDecompositionConfig};
use crate::error::{PilotError, Result};
use crate::mission::{AgentType, Task};

use super::Evidence;
use super::artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
use super::plan_agent::PlanAgent;
use super::task_quality::{TaskQualityIssue, TaskQualityResult, TaskQualityValidator};

pub struct TaskDecomposer {
    executor: PlanAgent,
    quality_config: TaskDecompositionConfig,
    working_dir: PathBuf,
}

impl TaskDecomposer {
    pub fn new(
        working_dir: &Path,
        agent_config: &AgentConfig,
        quality_config: &TaskDecompositionConfig,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            executor: PlanAgent::new(working_dir, agent_config)?,
            quality_config: quality_config.clone(),
            working_dir: working_dir.to_path_buf(),
        })
    }

    pub async fn decompose(
        &self,
        spec: &SpecArtifact,
        plan: &PlanArtifact,
        evidence: &Evidence,
    ) -> Result<TasksArtifact> {
        info!("Decomposing tasks from spec and plan");

        let validator =
            TaskQualityValidator::new(&self.quality_config).with_working_dir(&self.working_dir);
        let story_ids: Vec<String> = spec.user_stories.iter().map(|s| s.id.clone()).collect();
        let mut last_result: Option<TaskQualityResult> = None;

        for attempt in 1..=self.quality_config.max_retry_attempts {
            let feedback = last_result.as_ref().map(|r| r.format_feedback());
            let prompt = self.build_prompt(spec, plan, evidence, feedback.as_deref());

            match self.executor.run_structured::<TasksArtifact>(&prompt).await {
                Ok(mut tasks) => {
                    let dropped = tasks.normalize_task_ids();
                    if !dropped.is_empty() {
                        // Treat invalid dependencies as validation failure to prevent silent errors
                        let invalid_deps: Vec<String> = dropped
                            .iter()
                            .map(|d| format!("{}→{}", d.task_id, d.invalid_dep))
                            .collect();
                        warn!(
                            count = dropped.len(),
                            invalid = %invalid_deps.join(", "),
                            "Invalid dependencies detected - will trigger retry"
                        );
                        last_result = Some(TaskQualityResult {
                            passed: false,
                            score: 0.0,
                            issues: dropped
                                .into_iter()
                                .map(|d| TaskQualityIssue::MissingDependency {
                                    task_id: d.task_id,
                                    missing: d.invalid_dep,
                                })
                                .collect(),
                        });
                        continue;
                    }

                    debug!(
                        attempt,
                        phases = tasks.phases.len(),
                        tasks = tasks.total_tasks(),
                        "Received structured output"
                    );

                    let all_tasks: Vec<_> = tasks
                        .phases
                        .iter()
                        .flat_map(|p| &p.tasks)
                        .cloned()
                        .collect();
                    let result = validator.validate(&all_tasks, &story_ids);

                    if result.passed {
                        info!(
                            attempt,
                            score = result.score,
                            phases = tasks.phases.len(),
                            tasks = tasks.total_tasks(),
                            "Task decomposition passed quality validation"
                        );
                        return Ok(tasks);
                    }

                    warn!(
                        attempt,
                        score = result.score,
                        issues = result.issues.len(),
                        "Task decomposition failed validation, retrying"
                    );
                    last_result = Some(result);
                }
                Err(e) => {
                    warn!(attempt, error = %e, "Task decomposition failed, will retry");
                    last_result = Some(TaskQualityResult {
                        passed: false,
                        score: 0.0,
                        issues: vec![],
                    });
                }
            }
        }

        let issues_summary = last_result
            .map(|r| {
                r.issues
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_else(|| "Unknown error".to_string());

        Err(PilotError::TaskDecomposition(format!(
            "Failed after {} attempts. Issues: {}",
            self.quality_config.max_retry_attempts, issues_summary
        )))
    }

    fn build_prompt(
        &self,
        spec: &SpecArtifact,
        plan: &PlanArtifact,
        evidence: &Evidence,
        feedback: Option<&str>,
    ) -> String {
        let evidence_files: Vec<&str> = evidence
            .codebase_analysis
            .relevant_files
            .iter()
            .map(|f| f.path.as_str())
            .collect();

        let evidence_files_str = if evidence_files.is_empty() {
            "No specific files (use plan paths)".to_string()
        } else {
            evidence_files.join(", ")
        };

        let plan_affected = plan.project_structure.affected_paths.join(", ");
        let plan_new = plan.project_structure.new_paths.join(", ");

        // Build test pattern guidance from detected test structure
        let test_guidance = self.build_test_pattern_guidance(evidence);

        let mut prompt = format!(
            r#"## Task Decomposition

Break down the implementation into phases and tasks.

**Specification**:
{}

**Implementation Plan**:
{}

**Evidence-Backed Files**: {}

**Plan Affected Paths** (existing): {}

**Plan New Paths** (to create): {}

**Existing Patterns**: {}
{}
---

## Requirements

1. **Phases**: Group related tasks (id, name, purpose, checkpoint)
2. **Tasks**: Appropriate number per phase based on natural problem decomposition:
   - id: Unique task identifier (e.g., T001, T002)
   - description: Clear description of what to implement
   - user_story: REQUIRED - Just the user story ID (e.g., "US1", "US-001"). NOT the full description, only the ID part.
   - parallel: Whether this task can run in parallel with others
   - dependencies: List of task IDs this depends on
   - affected_files: Use paths from evidence, affected paths, OR new paths - do NOT invent paths
   - test_patterns: Patterns to run related tests
3. **User Story Coverage**: Every user story ID listed above MUST appear in at least one task's user_story field. Match the ID exactly.
4. **Dependencies**: Reference other task IDs (e.g., T001, T002)
5. **Output Size**: Keep descriptions concise to avoid exceeding output limits"#,
            spec.to_markdown(),
            plan.to_markdown(),
            evidence_files_str,
            plan_affected,
            plan_new,
            evidence.codebase_analysis.existing_patterns.join(", "),
            test_guidance
        );

        if let Some(fb) = feedback {
            prompt.push_str(&format!(
                "\n\n## Quality Feedback\n\n{}\n\nFix these issues:",
                fb
            ));
        }

        prompt
    }

    /// Build test pattern guidance from detected test structure.
    fn build_test_pattern_guidance(&self, evidence: &Evidence) -> String {
        let test_struct = &evidence.codebase_analysis.test_structure;

        if test_struct.file_patterns.is_empty() && test_struct.module_test_mapping.is_empty() {
            return String::new();
        }

        let mut guidance = String::from(
            "\n\n**Test Pattern Examples** (use these patterns for test_patterns field):\n",
        );

        if let Some(framework) = &test_struct.framework {
            guidance.push_str(&format!("- Framework: {}\n", framework));
        }

        if !test_struct.file_patterns.is_empty() {
            guidance.push_str(&format!(
                "- File patterns: {}\n",
                test_struct.file_patterns.join(", ")
            ));
        }

        if !test_struct.test_directories.is_empty() {
            guidance.push_str(&format!(
                "- Test directories: {}\n",
                test_struct.test_directories.join(", ")
            ));
        }

        // Provide concrete mapping examples
        for mapping in test_struct.module_test_mapping.iter().take(3) {
            guidance.push_str(&format!(
                "- {} → {}\n",
                mapping.source_file,
                mapping.test_files.join(", ")
            ));
        }

        guidance
    }

    pub fn to_mission_tasks(&self, artifact: &TasksArtifact) -> Vec<Task> {
        artifact
            .phases
            .iter()
            .flat_map(|phase| {
                phase.tasks.iter().map(|t| {
                    // Trust LLM-provided test_patterns. The prompt includes detected test structure
                    // with framework, file patterns, and concrete source→test mappings.
                    // If LLM returns empty, accept that as valid (no tests or LLM determined
                    // patterns from evidence). Language-specific inference was removed because:
                    // 1. It only covered 5 languages (Rust, JS/TS, Python, Go, Java/Kotlin)
                    // 2. Test conventions vary by framework, not just language
                    // 3. LLM already has test structure context to make informed decisions
                    Task::new(&t.id, &t.description)
                        .with_phase(phase.name.clone())
                        .with_dependencies(t.dependencies.clone())
                        .with_agent_type(AgentType::Coder)
                        .with_affected_files(t.affected_files.clone())
                        .with_test_patterns(t.test_patterns.clone())
                })
            })
            .collect()
    }
}
