#![allow(clippy::too_many_arguments)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::time::timeout;
use tracing::{debug, info, warn};

use std::collections::HashSet;

use super::super::super::consensus::{ConsensusResult, ConsensusTask};
use super::super::super::hierarchy::{ConsensusTier, ConsensusUnit, HierarchicalAggregator};
use super::super::super::pool::AgentPool;
use super::super::super::shared::{AgentId, TierLevel, TierResult};
use super::super::super::traits::TaskContext;
use super::super::types::AdaptiveConsensusState;
use super::super::{sha256_hash, AdaptiveConsensusExecutor, ConsensusMission, HierarchicalConsensusResult};
use crate::error::Result;
use crate::state::{DomainEvent, EventPayload};

impl AdaptiveConsensusExecutor {
    /// Execute hierarchical consensus (bottom-up).
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_hierarchical(
        &self,
        session_id: &str,
        tiers: &[ConsensusTier],
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        let start_time = Instant::now();

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

        let mut state = AdaptiveConsensusState::new(session_id.to_string());
        let mut all_tier_results = HashMap::new();
        let mut aggregator = HierarchicalAggregator::new();
        let mut total_rounds = 0;

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
                convergence_ratio = %format!("{:.0}%", aggregator.overall_convergence() * 100.0),
                "Tier completed"
            );

            aggregator.aggregate_tier(
                tier.level,
                tier_results.values().cloned().collect(),
            );
            all_tier_results.extend(tier_results);

