#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::time::Instant;

use tracing::{debug, info, warn};
use uuid::Uuid;

use super::super::hierarchy::{
    ConsensusStrategy, EdgeCaseResolution, MultiWorkspaceParticipantSelector,
    ParticipantSelector,
};
use super::super::pool::AgentPool;
use super::super::shared::ConsensusOutcome;
use super::{AdaptiveConsensusExecutor, ConsensusMission, HierarchicalConsensusResult};
use crate::agent::multi::scope::AgentScope;
use crate::error::Result;
use crate::state::{DomainEvent, EventPayload};
use crate::workspace::Workspace;

impl AdaptiveConsensusExecutor {
    /// Execute consensus with adaptive strategy selection.
    pub(crate) async fn execute(
        &self,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        let session_id = format!("consensus-{}", Uuid::new_v4());
        let start_time = Instant::now();

        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = selector.select(&file_refs);

        let participants = base_participants.enrich_from_pool(
            |pool_key| pool.agent_ids_for_key(pool_key),
            Some("planning"),
        );

        debug!(
            base_count = base_participants.len(),
            enriched_count = participants.len(),
            module_count = participants.module_count(),
            "Enriched participants from pool"
        );

        if let EdgeCaseResolution::EscalateToHuman { unmapped } =
            selector.handle_unmapped_files(&file_refs)
        {
            warn!(
                unmapped_count = unmapped.len(),
                "Too many unmapped files, escalating to human"
            );
            return Ok(HierarchicalConsensusResult {
                session_id,
                outcome: ConsensusOutcome::Escalated,
                plan: None,
                tasks: vec![],
                tier_results: HashMap::new(),
                total_rounds: 0,
                duration: start_time.elapsed(),
            });
        }

        let strategy = self.strategy_selector.select(scope, &participants);

        info!(
            session_id = %session_id,
            strategy = strategy.name(),
            participant_count = strategy.participant_count(),
            tier_count = strategy.tier_count(),
            "Starting adaptive consensus"
        );

        self.emit_event(
            DomainEvent::new(
                &mission.id,
                EventPayload::HierarchicalConsensusStarted {
                    session_id: session_id.clone(),
                    mission_id: mission.id.clone(),
                    strategy: strategy.kind(),
                    participant_count: strategy.participant_count(),
                    tier_count: strategy.tier_count(),
                },
            )
            .with_correlation(&session_id),
        )
        .await;

        if let Some(ref metrics) = self.metrics {
            metrics.on_session_started(
                strategy.name(),
                strategy.participant_count(),
                strategy.tier_count(),
            );
        }

        let result = match strategy {
            ConsensusStrategy::Direct { participant } => {
                self.execute_direct(&session_id, &participant, mission, pool)
                    .await
            }
            ConsensusStrategy::Flat { participants } => {
                self.execute_flat(&session_id, &participants, mission, pool)
                    .await
            }
            ConsensusStrategy::Hierarchical { tiers } => {
                self.execute_hierarchical(&session_id, &tiers, mission, pool)
                    .await
            }
        };

        let duration = start_time.elapsed();

        let (outcome, _plan, _tasks, tier_results, total_rounds) = match &result {
            Ok(r) => (
                r.outcome,
                r.plan.clone(),
                r.tasks.clone(),
                r.tier_results.clone(),
                r.total_rounds,
            ),
            Err(_) => (ConsensusOutcome::Failed, None, vec![], HashMap::new(), 0),
        };

        self.emit_event(
            DomainEvent::new(
                &mission.id,
                EventPayload::HierarchicalConsensusCompleted {
                    session_id: session_id.clone(),
                    outcome,
                    total_tiers: tier_results.len(),
                    total_rounds,
                    duration_ms: duration.as_millis() as u64,
                },
            )
            .with_correlation(&session_id),
        )
        .await;

        if let Some(ref metrics) = self.metrics {
            metrics.on_session_completed(outcome, duration, total_rounds);
        }

        result.map(|mut r| {
            r.duration = duration;
            r
        })
    }

    /// Execute consensus with pre-established constraints from Phase 1.
    pub(crate) async fn execute_with_constraints(
        &self,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
        constraints: Vec<String>,
    ) -> Result<HierarchicalConsensusResult> {
        let session_id = format!("consensus-{}", Uuid::new_v4());
        let start_time = Instant::now();

        info!(
            session_id = %session_id,
            constraints = constraints.len(),
            "Starting adaptive consensus with Phase 1 constraints"
        );

        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = selector.select(&file_refs);

        let participants = base_participants.enrich_from_pool(
            |pool_key| pool.agent_ids_for_key(pool_key),
            Some("planning"),
        );

        if let EdgeCaseResolution::EscalateToHuman { unmapped } =
            selector.handle_unmapped_files(&file_refs)
        {
            warn!(
                unmapped_count = unmapped.len(),
                "Too many unmapped files, escalating to human"
            );
            return Ok(HierarchicalConsensusResult {
                session_id,
                outcome: ConsensusOutcome::Escalated,
                plan: None,
                tasks: vec![],
                tier_results: HashMap::new(),
                total_rounds: 0,
                duration: start_time.elapsed(),
            });
        }

        let strategy = self.strategy_selector.select(scope, &participants);

        let result = match strategy {
            ConsensusStrategy::Direct { participant } => {
                self.execute_direct(&session_id, &participant, mission, pool)
                    .await
            }
            ConsensusStrategy::Flat {
                participants: flat_participants,
            } => {
                self.execute_flat(&session_id, &flat_participants, mission, pool)
                    .await
            }
            ConsensusStrategy::Hierarchical { tiers } => {
                self.execute_with_backtracking(&session_id, &tiers, mission, pool, constraints)
                    .await
            }
        };

        let duration = start_time.elapsed();

        result.map(|mut r| {
            r.duration = duration;
            r
        })
    }

    /// Execute cross-workspace consensus with multiple workspaces.
    pub(crate) async fn execute_cross_workspace(
        &self,
        mission: &ConsensusMission,
        workspaces: &HashMap<String, &Workspace>,
        pool: &AgentPool,
        constraints: Vec<String>,
    ) -> Result<HierarchicalConsensusResult> {
        let session_id = format!("cross-ws-{}", Uuid::new_v4());
        let start_time = Instant::now();

        info!(
            session_id = %session_id,
            workspace_count = workspaces.len(),
            constraints = constraints.len(),
            "Starting cross-workspace consensus"
        );

        let mut multi_selector = MultiWorkspaceParticipantSelector::new();
        for (ws_id, workspace) in workspaces {
            multi_selector = multi_selector.with_workspace(ws_id.clone(), workspace);
        }

        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = multi_selector.select(&file_refs);

        let participants = base_participants.enrich_from_pool(
            |pool_key| pool.agent_ids_for_key(pool_key),
            Some("planning"),
        );

        let scope = AgentScope::CrossWorkspace {
            workspaces: workspaces.keys().cloned().collect(),
        };

        let strategy = self.strategy_selector.select(&scope, &participants);

        let result = match strategy {
            ConsensusStrategy::Direct { participant } => {
                self.execute_direct(&session_id, &participant, mission, pool)
                    .await
            }
            ConsensusStrategy::Flat {
                participants: flat_participants,
            } => {
                self.execute_flat(&session_id, &flat_participants, mission, pool)
                    .await
            }
            ConsensusStrategy::Hierarchical { tiers } => {
                self.execute_with_backtracking(&session_id, &tiers, mission, pool, constraints)
                    .await
            }
        };

        let duration = start_time.elapsed();

        result.map(|mut r| {
            r.duration = duration;
            r
        })
    }
}
