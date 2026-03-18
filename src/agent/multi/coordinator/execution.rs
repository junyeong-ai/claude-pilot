//! Core execution logic for the Coordinator.
//!
//! Contains mission entry points, session lifecycle management,
//! adaptive and sequential mission orchestration flows.

use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;
use tracing::{info, warn};

use super::super::adaptive_consensus::ConsensusOutcome;
use super::super::consensus::{Conflict, ConsensusResult, ConsensusTask};
use super::super::escalation::EscalationStrategy;
use super::super::session::{NotificationType, OrchestrationSession, OrchestrationConfig, SharedSession};
use super::super::traits::{
    AgentConvergenceResult, AgentRole, AgentTask, AgentTaskResult, TaskContext, TaskPriority,
    VerificationVerdict,
};
use super::types::{
    ComplexityMetrics, CoordinatorMissionReport, HealthAdaptation, HumanDecisionRequest,
};
use super::Coordinator;
use crate::error::{PilotError, Result};
use crate::state::DomainEvent;

impl Coordinator {
    /// Execute a mission using adaptive consensus when available,
    /// falling back to sequential pipeline otherwise.
    ///
    /// This method creates an OrchestrationSession for the mission, managing
    /// participant tracking, task dependencies, health monitoring, and
    /// notification-based communication throughout the mission lifecycle.
    pub async fn execute_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<CoordinatorMissionReport> {
        if !self.config.enabled {
            return Err(PilotError::AgentExecution(
                "Multi-agent mode not enabled".into(),
            ));
        }

        let mission_timeout =
            tokio::time::Duration::from_secs(self.config.consensus.total_timeout_secs);

        let result = tokio::time::timeout(
            mission_timeout,
            self.execute_mission_internal(mission_id, description, working_dir),
        )
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => {
                warn!(
                    mission_id = %mission_id,
                    timeout_secs = self.config.consensus.total_timeout_secs,
                    "Mission timed out"
                );
                Err(PilotError::Timeout(format!(
                    "Mission '{}' timed out after {} seconds",
                    mission_id, self.config.consensus.total_timeout_secs
                )))
            }
        }
    }

    async fn execute_mission_internal(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<CoordinatorMissionReport> {
        let session = self.create_mission_session(mission_id, description, working_dir);
        info!(mission_id = %mission_id, "Orchestration session created");

        let result = if self.config.dynamic_mode
            && self.adaptive_executor.is_some()
            && self.workspace.is_some()
        {
            self.execute_adaptive_mission(mission_id, description, working_dir)
                .await
        } else {
            self.execute_sequential_mission(mission_id, description, working_dir)
                .await
        };

        self.end_mission_session(
            &session,
            result.as_ref().map(|r| r.success).unwrap_or(false),
        );

        result
    }

    fn create_mission_session(
        &self,
        mission_id: &str,
        mission: &str,
        working_dir: &Path,
    ) -> SharedSession {
        let session_config = OrchestrationConfig {
            max_duration: std::time::Duration::from_secs(86400),
            max_parallel_tasks: self.config.max_concurrent_agents,
            max_task_retries: self.config.convergent.max_fix_attempts_per_issue,
            ..OrchestrationConfig::default()
        };

        let session = Arc::new(RwLock::new(
            OrchestrationSession::new(mission, working_dir.to_path_buf())
                .with_config(session_config)
                .with_metadata("mission_id", mission_id),
        ));

        {
            let mut s = session.write();
            s.notify(
                "coordinator",
                "*",
                NotificationType::Text {
                    message:
                        "Orchestration session initialized - agents will register on first task"
                            .to_string(),
                },
            );
        }

        session
    }

    fn end_mission_session(&self, session: &SharedSession, success: bool) {
        let mut s = session.write();
        if success {
            s.complete();
        } else {
            s.fail("Mission ended without success");
        }

        let summary = s.summary();
        info!(
            session_id = %summary.id,
            status = ?summary.status,
            duration_secs = summary.duration_secs,
            participant_count = summary.participant_count,
            tasks_completed = summary.task_stats.completed,
            tasks_failed = summary.task_stats.failed,
            "Session ended"
        );

        self.invocation_tracker.write().clear();
        self.continuation_manager.write().clear_all();
    }

    async fn execute_adaptive_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<CoordinatorMissionReport> {
        info!(mission_id = %mission_id, "Starting adaptive consensus mission execution");

        if let HealthAdaptation::Escalate { reason } = self.check_health_adaptation() {
            return Err(PilotError::AgentExecution(format!(
                "Health escalation at mission start: {}",
                reason
            )));
        }

        let (mut context, mut results) = self.init_mission_context(mission_id, working_dir);

        if let Some(early_return) = self
            .run_research_and_check_blockers(&mut context, &mut results, description, working_dir)
            .await?
        {
            return Ok(early_return);
        }

        let consensus_result = self
            .run_adaptive_consensus(description, &context, working_dir)
            .await?;

        self.emit_hierarchical_consensus_event(mission_id, &consensus_result)
            .await;

        let impl_tasks = self.process_hierarchical_result(&consensus_result, &mut context);

        if consensus_result.outcome == ConsensusOutcome::Escalated
            || consensus_result.outcome == ConsensusOutcome::Failed
        {
            let outcome_label = if consensus_result.outcome == ConsensusOutcome::Escalated {
                "escalated"
            } else {
                "failed"
            };
            let blocking_conflicts: Vec<Conflict> = vec![
                Conflict::new(
                    format!("consensus-{}", mission_id),
                    format!("Consensus {} after {} rounds", outcome_label, consensus_result.total_rounds),
                    crate::domain::Severity::Critical,
                )
                .with_agents(consensus_result.tier_results.keys().cloned().collect())
                .with_positions(
                    consensus_result.tier_results.values().map(|r| r.synthesis.clone()).collect(),
                ),
            ];

            let consensus_for_escalation = ConsensusResult::NoConsensus {
                summary: format!(
                    "Consensus {} after {} rounds",
                    outcome_label,
                    consensus_result.total_rounds
                ),
                blocking_conflicts: blocking_conflicts.clone(),
                respondent_count: consensus_result
                    .tier_results
                    .values()
                    .map(|r| r.respondent_count)
                    .sum(),
            };

            let strategy = self
                .escalation_engine
                .escalate(mission_id, &consensus_for_escalation)?;

            match strategy {
                EscalationStrategy::RequestHumanDecision {
                    question,
                    options,
                    context: conflict_ctx,
                } => {
                    info!(
                        mission_id = %mission_id,
                        "Escalating to human decision"
                    );
                    return Ok(CoordinatorMissionReport {
                        success: false,
                        results,
                        summary: "Human decision required - consensus could not be reached".into(),
                        metrics: self.pool.metrics(),
                        conflicts: blocking_conflicts,
                        complexity_metrics: ComplexityMetrics::default(),
                        needs_human_decision: Some(HumanDecisionRequest {
                            question,
                            options,
                            conflict_summary: conflict_ctx.conflict_summary,
                            affected_files: conflict_ctx.affected_files,
                            agent_positions: conflict_ctx.agent_positions,
                            attempted_resolutions: conflict_ctx
                                .attempted_resolutions
                                .iter()
                                .map(|r| format!("{:?}: {}", r.level, r.outcome))
                                .collect(),
                        }),
                    });
                }
                EscalationStrategy::RetryWithDifferentAgents { exclude_agents, .. } => {
                    info!(
                        mission_id = %mission_id,
                        excluded = ?exclude_agents,
                        "Retrying consensus with different agents"
                    );
                }
                EscalationStrategy::ExecuteNonConflicting {
                    safe_tasks,
                    deferred_tasks,
                } => {
                    info!(
                        mission_id = %mission_id,
                        safe = safe_tasks.len(),
                        deferred = deferred_tasks.len(),
                        "Executing non-conflicting tasks only"
                    );
                    let safe_impl_tasks = self.prepare_tasks(safe_tasks);
                    let impl_results = self
                        .run_implementation_with_events(
                            mission_id,
                            &mut context,
                            safe_impl_tasks,
                            working_dir,
                        )
                        .await?;
                    results.extend(impl_results);

                    return self
                        .run_verification_and_finalize(
                            mission_id,
                            &mut context,
                            &mut results,
                            &[],
                            working_dir,
                            blocking_conflicts,
                        )
                        .await;
                }
                EscalationStrategy::ArchitecturalOverride {
                    decision_prompt,
                    affected_modules,
                } => {
                    info!(
                        mission_id = %mission_id,
                        modules = ?affected_modules,
                        "Requesting architectural mediation"
                    );

                    if let Some(architect) = self.pool.select(&AgentRole::architect()) {
                        let architect_task = AgentTask {
                            id: format!("{}-architect-mediation", mission_id),
                            description: decision_prompt,
                            context: context.clone(),
                            priority: TaskPriority::Critical,
                            role: Some(AgentRole::architect()),
                        };

                        match architect.execute(&architect_task, working_dir).await {
                            Ok(result) => {
                                context.key_findings.push(format!(
                                    "[Architect Decision] {}",
                                    result.output.lines().next().unwrap_or("No decision")
                                ));
                                info!(
                                    mission_id = %mission_id,
                                    verdict = %result.output.lines().next().unwrap_or("unknown"),
                                    "Architect mediation completed"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    mission_id = %mission_id,
                                    error = %e,
                                    "Architect mediation failed, proceeding with extracted tasks"
                                );
                            }
                        }
                    } else {
                        warn!(
                            mission_id = %mission_id,
                            "No architect agent available for mediation"
                        );
                    }
                }
                EscalationStrategy::SequentializeConflicts {
                    task_order,
                    rationale,
                } => {
                    info!(
                        mission_id = %mission_id,
                        order = ?task_order,
                        rationale = %rationale,
                        "Sequentializing conflicting tasks"
                    );
                }
            }
        }

        let mission_tasks = impl_tasks.clone();
        let impl_results = self
            .run_implementation_with_events(mission_id, &mut context, impl_tasks, working_dir)
            .await?;
        results.extend(impl_results);

        self.run_verification_and_finalize(
            mission_id,
            &mut context,
            &mut results,
            &mission_tasks,
            working_dir,
            vec![],
        )
        .await
    }

    async fn execute_sequential_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<CoordinatorMissionReport> {
        info!(mission_id = %mission_id, "Starting sequential mission execution");

        let (mut context, mut results) = self.init_mission_context(mission_id, working_dir);

        if let Some(early_return) = self
            .run_research_and_check_blockers(&mut context, &mut results, description, working_dir)
            .await?
        {
            return Ok(early_return);
        }

        let planning_result = self
            .run_planning_phase(&mut context, description, working_dir)
            .await?;
        results.push(planning_result);

        let impl_tasks = self.extract_implementation_tasks(&context);
        let mission_tasks = impl_tasks.clone();
        let impl_results = self
            .run_implementation_with_events(mission_id, &mut context, impl_tasks, working_dir)
            .await?;
        results.extend(impl_results);

        self.run_verification_and_finalize(
            mission_id,
            &mut context,
            &mut results,
            &mission_tasks,
            working_dir,
            vec![],
        )
        .await
    }

    pub(super) fn init_mission_context(
        &self,
        mission_id: &str,
        working_dir: &Path,
    ) -> (TaskContext, Vec<AgentTaskResult>) {
        let context = TaskContext {
            mission_id: mission_id.to_string(),
            working_dir: working_dir.to_path_buf(),
            key_findings: Vec::new(),
            blockers: Vec::new(),
            related_files: Vec::new(),
            manifest_context: None,
        };
        (context, Vec::new())
    }

    async fn run_research_and_check_blockers(
        &self,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
        description: &str,
        working_dir: &Path,
    ) -> Result<Option<CoordinatorMissionReport>> {
        let research_result = self
            .run_research_phase(context, description, working_dir)
            .await?;
        results.push(research_result);

        if self.has_blockers(context) {
            return Ok(Some(CoordinatorMissionReport {
                success: false,
                results: std::mem::take(results),
                summary: "Blocked during research phase".into(),
                metrics: self.pool.metrics(),
                conflicts: vec![],
                complexity_metrics: ComplexityMetrics::default(),
                needs_human_decision: None,
            }));
        }
        Ok(None)
    }

    async fn run_implementation_with_events(
        &self,
        mission_id: &str,
        context: &mut TaskContext,
        impl_tasks: Vec<ConsensusTask>,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        let task_count = impl_tasks.len();
        let impl_start = std::time::Instant::now();
        let impl_results = self
            .run_implementation_phase(context, impl_tasks, working_dir)
            .await?;
        let impl_duration_ms = impl_start.elapsed().as_millis() as u64;
        let success_count = impl_results.iter().filter(|r| r.is_success()).count();

        self.emit_critical_event(DomainEvent::multi_agent_phase_completed(
            mission_id,
            crate::agent::multi::session::SessionPhase::Implementation,
            task_count,
            success_count,
            impl_duration_ms,
        ))
        .await;

        self.maybe_compact_task_context(context);

        Ok(impl_results)
    }

    async fn run_verification_and_finalize(
        &self,
        mission_id: &str,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
        mission_tasks: &[ConsensusTask],
        working_dir: &Path,
        conflicts: Vec<Conflict>,
    ) -> Result<CoordinatorMissionReport> {
        let verification_start = std::time::Instant::now();
        let convergence = self
            .run_convergent_verification(context, working_dir, results)
            .await?;
        let verification_duration_ms = verification_start.elapsed().as_millis() as u64;

        if convergence.converged {
            self.emit_critical_event(DomainEvent::convergence_achieved(
                mission_id,
                convergence.total_rounds as u32,
                convergence.clean_rounds as u32,
            ))
            .await;
        } else {
            let remaining_issues = convergence
                .issues_found
                .len()
                .saturating_sub(convergence.issues_fixed.len());
            self.emit_critical_event(DomainEvent::verification_round(
                mission_id,
                convergence.total_rounds as u32,
                false,
                remaining_issues,
                verification_duration_ms,
            ))
            .await;
        }

        let success = convergence.converged;
        let summary = Self::build_convergence_summary(&convergence);

        let completed_ids: Vec<String> = results
            .iter()
            .filter(|r| r.is_success())
            .map(|r| r.task_id.clone())
            .collect();

        Ok(CoordinatorMissionReport {
            success,
            results: std::mem::take(results),
            summary,
            metrics: self.pool.metrics(),
            conflicts,
            complexity_metrics: ComplexityMetrics::from_tasks(mission_tasks, &completed_ids),
            needs_human_decision: None,
        })
    }

    fn build_convergence_summary(convergence: &AgentConvergenceResult) -> String {
        let remaining = convergence
            .issues_found
            .len()
            .saturating_sub(convergence.issues_fixed.len());
        match convergence.final_verdict {
            VerificationVerdict::Passed => format!(
                "Mission completed: {} rounds, {} issues fixed",
                convergence.total_rounds,
                convergence.issues_fixed.len()
            ),
            VerificationVerdict::PartialPass => format!(
                "Mission partial: {} clean of 2 required, {} issues remain",
                convergence.clean_rounds, remaining
            ),
            VerificationVerdict::Timeout => format!(
                "Mission timeout: max rounds reached, {} issues remain",
                remaining
            ),
            VerificationVerdict::Failed => format!(
                "Mission failed: {} unresolved issues",
                convergence.issues_found.len()
            ),
        }
    }
}
