use tracing::{debug, info, warn};

use super::Orchestrator;
use crate::context::MissionContext;
use crate::error::{MissionError, Result, VerificationError};
use crate::git::GitRunner;
use crate::mission::{Mission, TaskResult, TaskStatus};
use crate::notification::{EventType, MissionEvent};
use crate::quality::CoherenceTaskResult;
use crate::state::{DomainEvent, MissionState};

impl Orchestrator {
    /// Apply task result with convergent verification for automatic error correction.
    pub(super) async fn apply_task_result_with_convergence(
        &self,
        mission: &mut Mission,
        task_id: &str,
        result: Result<TaskResult>,
        file_changes: crate::verification::TrackedFileChanges,
        context: &mut MissionContext,
        working_dir: &std::path::Path,
    ) -> Result<()> {
        let mission_id = mission.id.clone();
        let task_not_found = || MissionError::TaskNotFound {
            mission_id: mission_id.clone(),
            task_id: task_id.to_string(),
        };

        match result {
            Ok(mut task_result) => {
                // Update file changes from FileTracker (more accurate than pattern matching)
                task_result.files_created = file_changes
                    .created
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                task_result.files_modified = file_changes
                    .modified
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                task_result.files_deleted = file_changes
                    .deleted
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();

                // Validate that actual implementation occurred
                let has_file_changes = !file_changes.is_empty();
                if !has_file_changes {
                    warn!(
                        task_id = %task_id,
                        "No file changes detected - task may not have been implemented"
                    );
                }

                mission.task_mut(task_id).ok_or_else(task_not_found)?.status =
                    TaskStatus::Verifying;

                let task_for_verify = mission
                    .tasks
                    .iter()
                    .find(|t| t.id == task_id)
                    .ok_or_else(task_not_found)?;

                let verification = self
                    .verifier
                    .verify_task(task_for_verify, &task_result, working_dir)
                    .await;

                // Check basic requirements before convergent verification
                let has_implementation = has_file_changes || !verification.checks.is_empty();

                if !has_implementation {
                    self.fail_task_with_learning(
                        mission,
                        task_id,
                        "No implementation detected - agent did not make any file changes",
                        "NoImplementation",
                        context,
                    )
                    .await;
                    return Ok(());
                }

                let diff = GitRunner::new(working_dir)
                    .diff_for_review(&task_result.files_modified, &task_result.files_created)
                    .await;

                info!(
                    task_id = %task_id,
                    initial_passed = verification.passed,
                    failed_checks = verification.failed_checks().len(),
                    files_created = task_result.files_created.len(),
                    files_modified = task_result.files_modified.len(),
                    has_diff = diff.is_some(),
                    has_test_patterns = !task_for_verify.test_patterns.is_empty(),
                    "Running task-scoped convergent verification"
                );

                let convergence = self
                    .convergent_verifier
                    .verify_until_convergent_task_scoped(
                        mission,
                        task_for_verify.test_patterns.clone(),
                        task_for_verify.module.clone(),
                        &task_result.files_created,
                        &task_result.files_modified,
                        diff.as_deref(),
                        working_dir,
                    )
                    .await?;

                if convergence.converged {
                    info!(
                        task_id = %task_id,
                        rounds = convergence.total_rounds,
                        "Convergent verification succeeded"
                    );
                    self.complete_task_success(mission, task_id, task_result, context)
                        .await?;
                } else {
                    let error_msg = format!(
                        "Convergent verification failed after {} rounds: {:?}",
                        convergence.total_rounds,
                        convergence
                            .persistent_issues
                            .iter()
                            .map(|i| &i.message)
                            .take(self.config.display.max_recent_items)
                            .collect::<Vec<_>>()
                    );

                    self.fail_task_with_learning(
                        mission,
                        task_id,
                        &error_msg,
                        "ConvergenceFailure",
                        context,
                    )
                    .await;
                }
            }
            Err(e) => {
                return Err(e);
            }
        }

        Ok(())
    }

    pub(super) async fn complete_task_success(
        &self,
        mission: &mut Mission,
        task_id: &str,
        task_result: TaskResult,
        context: &mut MissionContext,
    ) -> Result<()> {
        let mission_id = mission.id.clone();
        let task_not_found = || MissionError::TaskNotFound {
            mission_id: mission_id.clone(),
            task_id: task_id.to_string(),
        };

        let files: Vec<String> = task_result
            .files_modified
            .iter()
            .chain(&task_result.files_created)
            .cloned()
            .collect();

        mission
            .task_mut(task_id)
            .ok_or_else(task_not_found)?
            .complete(task_result);

        // Emit task completed event (before files ownership is transferred)
        self.emit_event(DomainEvent::task_completed(
            &mission_id,
            task_id,
            files.clone(),
            0, // Duration tracked by task itself
        ))
        .await;

        self.context_manager.complete_task(
            context,
            task_id,
            Some(format!("Completed: {} files changed", files.len())),
            &files,
        );

        let progress = mission.progress();
        self.notifier
            .notify(
                &MissionEvent::new(EventType::TaskCompleted, &mission_id)
                    .with_task(task_id)
                    .with_progress(progress.completed, progress.total),
            )
            .await;

        info!(
            mission_id = %mission_id,
            task_id = %task_id,
            "Task completed and verified"
        );

        Ok(())
    }

