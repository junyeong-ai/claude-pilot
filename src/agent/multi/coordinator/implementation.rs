//! Implementation phase execution for the Coordinator.
//!
//! Handles parallel and sequential task execution with file ownership,
//! deferred retry logic for P2P conflict resolution, and topological
//! ordering of dependent tasks.

use std::path::Path;

use tracing::{debug, info, warn};

use super::super::consensus::ConsensusTask;
use super::super::traits::{AgentTaskResult, ExecutionOutcome, TaskContext};
use super::Coordinator;
use crate::error::Result;
use crate::state::DomainEvent;

impl Coordinator {
    pub(super) async fn run_implementation_phase(
        &self,
        context: &mut TaskContext,
        tasks: Vec<ConsensusTask>,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        info!(task_count = tasks.len(), "Phase 3: Implementation");

        let (independent, dependent) = self.partition_tasks(&tasks);

        let mut final_results: Vec<AgentTaskResult> = Vec::new();
        let mut pending_tasks: Vec<&ConsensusTask> = independent;
        let max_deferred_retries = self.config.max_deferred_retries;
        let retry_delay_ms = self.config.deferred_retry_delay_ms;

        for retry_round in 0..=max_deferred_retries {
            if pending_tasks.is_empty() {
                break;
            }

            if retry_round > 0 {
                info!(
                    retry_round = retry_round,
                    pending_count = pending_tasks.len(),
                    "Retrying deferred tasks"
                );

                // Process pending conflict requests to unblock ownership transfers
                let completed_ids: Vec<String> = final_results
                    .iter()
                    .filter(|r| r.is_success())
                    .map(|r| r.task_id.clone())
                    .collect();
                self.process_pending_conflicts(&completed_ids);

                tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
            }

            let mut round_results = Vec::new();

            if self.config.parallel_execution {
                let (tasks_with_ownership, tasks_ownership_deferred) = self
                    .acquire_ownership_for_tasks(&pending_tasks, context)
                    .await;

                if !tasks_with_ownership.is_empty() {
                    let mut agent_tasks: Vec<_> = tasks_with_ownership
                        .iter()
                        .map(|(t, _)| Self::to_agent_task(t, context))
                        .collect();

                    for task in &mut agent_tasks {
                        self.enrich_task_and_emit(task).await;
                    }

                    let task_ids: Vec<_> = agent_tasks.iter().map(|t| t.id.clone()).collect();
                    let agent_ids: Vec<_> = agent_tasks
                        .iter()
                        .map(|t| {
                            let id = t
                                .role
                                .as_ref()
                                .map(|r| r.id.clone());
                            if id.is_none() {
                                warn!(task_id = %t.id, "Task missing role assignment, defaulting to coder");
                            }
                            id.unwrap_or_else(|| "coder".to_string())
                        })
                        .collect();

                    for (task, agent_id) in agent_tasks.iter().zip(agent_ids.iter()) {
                        self.broadcast_task_assignment(task, agent_id).await;
                    }

                    let parallel_results = self.pool.execute_many(agent_tasks, working_dir).await;

                    // Release ownership and collect deferred tasks that can now proceed
                    let mut ready_deferred_ids = std::collections::HashSet::new();
                    for (_task, guard) in tasks_with_ownership {
                        for deferred in guard.release_and_get_deferred() {
                            ready_deferred_ids.insert(deferred.task_id);
                        }
                    }

                    if !ready_deferred_ids.is_empty() {
                        debug!(
                            count = ready_deferred_ids.len(),
                            "Deferred tasks unblocked by ownership release"
                        );
                    }

                    for (idx, result) in parallel_results.into_iter().enumerate() {
                        let agent_id = agent_ids
                            .get(idx)
                            .cloned()
                            .unwrap_or_else(|| {
                                warn!(idx = idx, "Result index out of bounds for agent_ids, defaulting to coder");
                                "coder".to_string()
                            });
                        match result {
                            Ok(r) => {
                                self.broadcast_task_result(&r, &agent_id).await;
                                context.key_findings.extend(r.findings.clone());
                                round_results.push(r);
                            }
                            Err(e) => {
                                let task_id = task_ids
                                    .get(idx)
                                    .cloned()
                                    .unwrap_or_else(|| format!("unknown-task-{}", idx));
                                warn!(error = %e, task_id = %task_id, "Parallel task failed");
                                let failed_result =
                                    AgentTaskResult::failure(task_id, e.to_string());
                                self.broadcast_task_result(&failed_result, &agent_id).await;
                                round_results.push(failed_result);
                            }
                        }
                    }
                }

                for (task, reason) in tasks_ownership_deferred {
                    debug!(
                        task_id = %task.id,
                        reason = %reason,
                        "Executing ownership-deferred task sequentially"
                    );
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }
            } else {
                for task in &pending_tasks {
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }
            }

            let (completed, deferred): (Vec<_>, Vec<_>) = round_results
                .into_iter()
                .partition(|r| r.status != ExecutionOutcome::Deferred);

            final_results.extend(completed);

            for r in &deferred {
                self.emit_event(DomainEvent::task_deferred(
                    &context.mission_id,
                    &r.task_id,
                    &r.output,
                    "coordinator",
                ))
                .await;
            }

            if deferred.is_empty() {
                break;
            }

            let deferred_task_ids: std::collections::HashSet<_> =
                deferred.iter().map(|r| r.task_id.as_str()).collect();

            pending_tasks = tasks
                .iter()
                .filter(|t| deferred_task_ids.contains(t.id.as_str()))
                .collect();

            if retry_round == max_deferred_retries && !pending_tasks.is_empty() {
                warn!(
                    remaining_deferred = pending_tasks.len(),
                    "Max deferred retries reached, marking remaining as failed"
                );
                for task in &pending_tasks {
                    final_results.push(AgentTaskResult::failure(
                        &task.id,
                        format!(
                            "Task exhausted {} deferred retries due to persistent file conflicts",
                            max_deferred_retries
                        ),
                    ));
                }
            }
        }

        if !dependent.is_empty() {
            let dependent_vec: Vec<_> = dependent.iter().map(|t| (*t).clone()).collect();
            let sorted_deps = self.topological_sort_tasks(&dependent_vec);

            debug!(
                original_count = dependent.len(),
                sorted_count = sorted_deps.len(),
                "Executing dependent tasks in topological order"
            );

            let mut pending_deps: Vec<&ConsensusTask> = sorted_deps.to_vec();

            for retry_round in 0..=max_deferred_retries {
                if pending_deps.is_empty() {
                    break;
                }

                if retry_round > 0 {
                    info!(
                        retry_round = retry_round,
                        pending_count = pending_deps.len(),
                        "Retrying deferred dependent tasks"
                    );

                    let completed_ids: Vec<String> = final_results
                        .iter()
                        .filter(|r| r.is_success())
                        .map(|r| r.task_id.clone())
                        .collect();
                    self.process_pending_conflicts(&completed_ids);

                    tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
                }

                let mut round_results = Vec::new();
                for task in &pending_deps {
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }

                let (completed, deferred): (Vec<_>, Vec<_>) = round_results
                    .into_iter()
                    .partition(|r| r.status != ExecutionOutcome::Deferred);

                final_results.extend(completed);

                for r in &deferred {
                    self.emit_event(DomainEvent::task_deferred(
                        &context.mission_id,
                        &r.task_id,
                        &r.output,
                        "coordinator",
                    ))
                    .await;
                }

                if deferred.is_empty() {
                    break;
                }

                let deferred_ids: std::collections::HashSet<_> =
                    deferred.iter().map(|r| r.task_id.as_str()).collect();

                pending_deps = sorted_deps
                    .iter()
                    .copied()
                    .filter(|t| deferred_ids.contains(t.id.as_str()))
                    .collect();

                if retry_round == max_deferred_retries && !pending_deps.is_empty() {
                    warn!(
                        remaining_deferred = pending_deps.len(),
                        "Max deferred retries reached for dependent tasks"
                    );
                    for task in &pending_deps {
                        final_results.push(AgentTaskResult::failure(
                            &task.id,
                            format!(
                                "Task exhausted {} deferred retries due to persistent file conflicts",
                                max_deferred_retries
                            ),
                        ));
                    }
                }
            }
        }

        Ok(final_results)
    }
}
