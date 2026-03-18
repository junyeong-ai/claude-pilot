use std::collections::HashMap;

use tracing::{info, warn};
use uuid::Uuid;

use super::super::consensus::{Conflict, ConsensusResult, EnhancedProposal};
use super::super::hierarchy::ConsensusTier;
use super::super::pool::AgentPool;
use super::super::shared::{AgentId, ConsensusOutcome, ConvergenceCheckResult, TierResult};
use super::super::traits::AgentRole;
use super::AdaptiveConsensusExecutor;
use crate::domain::Severity;
use crate::error::Result;

impl AdaptiveConsensusExecutor {
    /// Build a complete synthesis plan from all tier results.
    pub(super) fn build_full_synthesis(
        &self,
        tier_results: &HashMap<String, TierResult>,
        tiers: &[ConsensusTier],
    ) -> String {
        let mut sections = Vec::new();

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
    pub(super) fn determine_outcome(
        &self,
        tier_results: &HashMap<String, TierResult>,
        tiers: &[ConsensusTier],
    ) -> ConsensusOutcome {
        if tier_results.is_empty() {
            return ConsensusOutcome::Failed;
        }

        let all_converged = tier_results.values().all(|r| r.converged);
        if all_converged {
            return ConsensusOutcome::Converged;
        }

        if let Some(top_tier) = tiers.last() {
            let top_converged = tier_results
                .values()
                .filter(|r| r.tier_level == top_tier.level)
                .all(|r| r.converged);

            if top_converged {
                return ConsensusOutcome::PartialConvergence;
            }
        }

        ConsensusOutcome::Failed
    }

    /// Synthesize consensus result from enhanced proposals using semantic convergence.
    pub(super) fn synthesize_from_enhanced_proposals(
        &self,
        proposals: &[EnhancedProposal],
        _mission: &super::ConsensusMission,
    ) -> Result<ConsensusResult> {
        use super::super::convergence::SemanticConvergenceChecker;

        if proposals.is_empty() {
            return Ok(ConsensusResult::NoConsensus {
                summary: "No proposals received".to_string(),
                blocking_conflicts: vec![],
                respondent_count: 0,
            });
        }

        let checker = SemanticConvergenceChecker::new(&self.config);
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
                    tasks: vec![],
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
                        .map(|c| {
                            Conflict::new(
                                format!("conflict-{}", Uuid::new_v4()),
                                c.description,
                                Severity::Critical,
                            )
                            .with_agents(c.agents_involved.clone())
                            .with_positions(c.agents_involved)
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
                    tasks: vec![],
                    dissents: vec![],
                    unresolved_conflicts: conflicts
                        .into_iter()
                        .map(|c| {
                            Conflict::new(
                                format!("conflict-{}", Uuid::new_v4()),
                                c.description,
                                Severity::Warning,
                            )
                            .with_agents(c.agents_involved.clone())
                            .with_positions(c.agents_involved)
                        })
                        .collect(),
                    respondent_count: proposals.len(),
                })
            }
        }
    }

    /// Build a synthesized plan from enhanced proposals.
    pub(super) fn build_plan_from_proposals(
        &self,
        proposals: &[EnhancedProposal],
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

    /// Detect conflicts between tier results.
    ///
    /// Checks for semantic overlap between new tier's conflicts and existing
    /// tier results using case-insensitive word-boundary matching.
    pub(super) fn detect_cross_tier_conflicts(
        &self,
        new_results: &HashMap<String, TierResult>,
        existing_results: &HashMap<String, TierResult>,
    ) -> Vec<String> {
        let mut conflicts = Vec::new();

        for (unit_id, new_result) in new_results {
            if !new_result.converged {
                for conflict in &new_result.conflicts {
                    let conflict_lower = conflict.to_lowercase();
                    for existing in existing_results.values() {
                        let synthesis_lower = existing.synthesis.to_lowercase();
                        let has_overlap = existing.conflicts.iter().any(|ec| {
                            let ec_lower = ec.to_lowercase();
                            ec_lower.contains(&conflict_lower)
                                || conflict_lower.contains(&ec_lower)
                        }) || synthesis_lower.contains(&conflict_lower);

                        if has_overlap {
                            conflicts.push(format!(
                                "Cross-tier conflict: '{}' conflicts with {} tier unit '{}'",
                                conflict, existing.tier_level, existing.unit_id
                            ));
                        }
                    }
                }
            }

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

    /// Generate constraints from detected conflicts.
    pub(super) fn conflicts_to_constraints(&self, conflicts: &[String]) -> Vec<String> {
        conflicts
            .iter()
            .map(|c| format!("MUST RESOLVE: {}", c))
            .collect()
    }

    /// Resolve agent requests by looking up agents in the pool.
    pub(super) fn resolve_agent_requests(
        requests: &[super::super::consensus::AgentRequest],
        pool: &AgentPool,
    ) -> Vec<AgentId> {
        requests
            .iter()
            .filter_map(|req| {
                let role = AgentRole::module(&req.module);
                pool.select(&role).map(|_| AgentId::module(&req.module))
            })
            .collect()
    }
}
