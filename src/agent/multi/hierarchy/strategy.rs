//! StrategySelector - consensus strategy selection based on participant count and scope.

use crate::agent::multi::shared::{AgentId, TierLevel};
use crate::config::ConsensusConfig;
use crate::agent::multi::scope::AgentScope;

use super::types::{
    ConsensusStrategy, ConsensusTier, ConsensusUnit, ParticipantSet, CROSS_WORKSPACE_COORDINATOR,
    UNIT_CROSS_WORKSPACE, UNIT_WORKSPACE,
};
use super::types::unit_id;

// ============================================================================
// Strategy Selector
// ============================================================================

pub struct StrategySelector {
    config: ConsensusConfig,
}

impl StrategySelector {
    pub fn new(config: ConsensusConfig) -> Self {
        Self { config }
    }

    pub fn select(&self, scope: &AgentScope, participants: &ParticipantSet) -> ConsensusStrategy {
        let count = participants.len();

        if count == 1 {
            return ConsensusStrategy::Direct {
                participant: participants.all().into_iter().next().unwrap_or_default(),
            };
        }

        if count <= self.config.flat_threshold {
            return ConsensusStrategy::Flat {
                participants: participants.all(),
            };
        }

        if count > self.config.hierarchical_threshold {
            return self.build_hierarchical(scope, participants);
        }

        self.analyze_for_strategy(scope, participants)
    }

    fn analyze_for_strategy(
        &self,
        scope: &AgentScope,
        participants: &ParticipantSet,
    ) -> ConsensusStrategy {
        let group_count = participants.distinct_groups();
        let has_cross_domain = participants.spans_multiple_domains();

        if group_count > 2 || has_cross_domain {
            self.build_hierarchical(scope, participants)
        } else {
            ConsensusStrategy::Flat {
                participants: participants.all(),
            }
        }
    }

    fn build_hierarchical(
        &self,
        scope: &AgentScope,
        participants: &ParticipantSet,
    ) -> ConsensusStrategy {
        let mut tiers = Vec::new();

        // Check if this spans multiple workspaces
        let is_cross_workspace = participants.spans_multiple_workspaces();

        match scope {
            AgentScope::CrossWorkspace { .. } => {
                // Full hierarchical consensus including cross-workspace tier
                tiers.push(self.build_module_tier(participants));
                tiers.push(self.build_group_tier(participants));
                tiers.push(self.build_domain_tier(participants));
                tiers.push(self.build_workspace_tier(participants));
                tiers.push(self.build_cross_workspace_tier(participants));
            }
            AgentScope::Workspace { .. } => {
                tiers.push(self.build_module_tier(participants));
                tiers.push(self.build_group_tier(participants));
                tiers.push(self.build_domain_tier(participants));
                tiers.push(self.build_workspace_tier(participants));
                // Add cross-workspace tier if spanning multiple workspaces
                if is_cross_workspace {
                    tiers.push(self.build_cross_workspace_tier(participants));
                }
            }
            AgentScope::Domain { .. } => {
                tiers.push(self.build_module_tier(participants));
                tiers.push(self.build_group_tier(participants));
                tiers.push(self.build_domain_tier(participants));
            }
            AgentScope::Group { .. } => {
                tiers.push(self.build_module_tier(participants));
                tiers.push(self.build_group_tier(participants));
            }
            AgentScope::Module { .. } => {
                return ConsensusStrategy::Flat {
                    participants: participants.all(),
                };
            }
        }

        tiers.retain(|t| !t.units.is_empty());

        if tiers.is_empty() || tiers.iter().map(|t| t.total_participants()).sum::<usize>() == 0 {
            return ConsensusStrategy::Flat {
                participants: participants.all(),
            };
        }

        ConsensusStrategy::Hierarchical { tiers }
    }

    /// Build Module tier with per-group collective units.
    ///
    /// Modules in the same group share one unit with cross-visibility enabled,
    /// ensuring module isolation between groups while allowing intra-group
    /// coordination. Modules without a group assignment are placed in a
    /// catch-all unit.
    fn build_module_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        use std::collections::HashMap as StdHashMap;

        let mut tier = ConsensusTier::new(TierLevel::Module);
        let mut groups: StdHashMap<String, Vec<AgentId>> = StdHashMap::new();
        let mut ungrouped: Vec<AgentId> = Vec::new();

