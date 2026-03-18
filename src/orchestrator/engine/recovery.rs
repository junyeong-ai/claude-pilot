use chrono::Utc;
use tracing::{debug, error, info, warn};

use super::{Orchestrator, TaskFailureOutcome};
use crate::context::MissionContext;
use crate::error::{PilotError, Result};
use crate::isolation::{IsolationPlanner, ScopeAnalysis};
use crate::mission::{
    IsolationMode, Learning, LearningCategory, Mission, TaskResult, TaskStatus,
};
use crate::notification::{EventType, MissionEvent};
use crate::recovery::{FailureHistory, RetryDecision};
use crate::state::{DomainEvent, MissionState};

impl Orchestrator {
    pub(super) async fn cleanup_on_failure(&self, mission: &mut Mission) {
        // Cleanup worktree if applicable
        if mission.isolation == IsolationMode::Worktree
            && let Err(e) = self.isolation.cleanup(mission, false).await
        {
            warn!(mission_id = %mission.id, error = %e, "Failed to cleanup worktree");
        }

        // Only transition if not already in a terminal state
        if !mission.status.is_terminal() {
            if let Err(e) = self
                .transition_state(mission, MissionState::Failed, "Cleanup on failure")
                .await
            {
                error!(mission_id = %mission.id, error = %e, "Failed to transition to Failed state");
                mission.status = MissionState::Failed;
                if let Err(e) = self.store.save(mission).await {
                    error!(mission_id = %mission.id, error = %e, "Failed to save mission state");
                }
            }

            self.notifier
                .notify(&MissionEvent::new(EventType::MissionFailed, &mission.id))
                .await;
        }
    }

    pub(super) async fn recover_in_progress_tasks(&self, mission: &mut Mission) -> Result<()> {
        let mission_id = mission.id.clone();
        let in_progress_ids: Vec<String> = mission
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InProgress || t.status == TaskStatus::Verifying)
            .map(|t| t.id.clone())
            .collect();

        for task_id in in_progress_ids {
            warn!(
                mission_id = %mission_id,
                task_id = %task_id,
                "Recovering interrupted task"
            );
            if let Some(task) = mission.task_mut(&task_id) {
                task.status = TaskStatus::Pending;
            }
        }

        if !mission.tasks.is_empty() {
            self.store.save(mission).await?;
        }