    pub(super) async fn finalize(&self, mission: &mut Mission) -> Result<()> {
        self.transition_state(
            mission,
            MissionState::Verifying,
            "Starting final verification",
        )
        .await?;

        let working_dir = self.isolation.working_dir(mission);

        // Build task results from mission for final coherence check
        let task_results: Vec<CoherenceTaskResult> = mission
            .tasks
            .iter()
            .filter(|t| t.status == crate::mission::TaskStatus::Completed)
            .map(|t| CoherenceTaskResult::new(&t.id, &t.description))
            .collect();

        // Run final coherence check before other verification
        self.run_final_coherence_check(mission, &task_results)
            .await?;

        let qa_result = self.agent.run_qa(mission, &working_dir).await?;

        if !qa_result.success {
            self.transition_state(mission, MissionState::Failed, "Final QA failed")
                .await?;
            return Err(VerificationError::Failed {
                message: "Final QA failed".into(),
                checks: Vec::new(),
            }
            .into());
        }

        // Collect all file changes from completed tasks for AI review context
        let (files_created, files_modified): (Vec<String>, Vec<String>) = {
            let mut created = Vec::new();
            let mut modified = Vec::new();
            for task in &mission.tasks {
                if task.status == crate::mission::TaskStatus::Completed
                    && let Some(ref result) = task.result
                {
                    created.extend(result.files_created.clone());
                    modified.extend(result.files_modified.clone());
                }
            }
            // Deduplicate
            created.sort();
            created.dedup();
            modified.sort();
            modified.dedup();
            (created, modified)
        };

        // Compute git diff for AI review (includes both modified and created files)
        let diff = GitRunner::new(&working_dir)
            .diff_for_review(&files_modified, &files_created)
            .await;

        // Run convergent verification for 2-pass requirement (NON-NEGOTIABLE)
        // This ensures the final state passes verification twice with AI review
        // Use verify_until_convergent_with_mission_changes to provide file change info AND diff
        info!(
            mission_id = %mission.id,
            files_created = files_created.len(),
            files_modified = files_modified.len(),
            has_diff = diff.is_some(),
            "Running final convergent verification with AI review for 2-pass requirement"
        );

        let convergence = self
            .convergent_verifier
            .verify_until_convergent_with_mission_changes(
                mission,
                &files_created,
                &files_modified,
                diff.as_deref(),
                &working_dir,
            )
            .await?;

        if !convergence.converged {
            let summary = format!(
                "Final convergent verification failed after {} rounds",
                convergence.total_rounds
            );
            self.transition_state(mission, MissionState::Failed, &summary)
                .await?;

            let checks: Vec<_> = convergence
                .persistent_issues
                .iter()
                .map(|i| {
                    use crate::error::{FailedCheck, FailedCheckType};
                    use crate::verification::IssueCategory;

                    let check_type = match i.category {
                        IssueCategory::BuildError => Some(FailedCheckType::Build),
                        IssueCategory::TestFailure => Some(FailedCheckType::Test),
                        IssueCategory::LintError => Some(FailedCheckType::Lint),
                        IssueCategory::TypeCheckError => Some(FailedCheckType::TypeCheck),
                        IssueCategory::MissingFile => Some(FailedCheckType::FileOperation),
                        _ => Some(FailedCheckType::Custom),
                    };

                    FailedCheck {
                        name: format!("{:?}", i.category),
                        message: i.message.clone(),
                        check_type,
                    }
                })
                .collect();

            return Err(VerificationError::Failed {
                message: summary,
                checks,
            }
            .into());
        }

        info!(
            mission_id = %mission.id,
            rounds = convergence.total_rounds,
            "Final convergent verification succeeded"
        );

        let final_message = format!("feat({}): {}", mission.id, mission.description);
        self.isolation.commit(mission, &final_message).await?;

        self.complete_mission(mission).await?;

        Ok(())
    }

    pub(super) async fn run_incremental_coherence(
        &self,
        mission: &Mission,
        task_results: &[CoherenceTaskResult],
    ) {
        if !self.config.coherence.enabled {
            return;
        }

        match self
            .coherence_checker
            .check_all(task_results, std::slice::from_ref(&mission.description))
            .await
        {
            Ok(report) => {
                if report.overall_passed {
                    debug!(
                        mission_id = %mission.id,
                        integration = report.integration_soundness.score,
                        "Incremental coherence check passed"
                    );
                } else {
                    warn!(
                        mission_id = %mission.id,
                        summary = %report.summary(),
                        "Incremental coherence issues detected"
                    );
                    for issue in report.all_issues() {
                        warn!(
                            check_type = ?issue.check_type,
                            severity = ?issue.severity,
                            description = %issue.description,
                            "Coherence issue"
                        );
                    }
                }
            }
            Err(e) => {
                warn!(mission_id = %mission.id, error = %e, "Incremental coherence check failed");
            }
        }
    }

    pub(super) async fn run_final_coherence_check(
        &self,
        mission: &Mission,
        task_results: &[CoherenceTaskResult],
    ) -> Result<()> {
        if !self.config.coherence.enabled || !self.config.coherence.run_after_all_tasks {
            return Ok(());
        }

        info!(mission_id = %mission.id, "Running final coherence check");

        let report = self
            .coherence_checker
            .check_all(task_results, std::slice::from_ref(&mission.description))
            .await?;

        if !report.overall_passed && self.config.coherence.fail_fast {
            return Err(VerificationError::Failed {
                message: report.summary(),
                checks: report
                    .all_issues()
                    .iter()
                    .map(|i| {
                        // Coherence issues are all Custom type (not build/test/lint)
                        crate::error::FailedCheck::new(
                            format!("{:?}", i.check_type),
                            i.description.clone(),
                        )
                        .with_type(crate::error::FailedCheckType::Custom)
                    })
                    .collect(),
            }
            .into());
        }

        info!(
            mission_id = %mission.id,
            passed = report.overall_passed,
            summary = %report.summary(),
            "Final coherence check complete"
        );

        Ok(())
    }
}