        for (module_id, agents) in participants.module_agents_multi() {
            if let Some(group_id) = participants.module_to_group_mappings().get(module_id) {
                groups
                    .entry(group_id.clone())
                    .or_default()
                    .extend(agents.iter().cloned());
            } else {
                ungrouped.extend(agents.iter().cloned());
            }
        }

        // Qualified module agents (cross-workspace) go to ungrouped
        for agents in participants.qualified_module_agents().values() {
            ungrouped.extend(agents.iter().cloned());
        }

        // Create one unit per group
        for (group_id, group_agents) in &groups {
            if !group_agents.is_empty() {
                let unit = ConsensusUnit::new(
                    unit_id(TierLevel::Module, group_id),
                    group_agents.clone(),
                )
                .with_quorum(self.config.tier_quorum.module)
                .with_cross_visibility(self.config.tier_cross_visibility.module);
                tier.add_unit(unit);
            }
        }

        // Ungrouped modules go into a single collective unit
        if !ungrouped.is_empty() {
            let unit = ConsensusUnit::new(
                unit_id(TierLevel::Module, "ungrouped"),
                ungrouped,
            )
            .with_quorum(self.config.tier_quorum.module)
            .with_cross_visibility(self.config.tier_cross_visibility.module);
            tier.add_unit(unit);
        }

        tier
    }

    fn build_group_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        let mut tier = ConsensusTier::new(TierLevel::Group);

        for (group_id, coordinator) in participants.group_coordinators() {
            let group_participants = participants.modules_in_group(group_id);
            let unit = ConsensusUnit::new(unit_id(TierLevel::Group, group_id), group_participants)
                .with_coordinator(coordinator.clone())
                .with_quorum(self.config.tier_quorum.group)
                .with_cross_visibility(self.config.tier_cross_visibility.group);
            tier.add_unit(unit);
        }

        tier
    }

    /// Build Domain tier with proper group filtering.
    ///
    /// Bug fix: Each domain unit should only contain groups belonging to that domain,
    /// not all groups from all domains.
    fn build_domain_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        let mut tier = ConsensusTier::new(TierLevel::Domain);

        for (domain_id, coordinator) in participants.domain_coordinators() {
            // Filter group coordinators to only those belonging to this domain
            let domain_participants: Vec<_> = participants
                .groups_in_domain(domain_id)
                .into_iter()
                .cloned()
                .collect();

            if !domain_participants.is_empty() {
                let unit =
                    ConsensusUnit::new(unit_id(TierLevel::Domain, domain_id), domain_participants)
                        .with_coordinator(coordinator.clone())
                        .with_quorum(self.config.tier_quorum.domain)
                        .with_cross_visibility(self.config.tier_cross_visibility.domain);
                tier.add_unit(unit);
            }
        }

        tier
    }

    /// Build Workspace tier with cross-workspace visibility.
    fn build_workspace_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        let mut tier = ConsensusTier::new(TierLevel::Workspace);

        let workspace_participants: Vec<_> = participants
            .domain_coordinators()
            .values()
            .cloned()
            .collect();

        if !workspace_participants.is_empty() {
            let unit = ConsensusUnit::new(UNIT_WORKSPACE, workspace_participants)
                .with_quorum(self.config.tier_quorum.workspace)
                .with_cross_visibility(self.config.tier_cross_visibility.workspace);
            tier.add_unit(unit);
        }

        tier
    }

    /// Build CrossWorkspace tier for multi-workspace consensus.
    ///
    /// Includes both workspace coordinators AND qualified module agents for
    /// consistency with `to_consensus_units()` and `participants_at_tier()`.
    fn build_cross_workspace_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        let mut tier = ConsensusTier::new(TierLevel::CrossWorkspace);

        // Include both workspace coordinators and qualified module agents
        let mut cross_ws_participants: Vec<_> = participants
            .workspace_coordinators()
            .values()
            .cloned()
            .collect();
        cross_ws_participants.extend(
            participants
                .qualified_module_agents()
                .values()
                .flatten()
                .cloned(),
        );

        if !cross_ws_participants.is_empty() {
            let modules: Vec<String> = participants
                .qualified_module_agents()
                .keys()
                .cloned()
                .collect();

            let unit = ConsensusUnit::new(UNIT_CROSS_WORKSPACE, cross_ws_participants)
                .with_coordinator(AgentId::new(CROSS_WORKSPACE_COORDINATOR))
                .with_quorum(self.config.tier_quorum.cross_workspace)
                .with_cross_visibility(self.config.tier_cross_visibility.cross_workspace)
                .with_modules(modules);
            tier.add_unit(unit);
        }

        tier
    }
}
