//! Hierarchical aggregation - bottom-up tier result aggregation for consensus.

use std::collections::HashMap;

use crate::agent::multi::shared::{TierLevel, TierResult};

// ============================================================================
// Tier Aggregation
// ============================================================================

/// Aggregated result for an entire tier.
#[derive(Debug, Clone)]
pub struct TierAggregation {
    pub tier_level: TierLevel,
    pub unit_results: Vec<TierResult>,
    pub convergence_ratio: f64,
    pub combined_synthesis: String,
    pub propagated_conflicts: Vec<String>,
}

impl TierAggregation {
    pub fn is_fully_converged(&self) -> bool {
        self.convergence_ratio >= 1.0
    }

    pub fn is_partially_converged(&self) -> bool {
        self.convergence_ratio >= 0.5
    }
}

// ============================================================================
// Hierarchical Aggregator
// ============================================================================

/// Aggregator for bottom-up hierarchical consensus.
///
/// Aggregates results from lower tiers and synthesizes them for presentation
/// to upper tiers, tracking convergence and propagating conflicts.
#[derive(Default)]
pub struct HierarchicalAggregator {
    tier_results: HashMap<TierLevel, TierAggregation>,
}

impl HierarchicalAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Aggregate results from a tier's consensus units.
    pub fn aggregate_tier(&mut self, tier_level: TierLevel, unit_results: Vec<TierResult>) {
        let converged_count = unit_results.iter().filter(|r| r.converged).count();
        let total_count = unit_results.len();

        let convergence_ratio = if total_count > 0 {
            converged_count as f64 / total_count as f64
        } else {
            0.0
        };

        let combined_synthesis = unit_results
            .iter()
            .map(|r| {
                format!(
                    "[{}] {}",
                    r.unit_id,
                    r.synthesis.lines().next().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let propagated_conflicts: Vec<_> = unit_results
            .iter()
            .flat_map(|r| r.conflicts.iter().cloned())
            .collect();

        let aggregation = TierAggregation {
            tier_level,
            unit_results,
            convergence_ratio,
            combined_synthesis,
            propagated_conflicts,
        };

        self.tier_results.insert(tier_level, aggregation);
    }

    /// Get the aggregation for a tier.
    pub fn tier(&self, tier_level: TierLevel) -> Option<&TierAggregation> {
        self.tier_results.get(&tier_level)
    }

    /// Build context from lower tiers for an upper tier.
    pub fn build_upper_tier_context(&self, target_tier: TierLevel) -> String {
        let lower_tiers: Vec<TierLevel> = match target_tier {
            TierLevel::Module => vec![],
            TierLevel::Group => vec![TierLevel::Module],
            TierLevel::Domain => vec![TierLevel::Module, TierLevel::Group],
            TierLevel::Workspace => vec![TierLevel::Module, TierLevel::Group, TierLevel::Domain],
            TierLevel::CrossWorkspace => vec![
                TierLevel::Module,
                TierLevel::Group,
                TierLevel::Domain,
                TierLevel::Workspace,
            ],
        };

        let mut context_parts = Vec::new();

        for tier in lower_tiers {
            if let Some(aggregation) = self.tier_results.get(&tier) {
                context_parts.push(format!(
                    "## {} Tier Results ({}% converged)\n{}",
                    tier,
                    (aggregation.convergence_ratio * 100.0) as u32,
                    aggregation.combined_synthesis
                ));
            }
        }

        if context_parts.is_empty() {
            String::new()
        } else {
            context_parts.join("\n\n")
        }
    }

    /// Get all conflicts that should be escalated to the next tier.
    pub fn conflicts_for_escalation(&self, from_tier: TierLevel) -> Vec<String> {
        self.tier_results
            .get(&from_tier)
            .map(|a| a.propagated_conflicts.clone())
            .unwrap_or_default()
    }

    /// Check if all recorded tiers have converged.
    pub fn all_tiers_converged(&self) -> bool {
        !self.tier_results.is_empty() && self.tier_results.values().all(|a| a.is_fully_converged())
    }

    /// Get final synthesis combining all tier results.
    pub fn final_synthesis(&self) -> String {
        let mut tiers: Vec<_> = self.tier_results.iter().collect();
        tiers.sort_by_key(|(level, _)| *level);

        tiers
            .into_iter()
            .map(|(level, agg)| format!("# {} Level\n{}", level, agg.combined_synthesis))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Get overall convergence status.
    pub fn overall_convergence(&self) -> f64 {
        if self.tier_results.is_empty() {
            return 0.0;
        }

        let total: f64 = self
            .tier_results
            .values()
            .map(|a| a.convergence_ratio)
            .sum();
        total / self.tier_results.len() as f64
    }
}
