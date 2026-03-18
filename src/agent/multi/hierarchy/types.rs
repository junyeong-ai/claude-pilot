//! Type definitions, constants, and core data structures for the hierarchy module.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::agent::multi::shared::{AgentId, TierLevel};
use crate::domain::{ConsensusStrategyKind, Quorum};

// ============================================================================
// Unit ID Constants
// ============================================================================

/// Unit ID prefix for module-level consensus units.
pub const UNIT_MODULE_PREFIX: &str = "unit-module";
/// Unit ID prefix for group-level consensus units.
pub const UNIT_GROUP_PREFIX: &str = "unit-group";
/// Unit ID prefix for domain-level consensus units.
pub const UNIT_DOMAIN_PREFIX: &str = "unit-domain";
/// Unit ID for workspace-level consensus.
pub const UNIT_WORKSPACE: &str = "unit-workspace";
/// Unit ID for cross-workspace consensus.
pub const UNIT_CROSS_WORKSPACE: &str = "unit-cross-workspace";
/// Default coordinator ID for cross-workspace consensus.
pub const CROSS_WORKSPACE_COORDINATOR: &str = "cross-workspace-coordinator";

/// Generate a unit ID for a given tier level and identifier.
#[inline]
pub fn unit_id(tier: TierLevel, id: &str) -> String {
    match tier {
        TierLevel::Module => format!("{}-{}", UNIT_MODULE_PREFIX, id),
        TierLevel::Group => format!("{}-{}", UNIT_GROUP_PREFIX, id),
        TierLevel::Domain => format!("{}-{}", UNIT_DOMAIN_PREFIX, id),
        TierLevel::Workspace => UNIT_WORKSPACE.to_string(),
        TierLevel::CrossWorkspace => UNIT_CROSS_WORKSPACE.to_string(),
    }
}

// ============================================================================
// Consensus Strategy
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusStrategy {
    Direct { participant: AgentId },
    Flat { participants: Vec<AgentId> },
    Hierarchical { tiers: Vec<ConsensusTier> },
}

impl ConsensusStrategy {
    pub fn kind(&self) -> ConsensusStrategyKind {
        match self {
            Self::Direct { .. } => ConsensusStrategyKind::Direct,
            Self::Flat { .. } => ConsensusStrategyKind::Flat,
            Self::Hierarchical { .. } => ConsensusStrategyKind::Hierarchical,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Flat { .. } => "flat",
            Self::Hierarchical { .. } => "hierarchical",
        }
    }

    pub fn participant_count(&self) -> usize {
        match self {
            Self::Direct { .. } => 1,
            Self::Flat { participants } => participants.len(),
            Self::Hierarchical { tiers } => tiers.iter().map(|t| t.total_participants()).sum(),
        }
    }

