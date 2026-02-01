//! Adaptive consensus executor with hierarchical support.
//!
//! This module provides the main executor for adaptive consensus, automatically
//! choosing between direct, flat, or hierarchical consensus based on participant count.

// Allow complex function signatures for hierarchical consensus orchestration
#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::timeout;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::consensus::{
    Conflict, ConflictSeverity, ConsensusEngine, ConsensusResult, ConsensusTask,
};
use super::hierarchy::{
    AgentId, ConsensusStrategy, ConsensusTier, ConsensusUnit, EdgeCaseResolution,
    MultiWorkspaceParticipantSelector, ParticipantSelector, StrategySelector, TierLevel,
};
use super::metrics::MetricsObserver;
use super::pool::AgentPool;
pub use super::shared::{ConsensusOutcome, SessionStatus, TierResult};
use super::traits::TaskContext;
use crate::config::ConsensusConfig;
use crate::error::Result;
use crate::orchestration::AgentScope;
use crate::state::{DomainEvent, EventPayload, EventStore};
use crate::workspace::Workspace;

/// Mission context for consensus.
pub struct ConsensusMission {
    pub id: String,
    pub description: String,
    pub affected_files: Vec<PathBuf>,
    pub context: TaskContext,
}

impl ConsensusMission {
    pub fn new(description: impl Into<String>, files: Vec<PathBuf>, context: TaskContext) -> Self {
        Self {
            id: format!("mission-{}", Uuid::new_v4()),
            description: description.into(),
            affected_files: files,
            context,
        }
    }
}

/// Result of hierarchical consensus execution.
#[derive(Debug)]
pub struct HierarchicalConsensusResult {
    pub session_id: String,
    pub outcome: ConsensusOutcome,
    pub plan: Option<String>,
    pub tasks: Vec<ConsensusTask>,
    pub tier_results: HashMap<String, TierResult>,
    pub total_rounds: usize,
    pub duration: Duration,
}

const DEFAULT_TIER_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_RATIO: f64 = 0.5;

/// State for checkpoint/recovery.
#[derive(Debug, Clone)]
pub struct ConsensusState {
    pub session_id: String,
    pub current_round: usize,
    pub current_tier: Option<TierLevel>,
    pub completed_tiers: Vec<String>,
    pub tier_results: HashMap<String, TierResult>,
    pub status: SessionStatus,
}

impl ConsensusState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            current_round: 0,
            current_tier: None,
            completed_tiers: Vec::new(),
            tier_results: HashMap::new(),
            status: SessionStatus::Running,
        }
    }

    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.session_id.as_bytes());
        hasher.update(self.current_round.to_le_bytes());

        // Current tier
        if let Some(ref tier) = self.current_tier {
            let tier_str = tier.as_str();
            hasher.update([1u8]);
            hasher.update((tier_str.len() as u32).to_le_bytes());
            hasher.update(tier_str.as_bytes());
        } else {
            hasher.update([0u8]);
        }

        // Completed tiers (length-prefixed)
        hasher.update((self.completed_tiers.len() as u32).to_le_bytes());
        for tier in &self.completed_tiers {
            hasher.update((tier.len() as u32).to_le_bytes());
            hasher.update(tier.as_bytes());
        }

        // Tier results (sorted for determinism)
        let mut result_keys: Vec<_> = self.tier_results.keys().collect();
        result_keys.sort();
        hasher.update((result_keys.len() as u32).to_le_bytes());
        for key in result_keys {
            hasher.update((key.len() as u32).to_le_bytes());
            hasher.update(key.as_bytes());
            if let Some(r) = self.tier_results.get(key) {
                hasher.update(r.tier_level.as_str().as_bytes());
                hasher.update([r.converged as u8]);
                hasher.update([r.timed_out as u8]);
                hasher.update((r.respondent_count as u32).to_le_bytes());
                hasher.update((r.rounds as u32).to_le_bytes());
                hasher.update((r.synthesis.len() as u32).to_le_bytes());
                hasher.update(r.synthesis.as_bytes());
                hasher.update((r.conflicts.len() as u32).to_le_bytes());
                for conflict in &r.conflicts {
                    hasher.update((conflict.len() as u32).to_le_bytes());
                    hasher.update(conflict.as_bytes());
                }
            }
        }

        // Session status
        hasher.update(self.status.as_str().as_bytes());

        format!("{:x}", hasher.finalize())
    }
}

/// Adaptive consensus executor.
pub struct AdaptiveConsensusExecutor {
    strategy_selector: StrategySelector,
    consensus_engine: ConsensusEngine,
    event_store: Option<Arc<EventStore>>,
    metrics: Option<Arc<dyn MetricsObserver>>,
    config: ConsensusConfig,
}

