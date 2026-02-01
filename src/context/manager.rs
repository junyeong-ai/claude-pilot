use std::path::Path;

use tracing::info;

use crate::utils::estimate_tokens;

use super::budget::ActualUsage;
use super::compaction::{CompactionTrigger, ContextCompactor};
use super::persistence::ContextPersistence;
use super::types::{MissionContext, PhaseStatus, PhaseSummary, TaskEntry};
use crate::config::{ContextConfig, DisplayConfig, ModelConfig};
use crate::error::Result;
use crate::mission::{Mission, Task};
use crate::planning::Evidence;

pub struct ContextManager {
    persistence: ContextPersistence,
    compactor: ContextCompactor,
    context_config: ContextConfig,
    display_config: DisplayConfig,
}

impl ContextManager {
    pub fn new(
        missions_dir: impl AsRef<Path>,
        context_config: ContextConfig,
        display_config: DisplayConfig,
    ) -> Self {
        Self {
            persistence: ContextPersistence::new(missions_dir),
            compactor: ContextCompactor::new(context_config.compaction.clone()),
            context_config,
            display_config,
        }
    }

    /// Load or initialize mission context.
    /// Use `load_or_init_with_model_config` when you have a ModelConfig to configure the token budget.
    pub async fn load_or_init(&self, mission: &Mission) -> Result<MissionContext> {
        self.load_or_init_internal(mission, None).await
    }

    /// Load or initialize mission context with model-specific token budget configuration.
    pub async fn load_or_init_with_model_config(
        &self,
        mission: &Mission,
        model_config: &ModelConfig,
    ) -> Result<MissionContext> {
        self.load_or_init_internal(mission, Some(model_config))
            .await
    }

    /// Internal implementation for context loading with optional model config.
    async fn load_or_init_internal(
        &self,
        mission: &Mission,
        model_config: Option<&ModelConfig>,
    ) -> Result<MissionContext> {
        let mut context = self.persistence.load_or_init(&mission.id).await?;

        if context.summary.objective.is_empty() {
            context
                .summary
                .set_objective(&mission.description, &self.context_config.limits);
        }

        if let Some(model_config) = model_config {
            let default_budget = super::TokenBudget::default();
            if context.token_budget.max_total == default_budget.max_total {
                context
                    .configure_budget(model_config.usable_context() as usize, &self.context_config);
            }
        }

        Ok(context)
    }

    pub async fn save(&self, context: &MissionContext) -> Result<()> {
        self.persistence.save(context).await
    }

    pub fn needs_compaction(&self, context: &MissionContext) -> Option<CompactionTrigger> {
        self.compactor.needs_compaction(context)
    }

    pub async fn compact(&self, context: &mut MissionContext) -> Result<()> {
        let result = self.compactor.compact(context).await?;
        self.persistence
            .save_compaction_log(&context.mission_id, &result)
            .await?;
        self.save(context).await?;
        Ok(())
    }

    pub async fn emergency_compact(&mut self, context: &mut MissionContext) -> Result<()> {
        self.compactor.emergency_compact(context).await?;
        self.save(context).await?;
        Ok(())
    }

    pub fn update_from_mission(&self, context: &mut MissionContext, mission: &Mission) {
        context.summary.progress = mission.progress();

        if let Some(current_phase) = mission.phases.last() {
            context.summary.current_phase = current_phase.name.clone();
        }

        self.sync_task_context_data(context, mission);
        context.update_timestamp();
    }

    /// Syncs context-only data from mission tasks.
    /// Note: This only syncs files_changed, NOT status/retry_count (those live only in Mission).
    fn sync_task_context_data(&self, context: &mut MissionContext, mission: &Mission) {
        for task in &mission.tasks {
            // Sync all terminal task states, not just Completed.
            // Skipped tasks may still have file changes from partial execution.
            // Failed tasks that modified files before failing should be tracked.
            if task.status.is_terminal()
                && let Some(ref result) = task.result
                && let Some(entry) = context.task_registry.task_mut(&task.id)
            {
                // Only sync context-only data (files affected)
                if entry.files_changed.is_empty() {
                    entry.files_changed = result.files_modified.clone();
                    entry.files_changed.extend(result.files_created.clone());
                }
            }
        }
    }