    pub fn tier_count(&self) -> usize {
        match self {
            Self::Direct { .. } | Self::Flat { .. } => 1,
            Self::Hierarchical { tiers } => tiers.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusTier {
    pub level: TierLevel,
    pub units: Vec<ConsensusUnit>,
}

impl ConsensusTier {
    pub fn new(level: TierLevel) -> Self {
        Self {
            level,
            units: Vec::new(),
        }
    }

    pub fn add_unit(&mut self, unit: ConsensusUnit) {
        self.units.push(unit);
    }

    pub fn total_participants(&self) -> usize {
        self.units.iter().map(|u| u.participants.len()).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusUnit {
    pub id: String,
    pub participants: Vec<AgentId>,
    pub coordinator: Option<AgentId>,
    pub parent_unit: Option<String>,
    pub quorum: Quorum,
    pub cross_visibility: bool,
    /// Modules represented by this unit (for tracking scope).
    pub modules: Vec<String>,
}

impl ConsensusUnit {
    pub fn new(id: impl Into<String>, participants: Vec<AgentId>) -> Self {
        Self {
            id: id.into(),
            participants,
            coordinator: None,
            parent_unit: None,
            quorum: Quorum::default(),
            cross_visibility: false,
            modules: Vec::new(),
        }
    }

    pub fn with_coordinator(mut self, coordinator: AgentId) -> Self {
        self.coordinator = Some(coordinator);
        self
    }

    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent_unit = Some(parent.into());
        self
    }

    pub fn with_parent_opt(mut self, parent: Option<String>) -> Self {
        self.parent_unit = parent;
        self
    }

    pub fn with_quorum(mut self, quorum: Quorum) -> Self {
        self.quorum = quorum;
        self
    }

    pub fn with_cross_visibility(mut self, enabled: bool) -> Self {
        self.cross_visibility = enabled;
        self
    }

    pub fn with_modules(mut self, modules: Vec<String>) -> Self {
        self.modules = modules;
        self
    }

    pub fn is_collective(&self) -> bool {
        self.cross_visibility && self.participants.len() > 1
    }
}

// ============================================================================
// Participant Set
// ============================================================================

/// Represents a collection of agents participating in a consensus round.
///
/// Supports multi-instance same-role agents across modules:
/// - Multiple planning agents from different modules can discuss together
/// - Multiple coder agents from different modules can implement together
/// - Cross-workspace consensus with workspace-qualified module IDs
#[derive(Debug, Clone, Default)]
pub struct ParticipantSet {
    /// Module ID -> list of agent IDs for that module (supports multiple agents per module)
    by_module: HashMap<String, Vec<AgentId>>,
    /// For cross-workspace: "workspace::module" -> list of agent IDs
    by_qualified_module: HashMap<String, Vec<AgentId>>,
    /// Module to group mapping
    module_to_group: HashMap<String, String>,
    /// Group to domain mapping
    group_to_domain: HashMap<String, String>,
    /// Group coordinators
    pub(crate) group_coordinators: HashMap<String, AgentId>,
    /// Domain coordinators
    pub(crate) domain_coordinators: HashMap<String, AgentId>,
    /// Workspace coordinators (for cross-workspace consensus)
    workspace_coordinators: HashMap<String, AgentId>,
    /// All participants (flattened, ordered)
    all_participants: Vec<AgentId>,
    /// O(1) deduplication check
    participant_set: HashSet<AgentId>,
}

impl ParticipantSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a participant if not already present. Returns true if newly added.
    pub(crate) fn try_register(&mut self, agent_id: AgentId) -> bool {
        if self.participant_set.insert(agent_id.clone()) {
            self.all_participants.push(agent_id);
            true
        } else {
            false
        }
    }

    /// Add the default agent for a module.
    pub fn add_module_agent(&mut self, module_id: String) {
        let agent_id = AgentId::module(&module_id);
        self.add_agent_to_module(module_id, agent_id);
    }

    /// Add multiple agents for a module (multi-instance support).
    pub fn add_module_agents(&mut self, module_id: String, agents: Vec<AgentId>) {
        for agent in agents {
            self.add_agent_to_module(module_id.clone(), agent);
        }
    }

    /// Add a single agent to a module's agent list.
    fn add_agent_to_module(&mut self, module_id: String, agent_id: AgentId) {
        let agents = self.by_module.entry(module_id).or_default();
        if !agents.contains(&agent_id) {
            agents.push(agent_id.clone());
        }
        self.try_register(agent_id);
    }

    /// Add agents for a cross-workspace qualified module (workspace::module).
    pub fn add_qualified_module_agents(&mut self, qualified_id: String, agents: Vec<AgentId>) {
        for agent in agents {
            let agent_list = self
                .by_qualified_module
                .entry(qualified_id.clone())
                .or_default();
            if !agent_list.contains(&agent) {
                agent_list.push(agent.clone());
            }
            self.try_register(agent);
        }
    }

    /// Add the default agent for a module and assign it to a group.
    pub fn add_module_agent_in_group(&mut self, module_id: String, group_id: String) {
        let agent_id = AgentId::module(&module_id);
        self.add_agent_to_module(module_id.clone(), agent_id);
        self.module_to_group.insert(module_id, group_id);
    }

    /// Add multiple agents for a module in a group (multi-instance support).
    pub fn add_module_agents_in_group(
        &mut self,
        module_id: String,
        group_id: String,
        agents: Vec<AgentId>,
    ) {
        for agent in agents {
            self.add_agent_to_module(module_id.clone(), agent);
        }
        self.module_to_group.insert(module_id, group_id);
    }

    pub fn add_group_coordinator(&mut self, group_id: String, leader_module: String) {
        self.add_group_coordinator_in_domain(group_id, leader_module, None);
    }

    /// Add a group coordinator with optional domain assignment.
    pub fn add_group_coordinator_in_domain(
        &mut self,
        group_id: String,
        leader_module: String,
        domain_id: Option<String>,
    ) {
        use std::collections::hash_map::Entry;
        let agent_id = AgentId::group_coordinator(&group_id);
        if let Entry::Vacant(e) = self.group_coordinators.entry(group_id.clone()) {
            e.insert(agent_id.clone());
            self.module_to_group
                .entry(leader_module)
                .or_insert(group_id.clone());
            if let Some(domain) = domain_id {
                self.group_to_domain.insert(group_id, domain);
            }
            self.try_register(agent_id);
        }
    }

    /// Get groups belonging to a specific domain.
    pub fn groups_in_domain(&self, domain_id: &str) -> Vec<&AgentId> {
        self.group_coordinators
            .iter()
            .filter(|(group_id, _)| {
                self.group_to_domain
                    .get(*group_id)
                    .is_some_and(|d| d == domain_id)
            })
            .map(|(_, agent)| agent)
            .collect()
    }

    pub fn add_domain_coordinator(&mut self, domain_id: String) {
        use std::collections::hash_map::Entry;
        let agent_id = AgentId::domain_coordinator(&domain_id);
        if let Entry::Vacant(e) = self.domain_coordinators.entry(domain_id) {
            e.insert(agent_id.clone());
            self.try_register(agent_id);
        }
    }

    /// Add a workspace coordinator for cross-workspace consensus.
    pub fn add_workspace_coordinator(&mut self, workspace_id: String) {
        use std::collections::hash_map::Entry;
        let agent_id = AgentId::new(format!("workspace-coordinator-{}", workspace_id));
        if let Entry::Vacant(e) = self.workspace_coordinators.entry(workspace_id) {
            e.insert(agent_id.clone());
            self.try_register(agent_id);
        }
    }

    pub fn all(&self) -> Vec<AgentId> {
        self.all_participants.clone()
    }

    pub fn len(&self) -> usize {
        self.all_participants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all_participants.is_empty()
    }

    /// Count of distinct modules (not agents).
    pub fn module_count(&self) -> usize {
        self.by_module.len() + self.by_qualified_module.len()
    }

    pub fn distinct_groups(&self) -> usize {
        let mut groups = HashSet::new();
        groups.extend(self.group_coordinators.keys().cloned());
        groups.extend(self.module_to_group.values().cloned());
        groups.len()
    }

    pub fn spans_multiple_domains(&self) -> bool {
        self.domain_coordinators.len() > 1
    }

    /// Check if this set spans multiple workspaces.
    pub fn spans_multiple_workspaces(&self) -> bool {
        if self.by_qualified_module.is_empty() {
            return false;
        }
        let workspaces: HashSet<_> = self
            .by_qualified_module
            .keys()
            .filter_map(|qid| qid.split("::").next())
            .collect();
        workspaces.len() > 1
    }

    /// Get all agents for a specific module.
    pub fn agents_for_module(&self, module_id: &str) -> Vec<AgentId> {
        self.by_module.get(module_id).cloned().unwrap_or_default()
    }

    /// Get all agents for a qualified module (workspace::module).
    pub fn agents_for_qualified_module(&self, qualified_id: &str) -> Vec<AgentId> {
        self.by_qualified_module
            .get(qualified_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all module agents.
    pub fn module_agents_multi(&self) -> &HashMap<String, Vec<AgentId>> {
        &self.by_module
    }

    /// Get all qualified module agents (cross-workspace).
    pub fn qualified_module_agents(&self) -> &HashMap<String, Vec<AgentId>> {
        &self.by_qualified_module
    }

    pub fn group_coordinators(&self) -> &HashMap<String, AgentId> {
        &self.group_coordinators
    }

    pub fn domain_coordinators(&self) -> &HashMap<String, AgentId> {
        &self.domain_coordinators
    }

    pub fn module_to_group_mappings(&self) -> &HashMap<String, String> {
        &self.module_to_group
    }

    pub fn group_to_domain_mappings(&self) -> &HashMap<String, String> {
        &self.group_to_domain
    }

    pub fn set_module_to_group(&mut self, module_id: String, group_id: String) {
        self.module_to_group.insert(module_id, group_id);
    }

    pub fn set_group_to_domain(&mut self, group_id: String, domain_id: String) {
        self.group_to_domain.insert(group_id, domain_id);
    }

    pub fn workspace_coordinators(&self) -> &HashMap<String, AgentId> {
        &self.workspace_coordinators
    }

    /// Get all agents from modules in a specific group.
    pub fn modules_in_group(&self, group_id: &str) -> Vec<AgentId> {
        self.module_to_group
            .iter()
            .filter(|(_, gid)| *gid == group_id)
            .flat_map(|(module_id, _)| self.by_module.get(module_id).cloned().unwrap_or_default())
            .collect()
    }

    /// Get all agents at a specific tier level.
    pub fn participants_at_tier(&self, tier: TierLevel) -> Vec<AgentId> {
        match tier {
            TierLevel::Module => self.by_module.values().flatten().cloned().collect(),
            TierLevel::Group => self.group_coordinators.values().cloned().collect(),
            TierLevel::Domain => self.domain_coordinators.values().cloned().collect(),
            TierLevel::Workspace => self.workspace_coordinators.values().cloned().collect(),
            TierLevel::CrossWorkspace => {
                // Cross-workspace includes workspace coordinators and qualified module agents
                let mut participants: Vec<_> =
                    self.workspace_coordinators.values().cloned().collect();
                participants.extend(self.by_qualified_module.values().flatten().cloned());
                participants
            }
        }
    }

    /// Convert to consensus units for a specific tier.
    pub fn to_consensus_units(&self, tier: TierLevel) -> Vec<ConsensusUnit> {
        match tier {
            TierLevel::Module => self
                .by_module
                .iter()
                .map(|(module_id, agents)| {
                    let parent = self
                        .module_to_group
                        .get(module_id)
                        .map(|g| unit_id(TierLevel::Group, g));
                    ConsensusUnit::new(unit_id(TierLevel::Module, module_id), agents.clone())
                        .with_parent_opt(parent)
                        .with_modules(vec![module_id.clone()])
                })
                .collect(),
            TierLevel::Group => self
                .group_coordinators
                .iter()
                .map(|(group_id, coordinator)| {
                    let participants = self.modules_in_group(group_id);
                    let modules: Vec<String> = self
                        .module_to_group
                        .iter()
                        .filter(|(_, g)| *g == group_id)
                        .map(|(m, _)| m.clone())
                        .collect();
                    ConsensusUnit::new(unit_id(TierLevel::Group, group_id), participants)
                        .with_coordinator(coordinator.clone())
                        .with_parent(UNIT_WORKSPACE.to_string())
                        .with_modules(modules)
                })
                .collect(),
            TierLevel::Domain => self
                .domain_coordinators
                .iter()
                .filter_map(|(domain_id, coordinator)| {
                    // Include all group coordinators belonging to this domain
                    let domain_participants: Vec<AgentId> = self
                        .groups_in_domain(domain_id)
                        .into_iter()
                        .cloned()
                        .collect();
                    // Skip domains with no groups (consistent with build_domain_tier)
                    if domain_participants.is_empty() {
                        return None;
                    }
                    Some(
                        ConsensusUnit::new(
                            unit_id(TierLevel::Domain, domain_id),
                            domain_participants,
                        )
                        .with_coordinator(coordinator.clone())
                        .with_parent(UNIT_WORKSPACE.to_string()),
                    )
                })
                .collect(),
            TierLevel::Workspace => {
                let participants: Vec<AgentId> =
                    self.domain_coordinators.values().cloned().collect();
                if participants.is_empty() {
                    vec![]
                } else {
                    let modules: Vec<String> = self.by_module.keys().cloned().collect();
                    vec![ConsensusUnit::new(UNIT_WORKSPACE, participants).with_modules(modules)]
                }
            }
            TierLevel::CrossWorkspace => {
                let mut participants: Vec<AgentId> =
                    self.workspace_coordinators.values().cloned().collect();
                participants.extend(self.by_qualified_module.values().flatten().cloned());

                if participants.is_empty() {
                    vec![]
                } else {
                    let modules: Vec<String> = self.by_qualified_module.keys().cloned().collect();
                    vec![
                        ConsensusUnit::new(UNIT_CROSS_WORKSPACE, participants)
                            .with_coordinator(AgentId::new(CROSS_WORKSPACE_COORDINATOR))
                            .with_modules(modules),
                    ]
                }
            }
        }
    }

    /// All standard agent roles for module-scoped lookup.
    const AGENT_ROLES: &'static [&'static str] = &[
        "planning",
        "coder",
        "verifier",
        "reviewer",
        "research",
        "architect",
    ];

    /// Enrich participant set with agent instances from the pool.
    ///
    /// Pool key format: `"{module}:{role}"` (e.g., "auth:planning").
    pub fn enrich_from_pool<F>(&self, agent_ids_for: F, role_filter: Option<&str>) -> Self
    where
        F: Fn(&str) -> Vec<AgentId>,
    {
        let mut seen = HashSet::new();
        let mut all_participants = Vec::new();
        let mut by_module = HashMap::new();
        let mut by_qualified_module = HashMap::new();

        // Helper to add agents without duplicates
        let mut add_agents = |agents: &[AgentId]| {
            for agent in agents {
                if seen.insert(agent.clone()) {
                    all_participants.push(agent.clone());
                }
            }
        };

        // Enrich unqualified modules
        for module_id in self.by_module.keys() {
            let agents = self.lookup_agents(&agent_ids_for, module_id, role_filter, false);
            add_agents(&agents);
            by_module.insert(module_id.clone(), agents);
        }

        // Enrich qualified modules (cross-workspace)
        for qualified_id in self.by_qualified_module.keys() {
            let agents = self.lookup_agents(&agent_ids_for, qualified_id, role_filter, true);
            add_agents(&agents);
            by_qualified_module.insert(qualified_id.clone(), agents);
        }

        // Add coordinators
        for agent in self.group_coordinators.values() {
            if seen.insert(agent.clone()) {
                all_participants.push(agent.clone());
            }
        }
        for agent in self.domain_coordinators.values() {
            if seen.insert(agent.clone()) {
                all_participants.push(agent.clone());
            }
        }
        for agent in self.workspace_coordinators.values() {
            if seen.insert(agent.clone()) {
                all_participants.push(agent.clone());
            }
        }

        Self {
            by_module,
            by_qualified_module,
            module_to_group: self.module_to_group.clone(),
            group_to_domain: self.group_to_domain.clone(),
            group_coordinators: self.group_coordinators.clone(),
            domain_coordinators: self.domain_coordinators.clone(),
            workspace_coordinators: self.workspace_coordinators.clone(),
            participant_set: seen,
            all_participants,
        }
    }

    /// Unified agent lookup for both qualified and unqualified modules.
    fn lookup_agents<F>(
        &self,
        agent_ids_for: &F,
        module_id: &str,
        role_filter: Option<&str>,
        is_qualified: bool,
    ) -> Vec<AgentId>
    where
        F: Fn(&str) -> Vec<AgentId>,
    {
        // Normalize qualified ID: "project-a::auth" -> "project-a:auth"
        let pool_base = if is_qualified {
            module_id.replace("::", ":")
        } else {
            module_id.to_string()
        };

        match role_filter {
            Some(role) => {
                self.lookup_single_role(agent_ids_for, &pool_base, module_id, role, is_qualified)
            }
            None => self.lookup_all_roles(agent_ids_for, &pool_base, module_id, is_qualified),
        }
    }

    /// Look up agents for a single role.
    fn lookup_single_role<F>(
        &self,
        agent_ids_for: &F,
        pool_base: &str,
        module_id: &str,
        role: &str,
        is_qualified: bool,
    ) -> Vec<AgentId>
    where
        F: Fn(&str) -> Vec<AgentId>,
    {
        // Primary: module-scoped role lookup
        let pool_key = format!("{}:{}", pool_base, role);
        let agents = agent_ids_for(&pool_key);
        if !agents.is_empty() {
            return agents;
        }

        // Fallback 1: global role filtered by module
        let global = agent_ids_for(role);
        let filtered: Vec<_> = global
            .into_iter()
            .filter(|a| Self::agent_matches_module(a, module_id))
            .collect();
        if !filtered.is_empty() {
            return filtered;
        }

        // Fallback 2: placeholder
        self.get_placeholder(module_id, is_qualified)
    }

    /// Look up agents for all roles (when role_filter is None).
    fn lookup_all_roles<F>(
        &self,
        agent_ids_for: &F,
        pool_base: &str,
        module_id: &str,
        is_qualified: bool,
    ) -> Vec<AgentId>
    where
        F: Fn(&str) -> Vec<AgentId>,
    {
        let mut all_agents = Vec::new();
        for role in Self::AGENT_ROLES {
            let pool_key = format!("{}:{}", pool_base, role);
            all_agents.extend(agent_ids_for(&pool_key));
        }

        if all_agents.is_empty() {
            return self.get_placeholder(module_id, is_qualified);
        }
        all_agents
    }

    /// Get placeholder agents for fallback.
    fn get_placeholder(&self, module_id: &str, is_qualified: bool) -> Vec<AgentId> {
        if is_qualified {
            self.by_qualified_module
                .get(module_id)
                .cloned()
                .unwrap_or_default()
        } else {
            self.by_module.get(module_id).cloned().unwrap_or_default()
        }
    }

    /// Check if an agent ID matches a module by naming convention.
    fn agent_matches_module(agent: &AgentId, module_id: &str) -> bool {
        let id = agent.as_str();
        // Match: "module:role-N", "ws:module:role-N", "module-role-N"
        id.starts_with(&format!("{}:", module_id))
            || id.starts_with(&format!("{}-", module_id))
            || id.contains(&format!(":{}:", module_id))
    }

    /// Select agents by role across all participating modules.
    ///
    /// Uses word-boundary matching to avoid false positives.
    /// Matches patterns: "{role}-N", ":{role}-N"
    pub fn agents_by_role(&self, role: &str) -> Vec<AgentId> {
        self.all_participants
            .iter()
            .filter(|a| Self::matches_role(a.as_str(), role))
            .cloned()
            .collect()
    }

    /// Check if agent ID contains the role as a complete token.
    fn matches_role(agent_id: &str, role: &str) -> bool {
        // Pattern: role followed by "-" (instance separator)
        // Examples: "planning-0", "auth:planning-0", "ws:auth:planning-0"
        let role_with_sep = format!("{}-", role);
        agent_id.contains(&role_with_sep) || agent_id.ends_with(role) // Edge case: no instance suffix
    }

    pub fn planning_agents(&self) -> Vec<AgentId> {
        self.agents_by_role("planning")
    }

    pub fn coder_agents(&self) -> Vec<AgentId> {
        self.agents_by_role("coder")
    }

    pub fn verifier_agents(&self) -> Vec<AgentId> {
        self.agents_by_role("verifier")
    }

    pub fn reviewer_agents(&self) -> Vec<AgentId> {
        self.agents_by_role("reviewer")
    }
}

// ============================================================================
// Routing
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutingTier {
    Direct,
    IntraGroup,
    InterGroup,
    Global,
}

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub tier: RoutingTier,
    pub target_groups: Vec<String>,
    pub primary_module: Option<String>,
    /// Whether this task spans multiple workspaces.
    pub is_cross_workspace: bool,
}

// ============================================================================
// Hierarchy Configuration and Group
// ============================================================================

#[derive(Debug, Clone)]
pub struct HierarchyConfig {
    pub max_agents_per_group: usize,
    pub max_active_groups: usize,
    pub global_agent_limit: usize,
}

impl Default for HierarchyConfig {
    fn default() -> Self {
        Self {
            max_agents_per_group: 5,
            max_active_groups: 10,
            global_agent_limit: 30,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HierarchyGroup {
    pub id: String,
    pub name: String,
    pub modules: Vec<String>,
    pub leader: Option<String>,
    pub external_deps: Vec<String>,
    pub domain_id: Option<String>,
}

// ============================================================================
// Edge Case Resolution
// ============================================================================

#[derive(Debug, Clone)]
pub enum EdgeCaseResolution {
    AllMapped,
    WorkspaceFallback { unmapped: Vec<std::path::PathBuf> },
    EscalateToHuman { unmapped: Vec<std::path::PathBuf> },
}