impl AdaptiveConsensusExecutor {
    pub fn new(consensus_engine: ConsensusEngine, config: ConsensusConfig) -> Self {
        Self {
            strategy_selector: StrategySelector::new(config.clone()),
            consensus_engine,
            event_store: None,
            metrics: None,
            config,
        }
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_metrics(mut self, metrics: Arc<dyn MetricsObserver>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Build a complete synthesis plan from all tier results.
    ///
    /// Includes all tiers from bottom to top, providing full context
    /// for implementation rather than just top-tier synthesis.
    fn build_full_synthesis(
        &self,
        tier_results: &HashMap<String, TierResult>,
        tiers: &[ConsensusTier],
    ) -> String {
        let mut sections = Vec::new();

        // Process tiers in order (bottom to top)
        for tier in tiers {
            let tier_units: Vec<_> = tier_results
                .values()
                .filter(|r| r.tier_level == tier.level)
                .collect();

            if tier_units.is_empty() {
                continue;
            }

            let converged_count = tier_units.iter().filter(|r| r.converged).count();
            let total = tier_units.len();

            let unit_syntheses: Vec<_> = tier_units
                .iter()
                .map(|r| {
                    let status = if r.converged { "✓" } else { "~" };
                    format!("### {} [{}]\n{}", r.unit_id, status, r.synthesis)
                })
                .collect();

            sections.push(format!(
                "## {} Tier ({}/{} converged)\n\n{}",
                tier.level,
                converged_count,
                total,
                unit_syntheses.join("\n\n")
            ));
        }

        if sections.is_empty() {
            "No tier results available.".to_string()
        } else {
            sections.join("\n\n---\n\n")
        }
    }

    /// Determine the consensus outcome from all tier results.
    fn determine_outcome(
        &self,
        tier_results: &HashMap<String, TierResult>,
        tiers: &[ConsensusTier],
    ) -> ConsensusOutcome {
        if tier_results.is_empty() {
            return ConsensusOutcome::Failed;
        }

        // Check if all tiers converged
        let all_converged = tier_results.values().all(|r| r.converged);
        if all_converged {
            return ConsensusOutcome::Converged;
        }

        // Check if top tier converged (even if lower tiers had issues)
        if let Some(top_tier) = tiers.last() {
            let top_converged = tier_results
                .values()
                .filter(|r| r.tier_level == top_tier.level)
                .all(|r| r.converged);

            if top_converged {
                return ConsensusOutcome::PartialConvergence;
            }
        }

        // Top tier didn't converge - this is a failure
        ConsensusOutcome::Failed
    }

    /// Execute consensus with adaptive strategy selection.
    pub async fn execute(
        &self,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        let session_id = format!("consensus-{}", Uuid::new_v4());
        let start_time = Instant::now();

        // 1. Select participants deterministically from manifest
        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = selector.select(&file_refs);

        // 2. Enrich with planning agents from pool for Phase 2 consensus
        // Only planning agents participate in consensus; coders execute in Phase 3
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

        // Handle unmapped files
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

        // 2. Select strategy based on scale
        let strategy = self.strategy_selector.select(scope, &participants);

        info!(
            session_id = %session_id,
            strategy = strategy.name(),
            participant_count = strategy.participant_count(),
            tier_count = strategy.tier_count(),
            "Starting adaptive consensus"
        );

        // 3. Emit session started event and record metrics
        self.emit_event(
            DomainEvent::new(
                &mission.id,
                EventPayload::HierarchicalConsensusStarted {
                    session_id: session_id.clone(),
                    mission_id: mission.id.clone(),
                    strategy: strategy.name().to_string(),
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

        // 4. Execute based on strategy
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

        // 5. Emit completion event
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
                    outcome: outcome.as_str().to_string(),
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
    ///
    /// This method is called by the coordinator after the two-phase orchestrator
    /// has established global constraints (tech decisions, API contracts, etc.).
    /// It uses backtracking to handle conflicts that arise during synthesis.
    pub async fn execute_with_constraints(
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

        // 1. Select participants deterministically from manifest
        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = selector.select(&file_refs);

        // 2. Enrich with planning agents from pool for Phase 2 consensus
        let participants = base_participants.enrich_from_pool(
            |pool_key| pool.agent_ids_for_key(pool_key),
            Some("planning"),
        );

        // Handle unmapped files
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

        // 3. Select strategy based on scale
        let strategy = self.strategy_selector.select(scope, &participants);

        // 4. Execute with backtracking (using constraints)
        let result = match strategy {
            ConsensusStrategy::Direct { participant } => {
                self.execute_direct(&session_id, &participant, mission, pool)
                    .await
            }
            ConsensusStrategy::Flat {
                participants: flat_participants,
            } => {
                // For flat, inject constraints into context
                self.execute_flat(&session_id, &flat_participants, mission, pool)
                    .await
            }
            ConsensusStrategy::Hierarchical { tiers } => {
                // Use backtracking execution with Phase 1 constraints
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
    ///
    /// Used when affected files span multiple workspaces/projects.
    /// Automatically builds hierarchical consensus with workspace coordinators.
    pub async fn execute_cross_workspace(
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

        // 1. Build multi-workspace participant selector using builder pattern
        let mut multi_selector = MultiWorkspaceParticipantSelector::new();
        for (ws_id, workspace) in workspaces {
            multi_selector = multi_selector.with_workspace(ws_id.clone(), workspace);
        }

        // 2. Select participants across all workspaces
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let base_participants = multi_selector.select(&file_refs);

        // 3. Enrich with planning agents from pool for Phase 2 consensus
        let participants = base_participants.enrich_from_pool(
            |pool_key| pool.agent_ids_for_key(pool_key),
            Some("planning"),
        );

        // 4. Cross-workspace scope triggers hierarchical with cross-workspace tier
        let scope = AgentScope::CrossWorkspace {
            workspaces: workspaces.keys().cloned().collect(),
        };

        // 5. Select strategy (will include CrossWorkspace tier)
        let strategy = self.strategy_selector.select(&scope, &participants);

        // 6. Execute with backtracking
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

    /// Unpack consensus result into components: (outcome, plan, tasks, rounds).
    fn unpack_consensus_result(
        &self,
        result: ConsensusResult,
    ) -> (ConsensusOutcome, Option<String>, Vec<ConsensusTask>, usize) {
        match result {
            ConsensusResult::Agreed {
                plan,
                tasks,
                rounds,
                ..
            } => (ConsensusOutcome::Converged, Some(plan), tasks, rounds),
            ConsensusResult::PartialAgreement { plan, .. } => (
                ConsensusOutcome::PartialConvergence,
                Some(plan),
                vec![],
                self.config.max_rounds,
            ),
            ConsensusResult::NoConsensus { summary, .. } => (
                ConsensusOutcome::Failed,
                Some(summary),
                vec![],
                self.config.max_rounds,
            ),
        }
    }

    /// Execute direct (single participant, no consensus).
    async fn execute_direct(
        &self,
        session_id: &str,
        participant: &AgentId,
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        debug!(
            session_id = %session_id,
            participant = %participant,
            "Executing direct (no consensus needed)"
        );

        // Single participant proposes directly via existing consensus engine
        // but with only one participant, it effectively bypasses voting
        let result = self
            .consensus_engine
            .run_with_dynamic_joining(
                &mission.description,
                &mission.context,
                std::slice::from_ref(participant),
                pool,
                |_| vec![], // No dynamic joining for direct
            )
            .await?;

        let (outcome, plan, tasks, _) = self.unpack_consensus_result(result);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: HashMap::new(),
            total_rounds: 1, // Direct execution is always 1 round
            duration: Duration::ZERO,
        })
    }

    /// Execute flat consensus.
    async fn execute_flat(
        &self,
        session_id: &str,
        participants: &[AgentId],
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        info!(
            session_id = %session_id,
            participant_count = participants.len(),
            "Executing flat consensus"
        );

        let result = self
            .consensus_engine
            .run_with_dynamic_joining(
                &mission.description,
                &mission.context,
                participants,
                pool,
                |requests| Self::resolve_agent_requests(requests, pool),
            )
            .await?;

        let (outcome, plan, tasks, rounds) = self.unpack_consensus_result(result);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: HashMap::new(),
            total_rounds: rounds,
            duration: Duration::ZERO,
        })
    }

    /// Execute hierarchical consensus (bottom-up).
    async fn execute_hierarchical(
        &self,
        session_id: &str,
        tiers: &[ConsensusTier],
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        let start_time = Instant::now();

        // Log detailed tier structure for debugging
        let tier_summary: Vec<_> = tiers
            .iter()
            .map(|t| format!("{}({}u)", t.level.as_str(), t.units.len()))
            .collect();

        info!(
            session_id = %session_id,
            tier_count = tiers.len(),
            tier_structure = %tier_summary.join(" → "),
            mission_id = %mission.id,
            affected_files = mission.affected_files.len(),
            "Starting hierarchical consensus (bottom-up)"
        );

        let mut state = ConsensusState::new(session_id.to_string());
        let mut all_tier_results = HashMap::new();
        let mut total_rounds = 0;

        // Execute tiers sequentially from lowest to highest (bottom-up)
        for (tier_idx, tier) in tiers.iter().enumerate() {
            let tier_start = Instant::now();
            let participant_count: usize = tier.units.iter().map(|u| u.participants.len()).sum();

            info!(
                session_id = %session_id,
                tier_idx = tier_idx,
                tier_level = %tier.level.as_str(),
                unit_count = tier.units.len(),
                participant_count = participant_count,
                elapsed_ms = start_time.elapsed().as_millis(),
                "Executing tier"
            );

            let tier_results = self
                .execute_tier(
                    session_id,
                    tier,
                    &all_tier_results,
                    mission,
                    pool,
                    &mut state,
                )
                .await?;

            // Accumulate actual rounds from each unit in this tier
            for result in tier_results.values() {
                total_rounds += result.rounds;
            }

            let converged_count = tier_results.values().filter(|r| r.converged).count();
            info!(
                session_id = %session_id,
                tier_level = %tier.level.as_str(),
                units_completed = tier_results.len(),
                converged = converged_count,
                tier_duration_ms = tier_start.elapsed().as_millis(),
                "Tier completed"
            );

            all_tier_results.extend(tier_results);

            // Checkpoint after each tier
            if self.config.checkpoint_on_tier_complete {
                self.create_checkpoint(session_id, &state, &mission.id)
                    .await?;
            }
        }

        // Synthesize final result including all tiers
        let outcome = self.determine_outcome(&all_tier_results, tiers);
        let plan = if !all_tier_results.is_empty() {
            Some(self.build_full_synthesis(&all_tier_results, tiers))
        } else {
            None
        };

        let total_duration = start_time.elapsed();
        let converged_tiers = all_tier_results.values().filter(|r| r.converged).count();

        info!(
            session_id = %session_id,
            outcome = ?outcome,
            total_rounds = total_rounds,
            tiers_completed = all_tier_results.len(),
            tiers_converged = converged_tiers,
            duration_ms = total_duration.as_millis(),
            "Hierarchical consensus completed"
        );

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks: vec![],
            tier_results: all_tier_results,
            total_rounds,
            duration: total_duration,
        })
    }

    /// Execute hierarchical consensus with backtracking support.
    ///
    /// When upper tiers detect conflicts, constraints are generated and
    /// lower tiers are re-executed with the new constraints.
    async fn execute_with_backtracking(
        &self,
        session_id: &str,
        tiers: &[ConsensusTier],
        mission: &ConsensusMission,
        pool: &AgentPool,
        initial_constraints: Vec<String>,
    ) -> Result<HierarchicalConsensusResult> {
        const MAX_BACKTRACKS: usize = 3;

        info!(
            session_id = %session_id,
            tier_count = tiers.len(),
            constraints = initial_constraints.len(),
            "Executing hierarchical consensus with backtracking"
        );

        let mut state = ConsensusState::new(session_id.to_string());
        let mut all_tier_results: HashMap<String, TierResult> = HashMap::new();
        let mut active_constraints = initial_constraints;
        let mut tier_idx = 0;
        let mut backtrack_count = 0;
        let mut total_rounds = 0;

        while tier_idx < tiers.len() {
            let tier = &tiers[tier_idx];

            // Execute tier with current constraints
            let tier_results = self
                .execute_tier_internal(
                    session_id,
                    tier,
                    &all_tier_results,
                    mission,
                    pool,
                    &mut state,
                    &active_constraints,
                )
                .await?;

            // Accumulate rounds
            for result in tier_results.values() {
                total_rounds += result.rounds;
            }

            // Check for cross-tier conflicts
            let conflicts = self.detect_cross_tier_conflicts(&tier_results, &all_tier_results);

            if !conflicts.is_empty() && tier_idx > 0 && backtrack_count < MAX_BACKTRACKS {
                // Generate constraints from all conflicts
                let new_constraints = self.conflicts_to_constraints(&conflicts);
                let constraint_count = new_constraints.len();
                active_constraints.extend(new_constraints);

                // Clear results from tiers we're re-running
                for t in &tiers[..tier_idx] {
                    for unit in &t.units {
                        all_tier_results.remove(&unit.id);
                    }
                }

                // Backtrack to start
                tier_idx = 0;
                backtrack_count += 1;

                info!(
                    backtrack = backtrack_count,
                    new_constraints = constraint_count,
                    total_constraints = active_constraints.len(),
                    "Backtracking due to cross-tier conflicts"
                );
                continue;
            }

            // No conflict or max backtracks reached - proceed
            all_tier_results.extend(tier_results);
            tier_idx += 1;

            // Checkpoint after each tier
            if self.config.checkpoint_on_tier_complete {
                self.create_checkpoint(session_id, &state, &mission.id)
                    .await?;
            }
        }

        // Synthesize final result including all tiers
        let outcome = self.determine_outcome(&all_tier_results, tiers);
        let plan = if !all_tier_results.is_empty() {
            Some(self.build_full_synthesis(&all_tier_results, tiers))
        } else {
            None
        };

        if backtrack_count > 0 {
            info!(
                backtrack_count = backtrack_count,
                "Consensus completed with backtracking"
            );
        }

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks: vec![],
            tier_results: all_tier_results,
            total_rounds,
            duration: Duration::ZERO,
        })
    }

    /// Execute a consensus unit with peer context and constraints.
    async fn execute_unit_with_context(
        &self,
        session_id: &str,
        tier_level: TierLevel,
        unit: &ConsensusUnit,
        lower_tier_results: &HashMap<String, TierResult>,
        mission: &ConsensusMission,
        pool: &AgentPool,
        peer_context: &str,
        constraints: &[String],
    ) -> Result<TierResult> {
        debug!(
            session_id = %session_id,
            tier = %tier_level,
            unit_id = %unit.id,
            cross_visibility = unit.cross_visibility,
            "Executing unit with context"
        );

        // Build enhanced context
        let mut enhanced_context = mission.context.clone();

        // Add lower tier results
        for result in lower_tier_results.values() {
            enhanced_context.key_findings.push(format!(
                "[{} tier - {}] {}",
                result.tier_level.as_str(),
                result.unit_id,
                result.synthesis.lines().next().unwrap_or("No synthesis")
            ));
        }

        // Add peer context
        if !peer_context.is_empty() {
            enhanced_context
                .key_findings
                .push(format!("[Peer Context]\n{}", peer_context));
        }

        // Add constraints
        if !constraints.is_empty() {
            enhanced_context.key_findings.push(format!(
                "[Active Constraints]\n{}",
                constraints
                    .iter()
                    .map(|c| format!("- {}", c))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }

        let result = if unit.is_collective() {
            debug!(
                unit_id = %unit.id,
                participants = unit.participants.len(),
                "Using cross-visibility consensus"
            );
            self.execute_with_cross_visibility(&unit.participants, mission, &enhanced_context, pool)
                .await?
        } else {
            self.consensus_engine
                .run_with_dynamic_joining(
                    &mission.description,
                    &enhanced_context,
                    &unit.participants,
                    pool,
                    |requests| Self::resolve_agent_requests(requests, pool),
                )
                .await?
        };

        let (converged, synthesis, conflicts, rounds) = match result {
            super::consensus::ConsensusResult::Agreed { plan, rounds, .. } => {
                (true, plan, vec![], rounds)
            }
            super::consensus::ConsensusResult::PartialAgreement {
                plan,
                unresolved_conflicts,
                ..
            } => (
                false,
                plan,
                unresolved_conflicts
                    .iter()
                    .map(|c| c.topic.clone())
                    .collect(),
                1,
            ),
            super::consensus::ConsensusResult::NoConsensus {
                summary,
                blocking_conflicts,
                ..
            } => (
                false,
                summary,
                blocking_conflicts.iter().map(|c| c.topic.clone()).collect(),
                1,
            ),
        };

        if converged {
            Ok(TierResult::success(
                tier_level,
                unit.id.clone(),
                synthesis,
                unit.participants.len(),
                rounds,
            ))
        } else {
            Ok(TierResult::failure(
                tier_level,
                unit.id.clone(),
                synthesis,
                unit.participants.len(),
                conflicts,
                rounds,
            ))
        }
    }

    /// Execute consensus with intra-unit cross-visibility.
    ///
    /// All participants see each other's proposals in real-time and can refine
    /// their approaches based on what others propose.
    async fn execute_with_cross_visibility(
        &self,
        participants: &[AgentId],
        mission: &ConsensusMission,
        context: &TaskContext,
        pool: &AgentPool,
    ) -> Result<super::consensus::ConsensusResult> {
        // Use the cross-visibility collection from ConsensusEngine
        let enhanced_proposals = self
            .consensus_engine
            .collect_with_cross_visibility(
                participants,
                &mission.description,
                context,
                pool,
                self.config.max_rounds,
            )
            .await?;

        // Synthesize consensus result using semantic convergence
        self.synthesize_from_enhanced_proposals(&enhanced_proposals, mission)
    }

    /// Synthesize consensus result from enhanced proposals using semantic convergence.
    fn synthesize_from_enhanced_proposals(
        &self,
        proposals: &[super::consensus::EnhancedProposal],
        _mission: &ConsensusMission,
    ) -> Result<ConsensusResult> {
        use super::convergence::SemanticConvergenceChecker;
        use super::shared::ConvergenceCheckResult;

        if proposals.is_empty() {
            return Ok(ConsensusResult::NoConsensus {
                summary: "No proposals received".to_string(),
                blocking_conflicts: vec![],
                respondent_count: 0,
            });
        }

        // Convert to semantic proposals and check convergence
        let checker = SemanticConvergenceChecker::new();
        let semantic_proposals: Vec<_> =
            proposals.iter().map(|p| p.to_semantic_proposal()).collect();

        let convergence = checker.check_convergence(&semantic_proposals);

        match convergence {
            ConvergenceCheckResult::Converged {
                score,
                minor_conflicts,
            } => {
                let plan = self.build_plan_from_proposals(proposals);
                info!(
                    score = score,
                    minor_conflicts = minor_conflicts.len(),
                    "Cross-visibility consensus converged"
                );
                Ok(ConsensusResult::Agreed {
                    plan,
                    tasks: vec![], // Tasks extracted separately
                    rounds: 1,
                    respondent_count: proposals.len(),
                })
            }
            ConvergenceCheckResult::Blocked { conflicts } => {
                warn!(
                    conflicts = conflicts.len(),
                    "Cross-visibility consensus blocked"
                );
                Ok(ConsensusResult::NoConsensus {
                    summary: format!("Blocked by {} semantic conflicts", conflicts.len()),
                    blocking_conflicts: conflicts
                        .into_iter()
                        .map(|c| Conflict {
                            id: format!("conflict-{}", uuid::Uuid::new_v4()),
                            agents: c.agents_involved.clone(),
                            topic: c.description,
                            positions: c.agents_involved,
                            severity: ConflictSeverity::Blocking,
                            resolution: None,
                        })
                        .collect(),
                    respondent_count: proposals.len(),
                })
            }
            ConvergenceCheckResult::NeedsMoreRounds {
                current_score,
                conflicts,
            } => {
                let plan = self.build_plan_from_proposals(proposals);
                info!(
                    score = current_score,
                    conflicts = conflicts.len(),
                    "Cross-visibility partial agreement"
                );
                Ok(ConsensusResult::PartialAgreement {
                    plan,
                    dissents: vec![],
                    unresolved_conflicts: conflicts
                        .into_iter()
                        .map(|c| Conflict {
                            id: format!("conflict-{}", uuid::Uuid::new_v4()),
                            agents: c.agents_involved.clone(),
                            topic: c.description,
                            positions: c.agents_involved,
                            severity: ConflictSeverity::Moderate,
                            resolution: None,
                        })
                        .collect(),
                    respondent_count: proposals.len(),
                })
            }
        }
    }

    /// Build a synthesized plan from enhanced proposals.
    fn build_plan_from_proposals(
        &self,
        proposals: &[super::consensus::EnhancedProposal],
    ) -> String {
        let mut plan = String::from("## Synthesized Plan\n\n");

        for proposal in proposals {
            plan.push_str(&format!(
                "### {} ({})\n\n**Summary**: {}\n\n**Approach**: {}\n\n",
                proposal.agent_id, proposal.module_id, proposal.summary, proposal.approach
            ));

            if !proposal.api_changes.is_empty() {
                plan.push_str("**API Changes**:\n");
                for api in &proposal.api_changes {
                    plan.push_str(&format!("- {}: {}\n", api.name, api.change_type));
                }
                plan.push('\n');
            }

            if !proposal.dependencies_provided.is_empty() {
                plan.push_str(&format!(
                    "**Provides**: {}\n\n",
                    proposal.dependencies_provided.join(", ")
                ));
            }
        }

        plan
    }

    /// Build peer context from completed units in the same tier.
    fn build_peer_context(
        &self,
        tier: &ConsensusTier,
        completed: &HashMap<String, TierResult>,
    ) -> String {
        let mut context = String::new();

        for unit in &tier.units {
            if let Some(result) = completed.get(&unit.id) {
                context.push_str(&format!(
                    "## {} ({})\n{}\n\n",
                    unit.id,
                    if result.converged {
                        "converged"
                    } else {
                        "partial"
                    },
                    result
                        .synthesis
                        .lines()
                        .take(5)
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }

        context
    }

    /// Detect conflicts between tier results.
    fn detect_cross_tier_conflicts(
        &self,
        new_results: &HashMap<String, TierResult>,
        existing_results: &HashMap<String, TierResult>,
    ) -> Vec<String> {
        let mut conflicts = Vec::new();

        // Check for synthesis conflicts
        for (unit_id, new_result) in new_results {
            if !new_result.converged {
                for conflict in &new_result.conflicts {
                    // Check if this conflict relates to existing results
                    for existing in existing_results.values() {
                        if existing.synthesis.contains(conflict) {
                            conflicts.push(format!(
                                "Cross-tier conflict: '{}' conflicts with {} tier result",
                                conflict, existing.tier_level
                            ));
                        }
                    }
                }
            }

            // Also flag non-converged units in upper tiers as potential cross-tier issues
            if !new_result.converged && !new_result.conflicts.is_empty() {
                conflicts.push(format!(
                    "Unit {} has unresolved conflicts: {}",
                    unit_id,
                    new_result.conflicts.join(", ")
                ));
            }
        }

        conflicts
    }

    /// Generate a constraint from detected conflicts.
    /// Generate constraints from detected conflicts.
    ///
    /// Returns a vector of constraint strings, one per conflict.
    fn conflicts_to_constraints(&self, conflicts: &[String]) -> Vec<String> {
        conflicts
            .iter()
            .map(|c| format!("MUST RESOLVE: {}", c))
            .collect()
    }

    /// Execute a single tier with timeout handling for each unit.
    ///
    /// Supports cross-visibility (intra-unit proposal sharing) and optional constraints.
    async fn execute_tier(
        &self,
        session_id: &str,
        tier: &ConsensusTier,
        lower_tier_results: &HashMap<String, TierResult>,
        mission: &ConsensusMission,
        pool: &AgentPool,
        state: &mut ConsensusState,
    ) -> Result<HashMap<String, TierResult>> {
        // Delegate to the full implementation with empty constraints
        self.execute_tier_internal(
            session_id,
            tier,
            lower_tier_results,
            mission,
            pool,
            state,
            &[],
        )
        .await
    }

    /// Internal tier execution with full feature support.
    async fn execute_tier_internal(
        &self,
        session_id: &str,
        tier: &ConsensusTier,
        lower_tier_results: &HashMap<String, TierResult>,
        mission: &ConsensusMission,
        pool: &AgentPool,
        state: &mut ConsensusState,
        constraints: &[String],
    ) -> Result<HashMap<String, TierResult>> {
        let mut results = HashMap::new();
        let mut timed_out_units = Vec::new();

        let unit_timeout = if self.config.tier_timeout_secs > 0 {
            Duration::from_secs(self.config.tier_timeout_secs)
        } else {
            Duration::from_secs(DEFAULT_TIER_TIMEOUT_SECS)
        };

        if let Some(ref metrics) = self.metrics {
            metrics.on_tier_started(tier.level, tier.units.len());
        }

        for unit in &tier.units {
            self.emit_event(
                DomainEvent::new(
                    &mission.id,
                    EventPayload::TierConsensusStarted {
                        session_id: session_id.to_string(),
                        tier_level: tier.level.as_str().to_string(),
                        unit_id: unit.id.clone(),
                        participants: unit.participants.iter().map(|id| id.to_string()).collect(),
                    },
                )
                .with_correlation(session_id),
            )
            .await;

            // Build peer context for cross-visibility
            let peer_context = if unit.cross_visibility {
                self.build_peer_context(tier, &results)
            } else {
                String::new()
            };

            let unit_result = match timeout(
                unit_timeout,
                self.execute_unit_with_context(
                    session_id,
                    tier.level,
                    unit,
                    lower_tier_results,
                    mission,
                    pool,
                    &peer_context,
                    constraints,
                ),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(e)) => {
                    warn!(
                        unit_id = %unit.id,
                        error = %e,
                        "Unit consensus failed"
                    );
                    TierResult::failure(
                        tier.level,
                        unit.id.clone(),
                        format!("Failed: {}", e),
                        unit.participants.len(),
                        vec![e.to_string()],
                        1,
                    )
                }
                Err(_) => {
                    warn!(
                        unit_id = %unit.id,
                        timeout_secs = unit_timeout.as_secs(),
                        "Unit consensus timed out"
                    );
                    timed_out_units.push(unit.id.clone());
                    TierResult::timeout(tier.level, unit.id.clone(), unit.participants.len())
                }
            };

            self.emit_event(
                DomainEvent::new(
                    &mission.id,
                    EventPayload::TierConsensusCompleted {
                        session_id: session_id.to_string(),
                        tier_level: tier.level.as_str().to_string(),
                        unit_id: unit.id.clone(),
                        synthesis_hash: sha256_hash(&unit_result.synthesis),
                        respondent_count: unit_result.respondent_count,
                        converged: unit_result.converged,
                    },
                )
                .with_correlation(session_id),
            )
            .await;

            if let Some(ref metrics) = self.metrics {
                metrics.on_tier_completed(tier.level, unit_result.converged, unit_result.timed_out);
            }

            results.insert(unit.id.clone(), unit_result);
        }

        // Check timeout ratio for tier health assessment
        // Design: Use soft degradation rather than hard failure when many units timeout.
        // Rationale: Partial consensus results are still valuable for downstream processing.
        // The caller (coordinator) can inspect tier_results to decide on escalation.
        let timeout_ratio = if tier.units.is_empty() {
            0.0
        } else {
            timed_out_units.len() as f64 / tier.units.len() as f64
        };

        if timeout_ratio >= MAX_TIMEOUT_RATIO {
            warn!(
                tier = %tier.level,
                timed_out = timed_out_units.len(),
                total = tier.units.len(),
                ratio = %format!("{:.1}%", timeout_ratio * 100.0),
                "Tier degraded due to timeouts - partial results returned"
            );
        }

        state.current_tier = Some(tier.level);
        state.completed_tiers.push(tier.level.as_str().to_string());

        Ok(results)
    }

    /// Resolve agent requests by looking up agents in the pool.
    fn resolve_agent_requests(
        requests: &[super::consensus::AgentRequest],
        pool: &AgentPool,
    ) -> Vec<AgentId> {
        use super::traits::AgentRole;

        requests
            .iter()
            .filter_map(|req| {
                let role = AgentRole::module(&req.module);
                pool.select(&role).map(|_| AgentId::module(&req.module))
            })
            .collect()
    }

    /// Create a checkpoint.
    async fn create_checkpoint(
        &self,
        session_id: &str,
        state: &ConsensusState,
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
                    tier_level: state
                        .current_tier
                        .map(|t| t.as_str().to_string())
                        .unwrap_or_else(|| "none".to_string()),
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

    /// Emit an event to the event store.
    async fn emit_event(&self, event: DomainEvent) {
        let Some(store) = &self.event_store else {
            return;
        };
        if let Err(e) = store.append(event).await {
            warn!(error = %e, "Failed to emit consensus event");
        }
    }

    /// Resume hierarchical consensus from a checkpoint.
    ///
    /// Reconstructs the consensus state from events and continues execution
    /// from the last completed tier.
    ///
    /// # Parameters
    /// - `checkpoint_id`: The checkpoint identifier
    /// - `session_id`: The session identifier (avoids full event scan)
    /// - `checkpoint_round`: The round number at checkpoint time
    pub async fn resume_from_checkpoint(
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

        // Reconstruct state using session-scoped query (O(log n) indexed lookup)
        let state = self
            .reconstruct_state_from_session(store, session_id, checkpoint_round)
            .await?;

        info!(
            session_id = %state.session_id,
            completed_tiers = state.completed_tiers.len(),
            "State reconstructed from checkpoint"
        );

        // Emit recovery started event with proper session_id and correlation
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

        // Build strategy and resume execution
        self.resume_from_state(state, mission, scope, workspace, pool)
            .await
    }

    /// Reconstruct consensus state from session events using indexed query.
    ///
    /// Uses `query_by_correlation` for O(log n) lookup instead of full scan.
    async fn reconstruct_state_from_session(
        &self,
        store: &EventStore,
        session_id: &str,
        checkpoint_round: usize,
    ) -> Result<ConsensusState> {
        let events = store.query_by_correlation(session_id).await?;

        let mut state = ConsensusState::new(session_id.to_string());
        state.current_round = checkpoint_round;

        // Single pass: collect checkpoint tier level AND completed tier results
        for event in &events {
            match &event.payload {
                EventPayload::ConsensusCheckpointCreated {
                    tier_level, round, ..
                } if *round == checkpoint_round => {
                    state.current_tier = Self::parse_tier_level(tier_level);
                }
                EventPayload::TierConsensusCompleted {
                    tier_level,
                    unit_id,
                    converged,
                    respondent_count,
                    ..
                } => {
                    state.completed_tiers.push(unit_id.clone());
                    let tier = Self::parse_tier_level(tier_level).unwrap_or(TierLevel::Module);
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

    /// Parse tier level from string.
    fn parse_tier_level(s: &str) -> Option<TierLevel> {
        match s {
            "module" => Some(TierLevel::Module),
            "group" => Some(TierLevel::Group),
            "domain" => Some(TierLevel::Domain),
            "workspace" => Some(TierLevel::Workspace),
            "cross_workspace" => Some(TierLevel::CrossWorkspace),
            _ => None,
        }
    }

    /// Resume execution from a reconstructed state.
    async fn resume_from_state(
        &self,
        state: ConsensusState,
        mission: &ConsensusMission,
        scope: &AgentScope,
        workspace: &Workspace,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        use crate::error::PilotError;

        let start_time = std::time::Instant::now();

        // Select participants
        let selector = ParticipantSelector::new(workspace);
        let file_refs: Vec<&std::path::Path> =
            mission.affected_files.iter().map(|p| p.as_path()).collect();
        let participants = selector.select(&file_refs);

        // Build strategy
        let strategy = self.strategy_selector.select(scope, &participants);

        let ConsensusStrategy::Hierarchical { tiers } = strategy else {
            return Err(PilotError::AgentExecution(
                "Resume only supported for hierarchical consensus".into(),
            ));
        };

        // Extract data from state before moving
        let session_id = state.session_id.clone();
        let completed_tiers = state.completed_tiers.clone();
        let completed_units: std::collections::HashSet<String> =
            completed_tiers.into_iter().collect();
        let mut all_results = state.tier_results.clone();
        let mut current_state = state;
        let mut total_rounds = 0;

        for tier in &tiers {
            // Check if all units in this tier are completed
            let tier_complete = tier.units.iter().all(|u| completed_units.contains(&u.id));
            if tier_complete {
                continue;
            }

            // Execute remaining units in this tier
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

        // Synthesize final result
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

        Ok(HierarchicalConsensusResult {
            session_id: current_state.session_id,
            outcome,
            plan,
            tasks: vec![],
            tier_results: all_results,
            total_rounds,
            duration: start_time.elapsed(),
        })
    }
}

/// Compute SHA256 hash of content.
fn sha256_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consensus_state_new() {
        let state = ConsensusState::new("session-1".to_string());
        assert_eq!(state.session_id, "session-1");
        assert_eq!(state.current_round, 0);
        assert!(state.current_tier.is_none());
        assert!(state.completed_tiers.is_empty());
        assert!(state.tier_results.is_empty());
        assert_eq!(state.status, SessionStatus::Running);
    }

    #[test]
    fn test_consensus_state_hash_deterministic() {
        let state1 = ConsensusState::new("session-1".to_string());
        let state2 = ConsensusState::new("session-2".to_string());

        // Different sessions produce different hashes
        assert_ne!(state1.content_hash(), state2.content_hash());

        // Same state produces same hash (deterministic)
        let state1_clone = ConsensusState::new("session-1".to_string());
        assert_eq!(state1.content_hash(), state1_clone.content_hash());
    }

    #[test]
    fn test_consensus_state_hash_includes_round() {
        let mut state = ConsensusState::new("session-1".to_string());
        let hash_before = state.content_hash();

        state.current_round = 5;
        let hash_after = state.content_hash();

        assert_ne!(hash_before, hash_after);
    }
}