    pub fn initialize_from_planning(
        &self,
        context: &mut MissionContext,
        mission: &Mission,
        evidence: &Evidence,
    ) {
        context
            .summary
            .set_objective(&mission.description, &self.context_config.limits);
        context.summary.progress = mission.progress();

        for phase in &mission.phases {
            let phase_tasks: Vec<&Task> = mission
                .tasks
                .iter()
                .filter(|t| t.phase.as_ref() == Some(&phase.name))
                .collect();

            let mut phase_summary = PhaseSummary::new(&phase.name, &phase.name);
            phase_summary.one_line_summary = phase.description.clone();
            phase_summary.task_count = phase_tasks.len();
            phase_summary.status = PhaseStatus::Pending;

            context.phase_summaries.push(phase_summary);

            for task in phase_tasks {
                let entry = TaskEntry::new(&task.id, &phase.name, &task.description)
                    .with_dependencies(task.dependencies.clone());
                context.task_registry.add_task(entry);
            }
        }

        context.task_registry.compute_execution_order();

        for pattern in &evidence.prior_knowledge.similar_patterns {
            context
                .summary
                .add_learning(pattern, &self.context_config.limits);
        }

        // Store evidence for checkpoint persistence during execution
        context.set_evidence(evidence.clone());

        context.update_timestamp();
    }

    pub fn start_phase(&self, context: &mut MissionContext, phase_id: &str) {
        let dependency_depth = self.compute_phase_dependency_depth(context, phase_id);

        let (task_count, files_touched, phase_name) = context
            .phase_summaries
            .iter()
            .find(|p| p.phase_id == phase_id)
            .map(|p| (p.task_count, p.files_modified.len(), p.name.clone()))
            .unwrap_or((0, 0, String::new()));

        for phase in &mut context.phase_summaries {
            if phase.phase_id == phase_id {
                phase.status = PhaseStatus::InProgress;
                break;
            }
        }

        context.summary.current_phase = phase_name;
        context.token_budget.update_complexity_from_phase(
            task_count,
            dependency_depth,
            files_touched,
        );
        context.update_timestamp();
    }

    fn compute_phase_dependency_depth(&self, context: &MissionContext, phase_id: &str) -> usize {
        let phase_tasks: Vec<_> = context
            .task_registry
            .tasks
            .iter()
            .filter(|t| t.phase == phase_id)
            .collect();

        let mut max_depth = 0;
        for task in &phase_tasks {
            let depth = self.count_dependency_chain(context, &task.id, 0);
            max_depth = max_depth.max(depth);
        }
        max_depth
    }

    fn count_dependency_chain(
        &self,
        context: &MissionContext,
        task_id: &str,
        depth: usize,
    ) -> usize {
        if depth > 10 {
            return depth;
        }

        if let Some(task) = context.task_registry.task(task_id) {
            if task.dependencies.is_empty() {
                return depth;
            }
            task.dependencies
                .iter()
                .map(|dep| self.count_dependency_chain(context, dep, depth + 1))
                .max()
                .unwrap_or(depth)
        } else {
            depth
        }
    }

    pub fn complete_phase(&self, context: &mut MissionContext, phase_id: &str) {
        for phase in &mut context.phase_summaries {
            if phase.phase_id == phase_id {
                phase.status = PhaseStatus::Completed;
                phase.completed_count = phase.task_count;
                phase.verification_passed = true;
                break;
            }
        }
        context.update_timestamp();
    }

    /// Records task start in context. Note: actual status is managed in Mission.tasks.
    pub fn start_task(&self, context: &mut MissionContext, _task_id: &str) {
        // Status is now managed solely in Mission.tasks
        // This method just updates timestamp for context tracking
        context.update_timestamp();
    }

    /// Records task completion context data (summary, files, verification).
    /// Note: actual status is managed in Mission.tasks.
    pub fn complete_task(
        &self,
        context: &mut MissionContext,
        task_id: &str,
        summary: Option<String>,
        files: Vec<String>,
    ) {
        let files_for_phase = files.clone();

        if let Some(entry) = context.task_registry.task_mut(task_id) {
            if let Some(s) = summary {
                entry.set_summary(s);
            }
            for file in files {
                entry.add_file(file);
            }
            entry.verification_status = super::types::VerificationStatus::Passed;
        }

        // Update phase completion count and files
        if let Some(entry) = context.task_registry.task(task_id) {
            let phase_id = entry.phase.clone();
            let task_desc = entry.one_line.clone();
            if let Some(phase) = context
                .phase_summaries
                .iter_mut()
                .find(|p| p.phase_id == phase_id)
            {
                phase.completed_count += 1;

                // Track files modified in this phase
                for file in &files_for_phase {
                    if phase.files_modified.len() < self.context_config.limits.max_phase_files
                        && !phase.files_modified.contains(file)
                    {
                        phase.files_modified.push(file.clone());
                    }
                }

                // Add key change if files were modified
                if !files_for_phase.is_empty() {
                    phase.add_change(
                        format!("{}: {} files", task_desc, files_for_phase.len()),
                        &self.context_config.limits,
                    );
                }

                // Check if phase is complete
                if phase.completed_count >= phase.task_count {
                    phase.status = super::types::PhaseStatus::Completed;
                }
            }
        }

        context.update_timestamp();
    }