            if self.config.checkpoint_on_tier_complete {
                self.create_checkpoint(session_id, &state, &mission.id)
                    .await?;
            }
        }

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

        let tasks = Self::collect_tasks_from_tier_results(&all_tier_results);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: all_tier_results,
            total_rounds,
            duration: total_duration,
        })
    }

    /// Execute hierarchical consensus with backtracking support.
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_with_backtracking(
        &self,
        session_id: &str,
        tiers: &[ConsensusTier],
        mission: &ConsensusMission,
        pool: &AgentPool,
        initial_constraints: Vec<String>,
    ) -> Result<HierarchicalConsensusResult> {
        let max_backtracks = self.config.max_backtracks;

        info!(
            session_id = %session_id,
            tier_count = tiers.len(),
            constraints = initial_constraints.len(),
            "Executing hierarchical consensus with backtracking"
        );

        let mut state = AdaptiveConsensusState::new(session_id.to_string());
        let mut all_tier_results: HashMap<String, TierResult> = HashMap::new();
        let mut active_constraints = initial_constraints;
        let mut tier_idx = 0;
        let mut backtrack_count = 0;
        let mut total_rounds = 0;

        while tier_idx < tiers.len() {
            let tier = &tiers[tier_idx];

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

            for result in tier_results.values() {
                total_rounds += result.rounds;
            }

            let conflicts = self.detect_cross_tier_conflicts(&tier_results, &all_tier_results);

            if !conflicts.is_empty() && tier_idx > 0 && backtrack_count < max_backtracks {
                let new_constraints = self.conflicts_to_constraints(&conflicts);
                let constraint_count = new_constraints.len();
                active_constraints.extend(new_constraints);

                let affected_start = self.find_earliest_affected_tier(
                    &conflicts,
                    &all_tier_results,
                    tiers,
                    tier_idx,
                );

                for t in &tiers[affected_start..tier_idx] {
                    for unit in &t.units {
                        all_tier_results.remove(&unit.id);
                    }
                }

                tier_idx = affected_start;
                backtrack_count += 1;

                info!(
                    backtrack = backtrack_count,
                    affected_from_tier = affected_start,
                    new_constraints = constraint_count,
                    total_constraints = active_constraints.len(),
                    "Backtracking affected tiers due to cross-tier conflicts"
                );
                continue;
            }

            all_tier_results.extend(tier_results);
            tier_idx += 1;

            if self.config.checkpoint_on_tier_complete {
                self.create_checkpoint(session_id, &state, &mission.id)
                    .await?;
            }
        }

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

        let tasks = Self::collect_tasks_from_tier_results(&all_tier_results);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: all_tier_results,
            total_rounds,
            duration: Duration::ZERO,
        })
    }

    /// Execute a consensus unit with peer context and constraints.
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_unit_with_context(
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

        let mut enhanced_context = mission.context.clone();

        for result in lower_tier_results.values() {
            enhanced_context.key_findings.push(format!(
                "[{} tier - {}] {}",
                result.tier_level.as_str(),
                result.unit_id,
                result.synthesis.lines().next().unwrap_or("No synthesis")
            ));
        }

        if !peer_context.is_empty() {
            enhanced_context
                .key_findings
                .push(format!("[Peer Context]\n{}", peer_context));
        }

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

        let (converged, synthesis, conflicts, rounds, tasks) = match result {
            ConsensusResult::Agreed {
                plan,
                rounds,
                tasks,
                ..
            } => (true, plan, vec![], rounds, tasks),
            ConsensusResult::PartialAgreement {
                plan,
                tasks,
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
                tasks,
            ),
            ConsensusResult::NoConsensus {
                summary,
                blocking_conflicts,
                ..
            } => (
                false,
                summary,
                blocking_conflicts.iter().map(|c| c.topic.clone()).collect(),
                1,
                vec![],
            ),
        };

        let tier_result = if converged {
            TierResult::success(
                tier_level,
                unit.id.clone(),
                synthesis,
                unit.participants.len(),
                rounds,
            )
        } else {
            TierResult::failure(
                tier_level,
                unit.id.clone(),
                synthesis,
                unit.participants.len(),
                conflicts,
                rounds,
            )
        };

        Ok(tier_result.with_tasks(tasks))
    }

    /// Execute consensus with intra-unit cross-visibility.
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_with_cross_visibility(
        &self,
        participants: &[AgentId],
        mission: &ConsensusMission,
        context: &TaskContext,
        pool: &AgentPool,
    ) -> Result<ConsensusResult> {
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

        self.synthesize_from_enhanced_proposals(&enhanced_proposals, mission)
    }

    /// Execute a single tier (delegates to execute_tier_internal with empty constraints).
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_tier(
        &self,
        session_id: &str,
        tier: &ConsensusTier,
        lower_tier_results: &HashMap<String, TierResult>,
        mission: &ConsensusMission,
        pool: &AgentPool,
        state: &mut AdaptiveConsensusState,
    ) -> Result<HashMap<String, TierResult>> {
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

    /// Internal tier execution with parallel unit support.
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_tier_internal(
        &self,
        session_id: &str,
        tier: &ConsensusTier,
        lower_tier_results: &HashMap<String, TierResult>,
        mission: &ConsensusMission,
        pool: &AgentPool,
        state: &mut AdaptiveConsensusState,
        constraints: &[String],
    ) -> Result<HashMap<String, TierResult>> {
        let unit_timeout = if self.config.tier_timeout_secs > 0 {
            Duration::from_secs(self.config.tier_timeout_secs)
        } else {
            Duration::from_secs(self.config.default_tier_timeout_secs)
        };

        if let Some(ref metrics) = self.metrics {
            metrics.on_tier_started(tier.level, tier.units.len());
        }

        // Emit start events for all units
        for unit in &tier.units {
            self.emit_event(
                DomainEvent::new(
                    &mission.id,
                    EventPayload::TierConsensusStarted {
                        session_id: session_id.to_string(),
                        tier_level: tier.level,
                        unit_id: unit.id.clone(),
                        participants: unit.participants.iter().map(|id| id.to_string()).collect(),
                    },
                )
                .with_correlation(session_id),
            )
            .await;
        }

        // Execute all units in parallel
        let futures: Vec<_> = tier
            .units
            .iter()
            .map(|unit| {
                async move {
                    let unit_result = match timeout(
                        unit_timeout,
                        self.execute_unit_with_context(
                            session_id,
                            tier.level,
                            unit,
                            lower_tier_results,
                            mission,
                            pool,
                            "",
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
                            TierResult::timeout(tier.level, unit.id.clone(), unit.participants.len())
                        }
                    };
                    (unit.id.clone(), unit_result)
                }
            })
            .collect();

        let unit_results = futures::future::join_all(futures).await;

        // Process results
        let mut results = HashMap::new();
        let mut timed_out_count = 0usize;

        for (unit_id, unit_result) in unit_results {
            if unit_result.timed_out {
                timed_out_count += 1;
            }

            self.emit_event(
                DomainEvent::new(
                    &mission.id,
                    EventPayload::TierConsensusCompleted {
                        session_id: session_id.to_string(),
                        tier_level: tier.level,
                        unit_id: unit_id.clone(),
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

            results.insert(unit_id, unit_result);
        }

        let timeout_ratio = if tier.units.is_empty() {
            0.0
        } else {
            timed_out_count as f64 / tier.units.len() as f64
        };

        if timeout_ratio >= self.config.max_timeout_ratio {
            warn!(
                tier = %tier.level,
                timed_out = timed_out_count,
                total = tier.units.len(),
                ratio = %format!("{:.1}%", timeout_ratio * 100.0),
                "Tier degraded due to timeouts - partial results returned"
            );
        }

        state.current_tier = Some(tier.level);
        state.completed_tiers.push(tier.level.as_str().to_string());

        Ok(results)
    }

    /// Find the earliest tier index affected by cross-tier conflicts.
    ///
    /// Uses case-insensitive word-boundary matching to reduce false positives
    /// from substring matches (e.g., "api" matching inside "api-v2").
    fn find_earliest_affected_tier(
        &self,
        conflicts: &[String],
        existing_results: &HashMap<String, TierResult>,
        tiers: &[ConsensusTier],
        current_tier_idx: usize,
    ) -> usize {
        let mut earliest = current_tier_idx;

        for conflict in conflicts {
            let conflict_lower = conflict.to_lowercase();
            for (idx, tier) in tiers[..current_tier_idx].iter().enumerate() {
                for unit in &tier.units {
                    if let Some(result) = existing_results.get(&unit.id) {
                        let conflict_match = result.conflicts.iter().any(|c| {
                            let c_lower = c.to_lowercase();
                            c_lower == conflict_lower
                                || conflict_lower.contains(&c_lower)
                                || c_lower.contains(&conflict_lower)
                        });
                        let unit_match = conflict_lower.contains(&result.unit_id.to_lowercase());

                        if conflict_match || unit_match {
                            earliest = earliest.min(idx);
                        }
                    }
                }
            }
        }

        earliest
    }

    /// Collect and deduplicate tasks from all tier results, ordered by tier level.
    pub(in crate::agent::multi::adaptive_consensus) fn collect_tasks_from_tier_results(
        tier_results: &HashMap<String, TierResult>,
    ) -> Vec<ConsensusTask> {
        let mut sorted_results: Vec<_> = tier_results.values().collect();
        sorted_results.sort_by_key(|r| r.tier_level);

        let mut seen_ids = HashSet::new();
        let mut tasks = Vec::new();

        for result in sorted_results {
            for task in &result.tasks {
                if seen_ids.insert(&task.id) {
                    tasks.push(task.clone());
                }
            }
        }

        tasks
    }
}