        Ok(())
    }

    pub(super) async fn setup_isolation(&self, mission: &mut Mission) -> Result<()> {
        if mission.isolation == IsolationMode::Auto {
            let scope = ScopeAnalysis::analyze(&mission.description);
            let active = self.store.active_missions().await?;
            let active_refs: Vec<_> = active.iter().collect();
            let planner = IsolationPlanner::new(&self.config, active_refs);
            mission.isolation = planner.determine_isolation(mission, scope.estimated_files);
        }

        self.isolation.setup(mission).await?;
        self.store.save(mission).await?;

        info!(
            mission_id = %mission.id,
            isolation = %mission.isolation,
            branch = ?mission.branch,
            worktree = ?mission.worktree_path,
            "Isolation configured"
        );

        Ok(())
    }

    pub(super) async fn fail_task_with_learning(
        &self,
        mission: &mut Mission,
        task_id: &str,
        error_msg: &str,
        error_category: &str,
        context: &mut MissionContext,
    ) {
        let mission_id = mission.id.clone();
        let working_dir = self.isolation.working_dir(mission);

        // Track failure in history for intelligent retry analysis
        {
            let mut histories = self.failure_histories.write().await;
            let history = histories
                .entry(format!("{}:{}", mission_id, task_id))
                .or_insert_with(|| {
                    if let Some(task) = mission.task(task_id) {
                        FailureHistory::new(task)
                    } else {
                        FailureHistory {
                            task_id: task_id.to_string(),
                            task_description: "Unknown task".to_string(),
                            attempts: Vec::new(),
                            last_updated_at: Utc::now(),
                        }
                    }
                });
            history.add_attempt(
                error_msg,
                error_category,
                None,
                self.config.recovery.retry_analyzer.max_error_history,
            );
        }

        // Check if task exists and get its retry status
        let task_can_retry = mission
            .task(task_id)
            .map(|t| t.can_retry())
            .unwrap_or(false);
        let task_max_retries = mission.task(task_id).map(|t| t.max_retries).unwrap_or(3);

        // Use LLM-based retry analysis if adaptive limits enabled
        let (should_retry, learning_to_add, escalation_learning) = if self
            .config
            .orchestrator
            .adaptive_limits
            && task_can_retry
        {
            let histories = self.failure_histories.read().await;
            if let Some(history) = histories.get(&format!("{}:{}", mission_id, task_id)) {
                match self.retry_analyzer.analyze(history, &working_dir).await {
                    Ok(RetryDecision::ContinueWithNewApproach {
                        strategy,
                        reasoning,
                    }) => {
                        info!(
                            task_id = %task_id,
                            strategy = %strategy,
                            "LLM suggests new approach for retry"
                        );
                        self.context_manager.add_learning(
                            context,
                            &format!("New approach for {}: {} ({})", task_id, strategy, reasoning),
                        );
                        let learning = Learning::new(
                            LearningCategory::Workaround,
                            format!("Retry strategy for {}: {}", task_id, strategy),
                        )
                        .with_source_task(task_id);
                        (true, Some(learning), None)
                    }
                    Ok(RetryDecision::ContinueSameApproach { reason }) => {
                        debug!(task_id = %task_id, reason = %reason, "Retrying with same approach");
                        (true, None, None)
                    }
                    Ok(RetryDecision::Escalate {
                        reason,
                        diagnosis,
                        suggested_actions,
                    }) => {
                        warn!(
                            task_id = %task_id,
                            reason = %reason,
                            "LLM recommends escalation, marking task as permanently failed"
                        );
                        let escalation = Learning::new(
                            LearningCategory::Gotcha,
                            format!(
                                "Task {} escalated: {}. Diagnosis: {}. Suggested: {}",
                                task_id,
                                reason,
                                diagnosis,
                                suggested_actions.join(", ")
                            ),
                        )
                        .with_source_task(task_id);
                        (false, None, Some((escalation, task_max_retries + 1)))
                    }
                    Err(e) => {
                        warn!(task_id = %task_id, error = %e, "Retry analysis failed, using default behavior");
                        (true, None, None) // Default to retry on analysis failure
                    }
                }
            } else {
                (true, None, None) // No history yet, allow retry
            }
        } else {
            (task_can_retry, None, None)
        };

        // Now apply the changes to the task
        if let Some(task) = mission.task_mut(task_id) {
            // Apply escalation if needed (set retry_count beyond max)
            if let Some((_, new_retry_count)) = &escalation_learning {
                task.retry_count = *new_retry_count;
            }
            task.fail(error_msg.to_string());
        }

        // Add learnings after releasing task borrow
        if let Some(learning) = learning_to_add {
            mission.add_learning(learning);
        }
        if let Some((learning, _)) = escalation_learning {
            mission.add_learning(learning);
        }

        self.context_manager.fail_task(context, task_id, error_msg);

        self.notifier
            .notify(
                &MissionEvent::new(EventType::VerificationFailed, &mission_id)
                    .with_task(task_id)
                    .with_message(error_msg),
            )
            .await;

        if should_retry {
            mission.add_learning(
                Learning::new(
                    LearningCategory::Gotcha,
                    format!("Task {} failed: {}", task_id, error_msg),
                )
                .with_source_task(task_id),
            );
            self.context_manager
                .add_learning(context, &format!("Task {} failed: {}", task_id, error_msg));
        }

        // Emit task failed event
        let retry_count = mission.task(task_id).map(|t| t.retry_count).unwrap_or(0);
        self.emit_event(DomainEvent::task_failed(
            &mission_id,
            task_id,
            error_msg,
            retry_count,
        ))
        .await;

        warn!(
            mission_id = %mission_id,
            task_id = %task_id,
            error = %error_msg,
            will_retry = should_retry,
            "Task verification failed"
        );
    }

    pub(super) async fn handle_task_failure(
        &self,
        mission: &mut Mission,
        task_id: &str,
        error: PilotError,
        context: &mut MissionContext,
    ) -> TaskFailureOutcome {
        let mission_id = mission.id.clone();

        if let Some(task) = mission.task_mut(task_id) {
            task.fail(error.to_string());
        }

        self.context_manager
            .fail_task(context, task_id, &error.to_string());

        // Upgrade budget complexity if failure rate is high
        let total_retries: u32 = mission.tasks.iter().map(|t| t.retry_count).sum();
        let task_count = mission.tasks.len() as u32;
        self.context_manager
            .upgrade_budget_on_failures(context, total_retries, task_count);

        self.notifier
            .notify(
                &MissionEvent::new(EventType::TaskFailed, &mission_id)
                    .with_task(task_id)
                    .with_message(error.to_string()),
            )
            .await;

        // Attempt recovery if enabled
        if !self.config.recovery.enabled {
            return TaskFailureOutcome::Failed;
        }

        let analysis = crate::recovery::FailureAnalyzer::analyze(
            &error,
            mission,
            crate::recovery::FailurePhase::Execution,
        );

        if !self.recovery_executor.should_auto_recover(&analysis).await {
            return TaskFailureOutcome::Failed;
        }

        let strategy = analysis.suggested_strategy.clone();
        match self
            .recovery_executor
            .execute_recovery(&strategy, mission, context)
            .await
        {
            Ok(crate::recovery::RecoveryStatus::Recovered { message, action }) => {
                info!(task_id = %task_id, message = %message, "Executing recovery action");
                self.execute_recovery_action(mission, context, task_id, action)
                    .await;
                TaskFailureOutcome::Recovered
            }
            Ok(crate::recovery::RecoveryStatus::Escalated { reason }) => {
                warn!(task_id = %task_id, reason = %reason, "Task escalated - suspending mission");
                TaskFailureOutcome::Escalated {
                    reason,
                    analysis: Some(analysis),
                }
            }
            Ok(crate::recovery::RecoveryStatus::Skipped { reason }) => {
                warn!(task_id = %task_id, reason = %reason, "Task skipped");
                TaskFailureOutcome::Skipped
            }
            Ok(crate::recovery::RecoveryStatus::Failed { reason, .. }) => {
                error!(task_id = %task_id, reason = %reason, "Recovery failed");
                TaskFailureOutcome::Failed
            }
            Err(recovery_err) => {
                error!(task_id = %task_id, error = %recovery_err, "Recovery error");
                TaskFailureOutcome::Failed
            }
        }
    }

    async fn execute_recovery_action(
        &self,
        mission: &mut Mission,
        _context: &mut MissionContext,
        task_id: &str,
        action: crate::recovery::RecoveryAction,
    ) {
        use crate::recovery::RecoveryAction;

        match action {
            RecoveryAction::Retry => {
                // Reset task status to pending for retry
                // Note: retry_count was already incremented by task.fail() in handle_task_failure
                if let Some(task) = mission.task_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    info!(task_id = %task_id, retry_count = task.retry_count, "Task reset for retry");
                }
            }
            RecoveryAction::RetryWithReducedChunks { reduction_factor } => {
                // Reset task for retry with reduced scope
                // Note: retry_count was already incremented by task.fail() in handle_task_failure
                if let Some(task) = mission.task_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    // Apply reduction to scope_factor (compounds with previous reductions)
                    task.scope_factor *= reduction_factor;
                    info!(
                        task_id = %task_id,
                        reduction_factor = reduction_factor,
                        new_scope_factor = task.scope_factor,
                        "Task reset for retry with reduced scope"
                    );
                }
            }
            RecoveryAction::StartConvergentVerification { max_rounds } => {
                // Run convergent verification loop with specified max rounds
                // Uses verify_until_convergent_recovery to include AI review (NON-NEGOTIABLE)
                info!(
                    task_id = %task_id,
                    max_rounds = max_rounds,
                    "Starting convergent verification with AI review"
                );
                let working_dir = self.isolation.working_dir(mission);
                match self
                    .convergent_verifier
                    .verify_until_convergent_recovery(mission, Some(max_rounds), &working_dir)
                    .await
                {
                    Ok(result) if result.converged => {
                        if let Some(task) = mission.task_mut(task_id) {
                            task.complete(TaskResult {
                                success: true,
                                output: format!(
                                    "Completed after {} convergent verification rounds",
                                    result.total_rounds
                                ),
                                files_modified: Vec::new(),
                                files_created: Vec::new(),
                                files_deleted: Vec::new(),
                                input_tokens: None,
                                output_tokens: None,
                            });
                            info!(
                                task_id = %task_id,
                                rounds = result.total_rounds,
                                "Task completed after convergent verification"
                            );
                        }
                    }
                    Ok(result) => {
                        warn!(
                            task_id = %task_id,
                            rounds = result.total_rounds,
                            "Convergent verification did not converge"
                        );
                    }
                    Err(e) => {
                        error!(task_id = %task_id, error = %e, "Convergent verification failed");
                    }
                }
            }
        }
    }

    /// Remove stale failure history entries to prevent unbounded memory growth.
    /// TTL is configured via `recovery.failure_history_ttl_secs`.
    pub(super) async fn cleanup_stale_failure_histories(&self) {
        let ttl = std::time::Duration::from_secs(self.config.recovery.failure_history_ttl_secs);
        let mut histories = self.failure_histories.write().await;
        let before_count = histories.len();

        histories.retain(|_, history| !history.is_stale(ttl));

        let removed = before_count - histories.len();
        if removed > 0 {
            debug!(
                removed_count = removed,
                remaining_count = histories.len(),
                "Cleaned up stale failure histories"
            );
        }
    }
}
