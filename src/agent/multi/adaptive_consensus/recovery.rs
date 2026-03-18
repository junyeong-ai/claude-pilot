use std::time::Instant;

use tracing::{debug, info};
use uuid::Uuid;

use super::super::hierarchy::{ConsensusStrategy, ParticipantSelector};
use super::super::pool::AgentPool;
use super::super::shared::{ConsensusOutcome, TierResult};
use super::types::AdaptiveConsensusState;
use super::{AdaptiveConsensusExecutor, ConsensusMission, HierarchicalConsensusResult};
use crate::agent::multi::scope::AgentScope;
use crate::error::Result;
use crate::state::{DomainEvent, EventPayload, EventStore};
use crate::workspace::Workspace;

impl AdaptiveConsensusExecutor {
    /// Create a checkpoint.
    pub(super) async fn create_checkpoint(
        &self,
        session_id: &str,
        state: &AdaptiveConsensusState,
        mission_id: &str,
    ) -> Result<String> {
        let checkpoint_id = format!("cp-{}-{}", session_id, Uuid::new_v4());
        let state_hash = state.content_hash();

        self.emit_event(
            DomainEvent::new(
                mission_id,
                EventPayload::ConsensusCheckpointCreated {
                    session_id: session_id.to_string(),
                    checkpoint_id: checkpoint_id.clone(),
                    round: state.current_round,
                    tier_level: state.current_tier,
                    state_hash,
                },
            )
            .with_correlation(session_id),
        )
        .await;

        debug!(
            session_id = %session_id,
            checkpoint_id = %checkpoint_id,
            "Created consensus checkpoint"
        );

        Ok(checkpoint_id)
    }

    /// Resume hierarchical consensus from a checkpoint.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resume_from_checkpoint(
        &self,
        checkpoint_id: &str,
        session_id: &str,
        checkpoint_round: usize,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        use crate::error::PilotError;

        let Some(ref store) = self.event_store else {
            return Err(PilotError::AgentExecution(
                "Event store required for checkpoint resume".into(),
            ));
        };

        info!(checkpoint_id = %checkpoint_id, session_id = %session_id, "Resuming from checkpoint");

        let state = self
            .reconstruct_state_from_session(store, session_id, checkpoint_round)
            .await?;

        info!(
            session_id = %state.session_id,
            completed_tiers = state.completed_tiers.len(),
            "State reconstructed from checkpoint"
        );

        self.emit_event(
            DomainEvent::new(
                &mission.id,
                EventPayload::SessionRecoveryStarted {
                    session_id: state.session_id.clone(),
                    checkpoint_id: checkpoint_id.to_string(),
                    recovery_round: state.current_round,
                },
            )
            .with_correlation(&state.session_id),
        )
        .await;

        self.resume_from_state(state, mission, scope, workspace, pool)
            .await
    }

    /// Reconstruct consensus state from session events using indexed query.
    async fn reconstruct_state_from_session(
        &self,
        store: &EventStore,
        session_id: &str,
        checkpoint_round: usize,
    ) -> Result<AdaptiveConsensusState> {
        let events = store.query_by_correlation(session_id).await?;

        let mut state = AdaptiveConsensusState::new(session_id.to_string());
        state.current_round = checkpoint_round;

        for event in &events {
            match &event.payload {
                EventPayload::ConsensusCheckpointCreated {
                    tier_level, round, ..
                } if *round == checkpoint_round => {
                    state.current_tier = *tier_level;
                }
                EventPayload::TierConsensusCompleted {
                    tier_level,
                    unit_id,
                    converged,
                    respondent_count,
                    ..
                } => {
                    state.completed_tiers.push(unit_id.clone());
                    let tier = *tier_level;
                    let result = if *converged {
                        TierResult::success(tier, unit_id, "(recovered)", *respondent_count, 1)
                    } else {
                        TierResult::failure(
                            tier,
                            unit_id,
                            "(recovered)",
                            *respondent_count,
                            vec![],
                            1,
                        )
                    };
                    state.tier_results.insert(unit_id.clone(), result);
                }
                _ => {}
            }
        }

        Ok(state)
    }

    /// Resume execution from a reconstructed state.
    async fn resume_from_state(
        &self,
        state: AdaptiveConsensusState,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        use crate::error::PilotError;

        let start_time = Instant::now();

        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let participants = selector.select(&file_refs);

        let strategy = self.strategy_selector.select(scope, &participants);

        let ConsensusStrategy::Hierarchical { tiers } = strategy else {
            return Err(PilotError::AgentExecution(
                "Resume only supported for hierarchical consensus".into(),
            ));
        };

        let session_id = state.session_id.clone();
        let completed_tiers = state.completed_tiers.clone();
        let completed_units: std::collections::HashSet<String> =
            completed_tiers.into_iter().collect();
        let mut all_results = state.tier_results.clone();
        let mut current_state = state;
        let mut total_rounds = 0;

        for tier in &tiers {
            let tier_complete = tier.units.iter().all(|u| completed_units.contains(&u.id));
            if tier_complete {
                continue;
            }

            let tier_results = self
                .execute_tier(
                    &session_id,
                    tier,
                    &all_results,
                    mission,
                    pool,
                    &mut current_state,
                )
                .await?;

            for result in tier_results.values() {
                total_rounds += result.rounds;
            }
            all_results.extend(tier_results);
        }

        let final_tier = tiers.last();
        let (outcome, plan) = if let Some(tier) = final_tier {
            let top_results: Vec<_> = all_results
                .values()
                .filter(|r| r.tier_level == tier.level)
                .collect();

            if top_results.iter().all(|r| r.converged) {
                let synthesis = top_results
                    .iter()
                    .map(|r| r.synthesis.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (ConsensusOutcome::Converged, Some(synthesis))
            } else {
                (ConsensusOutcome::PartialConvergence, None)
            }
        } else {
            (ConsensusOutcome::Failed, None)
        };

        let tasks = Self::collect_tasks_from_tier_results(&all_results);

        Ok(HierarchicalConsensusResult {
            session_id: current_state.session_id,
            outcome,
            plan,
            tasks,
            tier_results: all_results,
            total_rounds,
            duration: start_time.elapsed(),
        })
    }
}
