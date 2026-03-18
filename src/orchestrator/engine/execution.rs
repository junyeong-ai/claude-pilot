use std::sync::Arc;

use chrono::Utc;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use super::Orchestrator;
use crate::error::{MissionError, PilotError, Result, VerificationError};
use crate::mission::{
    IsolationMode, Mission, MissionFlags, MissionPriority, OnComplete, Task, TaskResult, TaskStatus,
};
use crate::notification::{EventType, MissionEvent};
use crate::quality::{CoherenceTaskResult, QualityFileChange};
use crate::state::{DomainEvent, MissionState, ProgressProjection};
use crate::verification::FileTracker;

impl Orchestrator {
    pub async fn create_mission(
        &self,
        description: &str,
        flags: MissionFlags,
        priority: Option<MissionPriority>,
        on_complete: Option<OnComplete>,
    ) -> Result<Mission> {
        use crate::isolation::{IsolationPlanner, ScopeAnalysis};

        let id = self.store.next_id().await?;

        let mut mission = Mission::new(&id, description);
        mission.flags = flags;
        mission.max_iterations = self.config.orchestrator.max_iterations;
        if let Some(p) = priority {
            mission.priority = p;
        }
        if let Some(oc) = on_complete {
            mission.on_complete = oc;
        }

        let scope = ScopeAnalysis::analyze(description);
        let active = self.store.active_missions().await?;
        let active_refs: Vec<_> = active.iter().collect();
        let planner = IsolationPlanner::new(&self.config, active_refs);

        let isolation = planner.determine_isolation(&mission, scope.estimated_files);
        mission.isolation = isolation;

        self.store.save(&mission).await?;

        {
            let mut signals = self.signals.write().await;
            signals.insert(
                id.clone(),
                crate::orchestrator::signal::SignalHandler::new(),
            );
        } // Drop write lock before async operations

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionCreated, &mission.id))
            .await;

        info!(
            mission_id = %mission.id,
            isolation = %isolation,
            "Mission created"
        );

        // Emit mission created event
        self.emit_event(DomainEvent::mission_created(&mission.id, description))
            .await;

        Ok(mission)
    }

    pub async fn execute(&self, mission_id: &str) -> Result<()> {
        // Acquire execution lock - prevents concurrent execution and enables orphan detection
        let _lock_guard = self.lifecycle.acquire_lock(mission_id).await?;

        let mut mission = self.store.load(mission_id).await?;

        if mission.status.is_terminal() {
            return Err(MissionError::InvalidState {
                expected: "non-terminal".into(),
                actual: mission.status.to_string(),
            }
            .into());
        }

        // Restore from checkpoint if available (durable execution for long-running missions)
        let restored = {
            let mut context = self.context_manager.load_or_init(&mission).await?;
            if let Some((checkpoint_id, failure_histories)) = self
                .recovery_executor
                .try_restore_on_startup(&mut mission, &mut context)
                .await?
            {
                info!(checkpoint_id = %checkpoint_id, "Mission state restored from checkpoint");
                self.store.save(&mission).await?;
                self.context_manager.save(&context).await?;
                // Restore failure histories for retry pattern learning
                if let Some(histories) = failure_histories {
                    let mut fh = self.failure_histories.write().await;
                    fh.extend(histories);
                }
                true
            } else {
                false
            }
        };

        if restored {
            debug!(mission_id = %mission.id, "Continuing from restored checkpoint state");
        }

        // Enhanced recovery: reconcile mission state from event history
        if let Some(replayer) = &self.event_replayer {
            match replayer
                .rebuild_projection::<ProgressProjection>(&mission.id)
                .await
            {
                Ok(projection) if projection.last_applied_seq > 0 => {
                    // Reconcile task completion counts if projection has more accurate data
                    let mission_completed = mission
                        .tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Completed)
                        .count();
                    if projection.completed_tasks > mission_completed {
                        info!(
                            mission_id = %mission.id,
                            events_replayed = projection.last_applied_seq,
                            projection_completed = projection.completed_tasks,
                            mission_completed = mission_completed,
                            "Event history shows more completed tasks than mission state"
                        );
                    }

                    // Log recovery context for debugging
                    debug!(
                        mission_id = %mission.id,
                        projection_state = ?projection.state,
                        total_tasks = projection.total_tasks,
                        completed_tasks = projection.completed_tasks,
                        issues_detected = projection.total_issues_detected,
                        issues_resolved = projection.total_issues_resolved,
                        "Mission progress recovered from event history"
                    );
                }
                Ok(_) => {
                    // No events to replay - fresh mission
                }
                Err(e) => {
                    debug!(error = %e, "Event replay not available (non-fatal)");
                }
            }
        }

        // Handle escalation state - enforce human intervention requirement
        if mission.status == MissionState::Escalated {
            // Mission is in Escalated state - must go through resume()
            // This applies even if escalation context is missing (data loss/legacy)
            return Err(MissionError::InvalidState {
                expected: "Use 'resume' command for escalated missions".into(),
                actual: "Escalated (pending human intervention)".into(),
            }
            .into());
        }

        // Clear residual escalation context if mission was properly resumed
        if mission.escalation.is_some() {
            debug!(mission_id = %mission.id, "Clearing resolved escalation context");
            mission.escalation = None;
            self.store.save(&mission).await?;
        }

        self.recover_in_progress_tasks(&mut mission).await?;
        self.setup_isolation(&mut mission).await?;

        // Check context budget before proceeding (prevents API overflow errors)
        self.check_context_budget(&mission).await?;

        // 3-tier complexity routing:
        // - Trivial: Skip evidence gathering entirely, execute directly
        // - Simple: Gather evidence, execute directly without full planning
        // - Complex: Full planning with spec/plan/tasks
        if mission.tasks.is_empty() {
            // Fast path: --direct flag bypasses all complexity evaluation and evidence gathering
            if mission.flags.direct {
                info!(
                    mission_id = %mission.id,
                    "Direct mode enabled, skipping evidence gathering and planning"
                );
                return self.execute_direct(&mut mission).await;
            }

            // Initial fast assessment without evidence
            let initial_tier = self.evaluate_complexity_tier(&mission, None).await;

            match initial_tier {
                crate::planning::ComplexityTier::Trivial => {
                    // Trivial tasks skip evidence gathering entirely
                    info!(
                        mission_id = %mission.id,
                        "Trivial task detected, skipping evidence gathering"
                    );
                    mission.estimated_complexity = Some(initial_tier);
                    return self.execute_direct(&mut mission).await;
                }
                crate::planning::ComplexityTier::Simple
                | crate::planning::ComplexityTier::Complex => {
                    // Gather evidence for more accurate assessment and planning reuse
                    let evidence = self
                        .evidence_gatherer
                        .gather(&mission.description, None)
                        .await?;

                    // Re-evaluate with evidence for better accuracy
                    let final_tier = self
                        .evaluate_complexity_tier(&mission, Some(&evidence))
                        .await;

                    match final_tier {
                        crate::planning::ComplexityTier::Trivial
                        | crate::planning::ComplexityTier::Simple => {
                            // Simple (or downgraded to trivial) - execute directly
                            debug!(
                                mission_id = %mission.id,
                                initial_tier = ?initial_tier,
                                final_tier = ?final_tier,
                                "Using direct execution after evidence assessment"
                            );
                            mission.estimated_complexity = Some(final_tier);
                            return self.execute_direct(&mut mission).await;
                        }
                        crate::planning::ComplexityTier::Complex => {
                            mission.estimated_complexity = Some(final_tier);

                            // Use multi-agent coordinator if available
                            if self.coordinator.is_some() {
                                info!(
                                    mission_id = %mission.id,
                                    "Using multi-agent coordinator for complex task"
                                );
                                return self.execute_with_coordinator(&mut mission).await;
                            }

                            // Fall back to single-agent planning + execution
                            self.transition_state(
                                &mut mission,
                                MissionState::Planning,
                                "Complex task requires full planning",
                            )
                            .await?;

                            if let Err(e) = self
                                .plan_mission_with_evidence(&mut mission, evidence)
                                .await
                            {
                                self.cleanup_on_failure(&mut mission).await;
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }

        // Only transition to Running if not already in Running state
        // (resume() may have already transitioned from Paused/Escalated to Running)
        if mission.status != MissionState::Running {
            self.transition_state(&mut mission, MissionState::Running, "Starting execution")
                .await?;
        }
        if mission.started_at.is_none() {
            mission.started_at = Some(Utc::now());
        }
        self.store.save(&mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, mission_id))
            .await;

        if let Err(e) = self.execute_loop(&mut mission).await {
            // Only cleanup if NOT in Escalated state.
            // Escalated state means we intentionally suspended for human intervention
            // and must preserve worktree and state for durable resume.
            if !mission.status.is_suspended() {
                self.cleanup_on_failure(&mut mission).await;
            }
            return Err(e);
        }

        // Lock is automatically released when _lock_guard goes out of scope
        Ok(())
    }

    async fn check_signal(&self, mission: &mut Mission) -> Result<bool> {
        use super::super::signal::Signal;

        let signal = {
            let signals = self.signals.read().await;
            signals
                .get(&mission.id)
                .map(|h| h.check_and_acknowledge())
                .unwrap_or(Signal::None)
        };

        match signal {
            Signal::None => Ok(false),
            Signal::Pause => {
                self.transition_state(mission, MissionState::Paused, "Pause signal received")
                    .await?;
                self.notifier
                    .notify(&MissionEvent::new(EventType::MissionPaused, &mission.id))
                    .await;
                info!(mission_id = %mission.id, "Mission paused by signal");
                Err(MissionError::Paused.into())
            }
            Signal::Cancel => {
                self.transition_state(mission, MissionState::Cancelled, "Cancel signal received")
                    .await?;
                if mission.isolation == IsolationMode::Worktree {
                    self.isolation.cleanup(mission, true).await?;
                }
                self.notifier
                    .notify(&MissionEvent::new(EventType::MissionCancelled, &mission.id))
                    .await;
                info!(mission_id = %mission.id, "Mission cancelled by signal");
                Err(MissionError::Cancelled.into())
            }
        }
    }

    async fn execute_loop(&self, mission: &mut Mission) -> Result<()> {
        let mut checkpoint_counter = 0;
        let mut coherence_counter = 0;
        let mut completed_task_results: Vec<CoherenceTaskResult> = Vec::new();
        let max_parallel = self.config.orchestrator.max_parallel_tasks;
        let semaphore = Arc::new(Semaphore::new(max_parallel));

        let checkpoint_interval_minutes = self.config.recovery.checkpoint.interval_minutes;
        let mut last_checkpoint_time = std::time::Instant::now();
        let mission_start = std::time::Instant::now();
        let mission_timeout =
            std::time::Duration::from_secs(self.config.orchestrator.mission_timeout_secs);

        let model_config = self.config.agent.model_config()?;
        let mut context = self
            .context_manager
            .load_or_init_with_model_config(mission, &model_config)
            .await?;

        while !mission.all_tasks_done() && mission.iteration < mission.max_iterations {
            self.check_signal(mission).await?;

            // Check mission-level timeout (0 = unlimited)
            if mission_timeout.as_secs() > 0 && mission_start.elapsed() > mission_timeout {
                let reason = format!(
                    "Mission timeout exceeded: {}s (can resume with 'pilot resume {}')",
                    mission_timeout.as_secs(),
                    mission.id
                );

                // Save checkpoint before timeout - allows resume later
                if let Err(cp_err) = self.checkpoint(mission).await {
                    warn!(error = %cp_err, "Failed to save checkpoint on timeout");
                }

                // Escalate instead of failing immediately
                self.transition_state(mission, MissionState::Escalated, &reason)
                    .await?;

                // Create and persist escalation context for timeout
                let mut escalation_ctx =
                    crate::recovery::EscalationContext::new(&mission.id, &reason);
                escalation_ctx.add_action(crate::recovery::HumanAction::Retry);
                mission.escalation = Some(escalation_ctx);

                self.notifier
                    .notify(
                        &MissionEvent::new(EventType::MissionPaused, &mission.id)
                            .with_message(reason.clone()),
                    )
                    .await;

                if let Err(save_err) = self.store.save(mission).await {
                    error!(error = %save_err, "Failed to save mission on timeout");
                }

                return Err(PilotError::Timeout(reason));
            }

            if self.config.context.enable_auto_compaction
                && let Some(trigger) = self.context_manager.needs_compaction(&context)
            {
                info!(?trigger, "Context compaction triggered");
                self.context_manager.compact(&mut context).await?;
            }

            mission.iteration += 1;

            let next_tasks = mission.next_tasks();
            if next_tasks.is_empty() {
                if mission.active_tasks().is_empty() {
                    if mission.all_tasks_done() {
                        break;
                    }

                    // Check for tasks blocked by permanently failed dependencies
                    // These tasks can never start, so auto-skip them to prevent stalling
                    let blocked_tasks: Vec<(String, String)> = mission
                        .tasks_blocked_by_failed_deps()
                        .into_iter()
                        .map(|(task, deps)| (task.id.clone(), deps.join(", ")))
                        .collect();

                    if !blocked_tasks.is_empty() {
                        for (task_id, failed_deps) in &blocked_tasks {
                            let reason = format!("Dependency failed permanently: {}", failed_deps);
                            warn!(
                                task_id = %task_id,
                                failed_deps = %failed_deps,
                                "Auto-skipping task due to failed dependencies"
                            );
                            if let Some(t) = mission.task_mut(task_id) {
                                t.skip(reason.clone());
                            }
                            self.context_manager
                                .skip_task(&mut context, task_id, &reason);
                        }
                        self.store.save(mission).await?;
                        self.context_manager.save(&context).await?;
                        // Continue loop to re-evaluate after skipping blocked tasks
                        continue;
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    continue;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                continue;
            }

            let working_dir = self.isolation.working_dir(mission);
            let task_snapshots: Vec<(String, Task)> = next_tasks
                .iter()
                .map(|t| (t.id.clone(), (*t).clone()))
                .collect();

            // Start focus tracking for each task (anti-drift)
            {
                let mut focus = self.focus_tracker.write().await;
                for (task_id, _) in &task_snapshots {
                    let scope =
                        focus.derive_scope(task_id, None, None, context.evidence_snapshot.as_ref());
                    focus.start_task(task_id, scope);
                }
            }

            for (task_id, task) in &task_snapshots {
                if let Some(t) = mission.task_mut(task_id) {
                    t.start();
                    self.context_manager.start_task(&mut context, task_id);
                    self.notifier
                        .notify(
                            &MissionEvent::new(EventType::TaskStarted, &mission.id)
                                .with_task(task_id),
                        )
                        .await;

                    // Emit task started event
                    self.emit_event(DomainEvent::task_started(
                        &mission.id,
                        task_id,
                        &task.description,
                    ))
                    .await;
                }
            }
            self.store.save(mission).await?;
            self.context_manager.save(&context).await?;

            // Execute tasks with file tracking
            let results = self
                .execute_tasks_with_tracking(
                    mission,
                    task_snapshots,
                    &working_dir,
                    semaphore.clone(),
                )
                .await;

            // Process results with convergent verification and focus tracking
            for (task_id, result, file_changes) in results {
                // Validate focus/scope for completed task (anti-drift)
                {
                    let mut focus = self.focus_tracker.write().await;
                    let validation = focus.validate_completion(&task_id, &file_changes);
                    if !validation.warnings.is_empty() {
                        for warning in &validation.warnings {
                            warn!(
                                task_id = %task_id,
                                warning = ?warning,
                                "Focus drift detected"
                            );
                        }
                    }
                }
                match self
                    .apply_task_result_with_convergence(
                        mission,
                        &task_id,
                        result,
                        file_changes.clone(),
                        &mut context,
                        &working_dir,
                    )
                    .await
                {
                    Ok(_) => {
                        if let Some(task) = mission.task(&task_id) {
                            let mut changes: Vec<QualityFileChange> = file_changes
                                .created
                                .iter()
                                .map(|p| QualityFileChange::created(p.clone()))
                                .collect();
                            changes.extend(
                                file_changes
                                    .modified
                                    .iter()
                                    .map(|p| QualityFileChange::modified(p.clone())),
                            );
                            completed_task_results.push(
                                CoherenceTaskResult::new(&task_id, &task.description)
                                    .with_file_changes(changes),
                            );
                        }
                    }
                    Err(e) => {
                        error!(task_id = %task_id, error = %e, "Task execution failed");
                        let outcome = self
                            .handle_task_failure(mission, &task_id, e, &mut context)
                            .await;

                        match outcome {
                            super::TaskFailureOutcome::Recovered => {
                                // Task was reset to Pending by execute_recovery_action
                                // It will be picked up in the next iteration
                                debug!(task_id = %task_id, "Task recovered, will retry");
                            }
                            super::TaskFailureOutcome::Skipped => {
                                // Mark task as skipped with proper result
                                let reason = "Task skipped by recovery decision";
                                if let Some(task) = mission.task_mut(&task_id) {
                                    task.skip(reason.to_string());
                                    info!(task_id = %task_id, "Task marked as skipped");
                                }
                                // Correct context to reflect skip (removes failed blocker, sets Skipped status)
                                self.context_manager
                                    .skip_task(&mut context, &task_id, reason);
                            }
                            super::TaskFailureOutcome::Failed => {
                                // Task stays in Failed state (set by task.fail())
                                // If it can't retry, it will be counted as permanently failed
                                debug!(task_id = %task_id, "Task permanently failed");
                            }
                            super::TaskFailureOutcome::Escalated { reason, analysis } => {
                                // Save checkpoint before escalation
                                if let Err(cp_err) = self.checkpoint(mission).await {
                                    warn!(error = %cp_err, "Failed to save checkpoint before escalation");
                                }

                                // Transition to Escalated state
                                if let Err(state_err) = self
                                    .transition_state(mission, MissionState::Escalated, &reason)
                                    .await
                                {
                                    error!(error = %state_err, "Failed to transition to Escalated state");
                                }

                                // Create escalation context with full details
                                let mut escalation_ctx =
                                    crate::recovery::EscalationContext::new(&mission.id, &reason);
                                if let Some(a) = analysis {
                                    escalation_ctx = escalation_ctx.with_analysis(a);
                                }
                                escalation_ctx.add_action(crate::recovery::HumanAction::Retry);

                                // Persist escalation context to mission for durable resume
                                mission.escalation = Some(escalation_ctx.clone());

                                // Notify about escalation
                                self.notifier
                                    .notify(
                                        &MissionEvent::new(EventType::MissionPaused, &mission.id)
                                            .with_message(format!("Escalation: {}", reason)),
                                    )
                                    .await;

                                // Save mission state with escalation context
                                if let Err(save_err) = self.store.save(mission).await {
                                    error!(error = %save_err, "Failed to save mission after escalation");
                                }

                                // Return error to exit execution loop
                                return Err(PilotError::EscalationRequired { summary: reason });
                            }
                        }
                    }
                }
            }

            self.store.save(mission).await?;
            self.context_manager
                .update_from_mission(&mut context, mission);
            self.context_manager.save(&context).await?;

            // Incremental coherence check
            coherence_counter += 1;
            if self.config.coherence.incremental_enabled
                && coherence_counter >= self.config.coherence.incremental_interval as usize
                && !completed_task_results.is_empty()
            {
                self.run_incremental_coherence(mission, &completed_task_results)
                    .await;
                coherence_counter = 0;
            }

            // Task-based checkpoint
            checkpoint_counter += 1;
            let task_checkpoint_needed =
                checkpoint_counter >= self.config.recovery.checkpoint.interval_tasks as usize;

            // Time-based checkpoint
            let time_checkpoint_needed = checkpoint_interval_minutes > 0
                && last_checkpoint_time.elapsed().as_secs() >= checkpoint_interval_minutes * 60;

            if task_checkpoint_needed || time_checkpoint_needed {
                self.checkpoint(mission).await?;
                if self.config.recovery.enabled {
                    let evidence = if self.config.recovery.checkpoint.persist_evidence {
                        context.evidence_snapshot.as_ref()
                    } else {
                        None
                    };
                    let histories = self.failure_histories.read().await;
                    self.recovery_executor
                        .create_checkpoint(mission, &context, evidence, Some(&*histories))
                        .await?;
                    drop(histories);

                    // Emit checkpoint created event
                    let checkpoint_id = format!("cp-{}-{}", mission.id, mission.iteration);
                    self.emit_event(DomainEvent::checkpoint_created(
                        &mission.id,
                        &checkpoint_id,
                        mission.tasks.len(),
                    ))
                    .await;
                }
                self.cleanup_stale_failure_histories().await;
                checkpoint_counter = 0;
                last_checkpoint_time = std::time::Instant::now();
            }
        }

        if mission.iteration >= mission.max_iterations {
            self.transition_state(mission, MissionState::Failed, "Maximum iterations exceeded")
                .await?;
            return Err(PilotError::MaxIterationsExceeded(mission.id.clone()));
        }

        let failed_ids: Vec<String> = mission
            .permanently_failed_tasks()
            .iter()
            .map(|t| t.id.clone())
            .collect();

        if !failed_ids.is_empty() {
            let reason = format!(
                "{} permanently failed tasks: {}",
                failed_ids.len(),
                failed_ids.join(", ")
            );
            self.transition_state(mission, MissionState::Failed, &reason)
                .await?;
            return Err(VerificationError::Failed {
                message: reason,
                checks: Vec::new(),
            }
            .into());
        }

        self.finalize(mission).await?;

        Ok(())
    }

    /// Execute tasks with file tracking using FileTracker for accurate change detection.
    async fn execute_tasks_with_tracking(
        &self,
        mission: &Mission,
        tasks: Vec<(String, Task)>,
        working_dir: &std::path::Path,
        semaphore: Arc<Semaphore>,
    ) -> Vec<(String, Result<TaskResult>, crate::verification::TrackedFileChanges)> {
        use futures::future::join_all;

        let task_ids: Vec<String> = tasks.iter().map(|(id, _)| id.clone()).collect();
        let mission_arc = Arc::new(mission.clone());
        // Parallel mode only when both: multiple tasks AND semaphore permits > 1
        // If semaphore permits == 1, tasks run sequentially even with multiple tasks
        let actual_concurrency = semaphore.available_permits().min(tasks.len());
        let parallel_mode = actual_concurrency > 1;

        let handles: Vec<_> = tasks
            .into_iter()
            .map(|(task_id, task)| {
                let sem = semaphore.clone();
                let agent = Arc::clone(&self.agent);
                let mission_ref = Arc::clone(&mission_arc);
                let working_dir = working_dir.to_path_buf();

                tokio::spawn(async move {
                    let permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => {
                            return (
                                task_id,
                                Err(PilotError::AgentExecution("semaphore closed".into())),
                                crate::verification::TrackedFileChanges::default(),
                            );
                        }
                    };

                    debug!(task_id = %task_id, "Starting task execution with file tracking");

                    // Use parallel mode when multiple tasks execute concurrently
                    let mut file_tracker = FileTracker::new().with_parallel_mode(parallel_mode);
                    file_tracker.capture_before(&working_dir);

                    let result = agent.execute_task(&mission_ref, &task, &working_dir).await;

                    // Capture filesystem state after execution
                    file_tracker.capture_after(&working_dir);

                    let file_changes = file_tracker.changes();

                    drop(permit);

                    debug!(
                        task_id = %task_id,
                        success = result.is_ok(),
                        files_created = file_changes.created.len(),
                        files_modified = file_changes.modified.len(),
                        files_deleted = file_changes.deleted.len(),
                        "Task execution completed with file tracking"
                    );

                    (task_id, result, file_changes)
                })
            })
            .collect();

        let results = join_all(handles).await;

        results
            .into_iter()
            .zip(task_ids)
            .map(|(r, original_id)| match r {
                Ok(result) => result,
                Err(e) => {
                    error!(task_id = %original_id, error = %e, "Task panicked during execution");
                    (
                        original_id,
                        Err(PilotError::AgentExecution(format!("task panicked: {}", e))),
                        crate::verification::TrackedFileChanges::default(),
                    )
                }
            })
            .collect()
    }
}
