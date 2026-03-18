//! Session recovery logic for the Coordinator.
//!
//! Handles checkpoint-based session recovery, allowing missions to resume
//! from the last known good state after failures or restarts.

use std::collections::HashMap;
use std::path::Path;

use tracing::{info, warn};

use super::super::adaptive_consensus::{ConsensusMission, ConsensusOutcome};
use super::super::consensus::Conflict;
use super::super::shared::TierResult;
use super::super::traits::{AgentExecutionMetrics, TaskContext};
use super::types::{CheckpointInfo, ComplexityMetrics, CoordinatorMissionReport};
use super::Coordinator;
use crate::agent::multi::scope::AgentScope;
use crate::domain::Severity;
use crate::error::Result;
use crate::state::{DomainEvent, EventPayload};

impl Coordinator {
    /// Check for an incomplete session and attempt recovery.
    ///
    /// Returns `Ok(Some(result))` if session was recovered successfully,
    /// `Ok(None)` if no recovery was needed, or an error if recovery failed.
    pub async fn try_recover_session(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<Option<CoordinatorMissionReport>> {
        let Some(checkpoint) = self.find_latest_checkpoint(mission_id).await? else {
            return Ok(None);
        };

        info!(
            mission_id = %mission_id,
            checkpoint_id = %checkpoint.id,
            round = checkpoint.round,
            "Found incomplete session, attempting recovery"
        );

        let Some(ref executor) = self.adaptive_executor else {
            self.emit_recovery_failed(
                mission_id,
                Some(&checkpoint.session_id),
                &checkpoint.id,
                "Adaptive executor not configured",
            )
            .await;
            return Ok(None);
        };

        let Some(ref workspace) = self.workspace else {
            self.emit_recovery_failed(
                mission_id,
                Some(&checkpoint.session_id),
                &checkpoint.id,
                "Workspace not configured",
            )
            .await;
            return Ok(None);
        };

        let scope = AgentScope::Workspace {
            workspace: workspace.name.clone(),
        };
        let mission = ConsensusMission::new(
            description,
            vec![],
            TaskContext::new(mission_id, working_dir.to_path_buf()),
        );

        let start_time = std::time::Instant::now();

        match executor
            .resume_from_checkpoint(
                &checkpoint.id,
                &checkpoint.session_id,
                checkpoint.round,
                &mission,
                &scope,
                workspace,
                &self.pool,
            )
            .await
        {
            Ok(consensus_result) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let success = matches!(
                    consensus_result.outcome,
                    ConsensusOutcome::Converged | ConsensusOutcome::PartialConvergence
                );

                self.emit_event(
                    DomainEvent::new(
                        mission_id,
                        EventPayload::SessionRecoveryCompleted {
                            session_id: consensus_result.session_id.clone(),
                            checkpoint_id: checkpoint.id.clone(),
                            outcome: consensus_result.outcome,
                            recovered_rounds: consensus_result.total_rounds,
                            duration_ms,
                        },
                    )
                    .with_correlation(&consensus_result.session_id),
                )
                .await;

                let conflicts = Self::extract_conflicts_from_tiers(&consensus_result.tier_results);

                let tier_count = consensus_result.tier_results.len();
                let converged_tiers = consensus_result
                    .tier_results
                    .values()
                    .filter(|t| t.converged)
                    .count();

                Ok(Some(CoordinatorMissionReport {
                    success,
                    results: Vec::new(),
                    summary: consensus_result
                        .plan
                        .unwrap_or_else(|| "No consensus plan produced".to_string()),
                    metrics: AgentExecutionMetrics {
                        total_executions: consensus_result.total_rounds as u64,
                        successful_executions: converged_tiers as u64,
                        failed_executions: (tier_count - converged_tiers) as u64,
                        success_rate: if tier_count > 0 {
                            converged_tiers as f64 / tier_count as f64
                        } else {
                            0.0
                        },
                        avg_duration_ms: duration_ms,
                    },
                    conflicts,
                    complexity_metrics: ComplexityMetrics {
                        by_level: HashMap::new(),
                        total_score: 0.0,
                        completed_score: 0.0,
                        completion_pct: if success { 100.0 } else { 0.0 },
                    },
                    needs_human_decision: None,
                }))
            }
            Err(e) => {
                self.emit_recovery_failed(
                    mission_id,
                    Some(&checkpoint.session_id),
                    &checkpoint.id,
                    &e.to_string(),
                )
                .await;
                warn!(error = %e, "Session recovery failed, will start fresh");
                Ok(None)
            }
        }
    }

    async fn emit_recovery_failed(
        &self,
        mission_id: &str,
        session_id: Option<&str>,
        checkpoint_id: &str,
        reason: &str,
    ) {
        let mut event = DomainEvent::new(
            mission_id,
            EventPayload::SessionRecoveryFailed {
                session_id: session_id.map(String::from),
                checkpoint_id: checkpoint_id.to_string(),
                reason: reason.to_string(),
            },
        );
        if let Some(sid) = session_id {
            event = event.with_correlation(sid);
        }
        self.emit_event(event).await;
    }

    pub(super) fn extract_conflicts_from_tiers(
        tier_results: &HashMap<String, TierResult>,
    ) -> Vec<Conflict> {
        tier_results
            .values()
            .flat_map(|tier| {
                tier.conflicts.iter().enumerate().map(|(i, desc)| {
                    Conflict::new(
                        format!("{}-conflict-{}", tier.unit_id, i),
                        desc.as_str(),
                        Severity::Warning,
                    )
                })
            })
            .collect()
    }

    async fn find_latest_checkpoint(&self, mission_id: &str) -> Result<Option<CheckpointInfo>> {
        let Some(ref store) = self.event_store else {
            return Ok(None);
        };

        let events = store.query(mission_id, 0).await?;

        let mut latest_checkpoint: Option<CheckpointInfo> = None;
        let mut completed_sessions: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for event in &events {
            match &event.payload {
                EventPayload::ConsensusCheckpointCreated {
                    session_id,
                    checkpoint_id,
                    round,
                    ..
                } => {
                    latest_checkpoint = Some(CheckpointInfo {
                        id: checkpoint_id.clone(),
                        session_id: session_id.clone(),
                        round: *round,
                    });
                }
                EventPayload::HierarchicalConsensusCompleted { session_id, .. } => {
                    completed_sessions.insert(session_id.clone());
                }
                EventPayload::ConsensusCompleted { .. } => {}
                _ => {}
            }
        }

        match latest_checkpoint {
            Some(cp) if !completed_sessions.contains(&cp.session_id) => Ok(Some(cp)),
            _ => Ok(None),
        }
    }
}