    /// Records task failure context data (error message, verification status).
    /// Note: actual status/retry_count is managed in Mission.tasks.
    pub fn fail_task(&self, context: &mut MissionContext, task_id: &str, error: &str) {
        if let Some(entry) = context.task_registry.task_mut(task_id) {
            entry.last_error = Some(error.to_string());
            entry.verification_status = super::types::VerificationStatus::Failed;
        }
        context
            .summary
            .add_blocker(format!("Task {} failed: {}", task_id, error));
        context.update_timestamp();
    }

    /// Upgrade token budget complexity if failure rate exceeds threshold.
    ///
    /// Call this after task failures to give retries more context budget.
    /// Returns true if complexity was upgraded.
    pub fn upgrade_budget_on_failures(
        &self,
        context: &mut MissionContext,
        retry_count: u32,
        task_count: u32,
    ) -> bool {
        const FAILURE_RATE_THRESHOLD: f32 = 0.5;

        if context.token_budget.upgrade_on_high_failure_rate(
            retry_count,
            task_count,
            FAILURE_RATE_THRESHOLD,
        ) {
            info!(
                retry_count,
                task_count,
                new_complexity = %context.token_budget.phase_complexity,
                "Budget complexity upgraded due to high failure rate"
            );
            context.update_timestamp();
            true
        } else {
            false
        }
    }

    /// Records task skip in context (corrects previous fail_task if called).
    /// Note: actual status is managed in Mission.tasks.
    pub fn skip_task(&self, context: &mut MissionContext, task_id: &str, reason: &str) {
        if let Some(entry) = context.task_registry.task_mut(task_id) {
            // Correct the verification status to Skipped
            entry.verification_status = super::types::VerificationStatus::Skipped;
            // Clear any error from previous fail_task() call
            entry.last_error = None;
            // Set summary to indicate skip reason
            entry.set_summary(format!("Skipped: {}", reason));
        }

        // Remove the blocker added by fail_task() if present
        let blocker_prefix = format!("Task {} failed:", task_id);
        context
            .summary
            .blockers
            .retain(|b| !b.starts_with(&blocker_prefix));

        context.update_timestamp();
    }

    pub fn add_learning(&self, context: &mut MissionContext, learning: &str) {
        context
            .summary
            .add_learning(learning, &self.context_config.limits);
        context.update_timestamp();
    }

    pub fn add_decision(&self, context: &mut MissionContext, decision: &str) {
        context
            .summary
            .add_decision(decision, &self.context_config.limits);
        context.update_timestamp();
    }

    /// Update context with actual token usage from API response.
    /// This provides ground truth for compaction decisions.
    ///
    /// # Example with claude-agent-rs
    /// ```ignore
    /// let response = agent.send(&messages).await?;
    /// context_manager.update_from_api_usage(context, &response.usage);
    /// ```
    pub fn update_actual_usage(&self, context: &mut MissionContext, usage: ActualUsage) {
        context.token_budget.update_actual(usage);
        info!(
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cache_hit_rate = ?context.token_budget.cache_efficiency(),
            has_actual = context.token_budget.has_actual_usage(),
            "Updated actual token usage"
        );
    }

    /// Update context with actual usage from claude-agent-rs API response.
    /// Accepts any type that implements `Into<ActualUsage>` (Usage, TokenUsage, etc.)
    pub fn update_from_api_usage(
        &self,
        context: &mut MissionContext,
        usage: impl Into<ActualUsage>,
    ) {
        self.update_actual_usage(context, usage.into());
    }

    /// Update context with actual token values (convenience method).
    pub fn update_actual_tokens(
        &self,
        context: &mut MissionContext,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let usage = ActualUsage::new(input_tokens, output_tokens);
        self.update_actual_usage(context, usage);
    }

    /// Update context with full actual usage including cache information.
    pub fn update_actual_tokens_with_cache(
        &self,
        context: &mut MissionContext,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_create: u64,
    ) {
        let usage = ActualUsage::with_cache(input_tokens, output_tokens, cache_read, cache_create);
        self.update_actual_usage(context, usage);
    }

    pub async fn build_planning_context(
        &self,
        context: &MissionContext,
        phase: &str,
    ) -> Result<String> {
        let budget = context.token_budget.allocation.current_phase;
        let mut result = String::new();
        let mut used = 0;

        let mission_summary_budget = self.context_config.task_budget.mission_summary_tokens;
        let phase_summary_budget = mission_summary_budget / 2;

        result.push_str("## Mission Summary\n");
        result.push_str(&context.summary.objective);
        result.push_str("\n\n");
        used += mission_summary_budget;

        let completed: Vec<_> = context.completed_phases().collect();
        for phase_summary in completed
            .iter()
            .rev()
            .take(self.display_config.max_recent_items)
        {
            if used + phase_summary_budget > budget {
                break;
            }
            result.push_str(&format!(
                "## Completed: {}\n{}\n\n",
                phase_summary.name, phase_summary.one_line_summary
            ));
            used += phase_summary_budget;
        }

        if !context.summary.critical_learnings.is_empty() {
            result.push_str("## Learnings\n");
            for learning in &context.summary.critical_learnings {
                result.push_str(&format!("- {}\n", learning));
            }
            result.push('\n');
        }

        info!(phase, used, "Built planning context");
        Ok(result)
    }

    pub async fn build_execution_context(
        &self,
        context: &MissionContext,
        task_id: &str,
    ) -> Result<String> {
        let budget = context.token_budget.allocation.current_task
            + context.token_budget.allocation.recent_tasks;
        let mut result = String::new();
        let mut estimated_tokens = 0;

        // Mission objective (always included)
        result.push_str("## Mission\n");
        result.push_str(&context.summary.objective);
        result.push_str("\n\n");
        estimated_tokens += estimate_tokens(&context.summary.objective) + 20;

        // Current phase
        if let Some(current_phase) = context.current_phase() {
            let phase_text = format!(
                "## Current Phase: {}\n{}\n\n",
                current_phase.name, current_phase.one_line_summary
            );
            estimated_tokens += estimate_tokens(&phase_text);
            if estimated_tokens <= budget {
                result.push_str(&phase_text);
            }
        }

        // Current task (always included - essential)
        if let Some(task) = context.task_registry.task(task_id) {
            result.push_str(&format!(
                "## Current Task: {}\n{}\n\n",
                task.id, task.description
            ));

            // Dependencies (budget-aware)
            if !task.dependencies.is_empty() {
                let mut deps_text = String::from("### Dependencies completed:\n");
                for dep_id in &task.dependencies {
                    if let Some(dep) = context.task_registry.task(dep_id)
                        && let Some(ref summary) = dep.summary
                    {
                        let dep_line = format!("- {}: {}\n", dep_id, summary);
                        if estimated_tokens + estimate_tokens(&dep_line) <= budget {
                            deps_text.push_str(&dep_line);
                            estimated_tokens += estimate_tokens(&dep_line);
                        }
                    }
                }
                deps_text.push('\n');
                result.push_str(&deps_text);
            }
        }

        // Recent completions (budget-aware, limit by budget)
        // Use tasks_with_summaries as proxy for completed tasks
        let completed_tasks: Vec<_> = context.task_registry.tasks_with_summaries().collect();
        let recent: Vec<_> = completed_tasks
            .iter()
            .rev()
            .take(self.display_config.max_recent_items)
            .collect();
        if !recent.is_empty() && estimated_tokens < budget {
            result.push_str("## Recent completions:\n");
            for task in recent {
                if let Some(ref summary) = task.summary {
                    let task_line = format!("- {}: {}\n", task.id, summary);
                    if estimated_tokens + estimate_tokens(&task_line) <= budget {
                        result.push_str(&task_line);
                        estimated_tokens += estimate_tokens(&task_line);
                    } else {
                        break;
                    }
                }
            }
            result.push('\n');
        }

        info!(task_id, estimated_tokens, budget, "Built execution context");
        Ok(result)
    }

    pub async fn build_review_context(&self, context: &MissionContext) -> Result<String> {
        let mut result = String::new();

        result.push_str("## Mission Summary\n");
        result.push_str(&context.summary.objective);
        result.push_str("\n\n");

        result.push_str(&format!(
            "## Progress: {}%\n\n",
            context.summary.progress.percentage
        ));

        result.push_str("## Completed Phases:\n");
        for phase in context.completed_phases() {
            result.push_str(&format!(
                "- {} ({} tasks): {}\n",
                phase.name, phase.completed_count, phase.one_line_summary
            ));
        }
        result.push('\n');

        if let Some(current) = context.current_phase() {
            result.push_str(&format!(
                "## Current Phase: {} ({}/{})\n\n",
                current.name, current.completed_count, current.task_count
            ));
        }

        Ok(result)
    }
}
