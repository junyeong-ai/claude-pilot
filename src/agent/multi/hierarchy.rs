//! Agent hierarchy and consensus strategy based on manifest structure.
//!
//! Unified module combining:
//! - Tier levels (Module → Group → Domain → Workspace)
//! - Participant selection and tracking
//! - Strategy selection (Direct, Flat, Hierarchical)
//! - Task routing

use std::collections::{HashMap, HashSet};
use std::path::Path;

use modmap::Module;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::consensus::ConsensusTask;
use crate::config::ConsensusConfig;
use crate::orchestration::AgentScope;
use crate::workspace::Workspace;

pub use super::shared::{AgentId, Quorum, TierLevel, TierResult};

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
    group_coordinators: HashMap<String, AgentId>,
    /// Domain coordinators
    domain_coordinators: HashMap<String, AgentId>,
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
    fn try_register(&mut self, agent_id: AgentId) -> bool {
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
// Agent Hierarchy
// ============================================================================

pub struct AgentHierarchy {
    config: HierarchyConfig,
    groups: Vec<HierarchyGroup>,
    module_to_group: HashMap<String, String>,
    file_to_module: HashMap<String, String>,
}

impl AgentHierarchy {
    pub fn from_workspace(workspace: &Workspace, config: HierarchyConfig) -> Self {
        let groups = Self::build_from_workspace(workspace);

        let mut module_to_group = HashMap::new();
        for group in &groups {
            for module_id in &group.modules {
                module_to_group.insert(module_id.clone(), group.id.clone());
            }
        }

        let mut file_to_module = HashMap::new();
        for module in workspace.modules() {
            for path in &module.paths {
                file_to_module.insert(path.clone(), module.id.clone());
            }
        }

        debug!(
            groups = groups.len(),
            modules = module_to_group.len(),
            "Created agent hierarchy from manifest"
        );

        Self {
            config,
            groups,
            module_to_group,
            file_to_module,
        }
    }

    fn build_from_workspace(workspace: &Workspace) -> Vec<HierarchyGroup> {
        let groups = workspace.groups();
        if groups.is_empty() {
            return Self::create_default_group(workspace.modules());
        }

        groups
            .iter()
            .map(|g| HierarchyGroup {
                id: g.id.clone(),
                name: g.name.clone(),
                modules: g.module_ids.clone(),
                leader: g.leader_module.clone(),
                external_deps: Self::compute_external_deps(g, workspace),
                domain_id: g.domain_id.clone(),
            })
            .collect()
    }

    fn create_default_group(modules: &[Module]) -> Vec<HierarchyGroup> {
        if modules.is_empty() {
            return Vec::new();
        }

        vec![HierarchyGroup {
            id: "default".into(),
            name: "Default Group".into(),
            modules: modules.iter().map(|m| m.id.clone()).collect(),
            leader: modules.first().map(|m| m.id.clone()),
            external_deps: Vec::new(),
            domain_id: None,
        }]
    }

    fn compute_external_deps(group: &modmap::ModuleGroup, workspace: &Workspace) -> Vec<String> {
        let member_set: HashSet<&str> = group.module_ids.iter().map(|s| s.as_str()).collect();

        workspace
            .modules()
            .iter()
            .filter(|m| member_set.contains(m.id.as_str()))
            .flat_map(|m| m.dependencies.iter())
            .filter(|d| !member_set.contains(d.module_id.as_str()))
            .map(|d| d.module_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn route_task(&self, task: &ConsensusTask) -> RoutingDecision {
        let affected_modules: HashSet<String> = task
            .files_affected
            .iter()
            .filter_map(|f| self.module_for_file(f))
            .collect();

        let affected_groups: HashSet<String> = affected_modules
            .iter()
            .filter_map(|m| self.module_to_group.get(m))
            .cloned()
            .collect();

        // Check if this is a cross-workspace task (qualified module format)
        let is_cross_workspace = task
            .assigned_module
            .as_ref()
            .is_some_and(|m| m.contains("::"));

        let tier = if is_cross_workspace {
            // Cross-workspace tasks go to Global tier
            RoutingTier::Global
        } else {
            match affected_groups.len() {
                0 => RoutingTier::Direct,
                1 => RoutingTier::IntraGroup,
                2..=3 => RoutingTier::InterGroup,
                _ => RoutingTier::Global,
            }
        };

        // For qualified modules, preserve the full qualified path
        let primary_module = task
            .assigned_module
            .clone()
            .filter(|m| !m.is_empty())
            .or_else(|| affected_modules.into_iter().next());

        RoutingDecision {
            tier,
            target_groups: affected_groups.into_iter().collect(),
            primary_module,
            is_cross_workspace,
        }
    }

    pub fn module_for_file(&self, file: &str) -> Option<String> {
        if let Some(module) = self.file_to_module.get(file) {
            return Some(module.clone());
        }

        for (prefix, module) in &self.file_to_module {
            if file.starts_with(prefix) {
                return Some(module.clone());
            }
        }

        None
    }

    pub fn group_for_module(&self, module_id: &str) -> Option<&HierarchyGroup> {
        self.module_to_group
            .get(module_id)
            .and_then(|group_id| self.groups.iter().find(|g| &g.id == group_id))
    }

    pub fn groups(&self) -> &[HierarchyGroup] {
        &self.groups
    }

    pub fn total_modules(&self) -> usize {
        self.module_to_group.len()
    }

    pub fn modules_in_group(&self, group_id: &str) -> Vec<String> {
        self.groups
            .iter()
            .find(|g| g.id == group_id)
            .map(|g| g.modules.clone())
            .unwrap_or_default()
    }

    pub fn is_within_limits(&self) -> bool {
        self.groups.len() <= self.config.max_active_groups
            && self.module_to_group.len() <= self.config.global_agent_limit
    }

    pub fn partition_tasks_by_tier(
        &self,
        tasks: &[ConsensusTask],
    ) -> HashMap<RoutingTier, Vec<ConsensusTask>> {
        let mut result: HashMap<RoutingTier, Vec<ConsensusTask>> = HashMap::new();

        for task in tasks {
            let decision = self.route_task(task);
            result.entry(decision.tier).or_default().push(task.clone());
        }

        result
    }
}

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

    /// Build Module tier as SINGLE COLLECTIVE UNIT with ALL planners.
    ///
    /// v7 change: Instead of isolated units per module, all module planners
    /// are placed in ONE unit with cross-visibility enabled. This allows
    /// planners from different modules to see each other's proposals in
    /// real-time and reach semantic convergence.
    fn build_module_tier(&self, participants: &ParticipantSet) -> ConsensusTier {
        let mut tier = ConsensusTier::new(TierLevel::Module);

        // Collect ALL module agents into a single collective unit
        let all_module_agents: Vec<AgentId> = participants
            .module_agents_multi()
            .values()
            .flatten()
            .cloned()
            .collect();

        // Also include qualified module agents (cross-workspace)
        let qualified_agents: Vec<AgentId> = participants
            .qualified_module_agents()
            .values()
            .flatten()
            .cloned()
            .collect();

        let mut collective_participants = all_module_agents;
        collective_participants.extend(qualified_agents);

        if !collective_participants.is_empty() {
            let unit = ConsensusUnit::new(
                unit_id(TierLevel::Module, "collective"),
                collective_participants,
            )
            .with_quorum(self.config.tier_quorum.module.into())
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
                .with_quorum(self.config.tier_quorum.group.into())
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
                        .with_quorum(self.config.tier_quorum.domain.into())
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
                .with_quorum(self.config.tier_quorum.workspace.into())
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
                .with_quorum(self.config.tier_quorum.cross_workspace.into())
                .with_cross_visibility(self.config.tier_cross_visibility.cross_workspace)
                .with_modules(modules);
            tier.add_unit(unit);
        }

        tier
    }
}

// ============================================================================
// Participant Selector
// ============================================================================

pub struct ParticipantSelector<'a> {
    workspace: &'a Workspace,
}

impl<'a> ParticipantSelector<'a> {
    pub fn new(workspace: &'a Workspace) -> Self {
        Self { workspace }
    }

    pub fn select(&self, files: &[&Path]) -> ParticipantSet {
        let mut set = ParticipantSet::new();

        let affected_modules = self.find_affected_modules(files);
        let affected_groups = self.find_groups_for_modules(&affected_modules);

        let mut module_to_group_map: HashMap<&str, &str> = HashMap::new();
        for group in &affected_groups {
            for module_id in &group.module_ids {
                module_to_group_map.insert(module_id.as_str(), group.id.as_str());
            }
        }

        for module in &affected_modules {
            if let Some(&group_id) = module_to_group_map.get(module.id.as_str()) {
                set.add_module_agent_in_group(module.id.clone(), group_id.to_string());
            } else {
                set.add_module_agent(module.id.clone());
            }
        }

        if affected_groups.len() > 1 || affected_modules.len() > 1 {
            for group in &affected_groups {
                let leader = group
                    .leader_module
                    .clone()
                    .or_else(|| group.module_ids.first().cloned())
                    .unwrap_or_else(|| format!("default-{}", group.id));
                // Include domain mapping for proper domain tier filtering
                let domain_id = self
                    .workspace
                    .find_domain_for_group(&group.id)
                    .map(|d| d.id.clone());
                set.add_group_coordinator_in_domain(group.id.clone(), leader, domain_id);
            }
        }

        let affected_domains = self.find_domains_for_groups(&affected_groups);

        if affected_domains.len() > 1 || affected_groups.len() > 1 {
            for domain in &affected_domains {
                set.add_domain_coordinator(domain.id.clone());
            }
        }

        set
    }

    fn find_affected_modules(&self, files: &[&Path]) -> Vec<&Module> {
        let mut modules = Vec::new();
        let mut seen = HashSet::new();

        for file in files {
            if let Some(module) = self.workspace.find_module_for_file(file)
                && seen.insert(&module.id)
            {
                modules.push(module);
            }
        }

        modules.sort_by(|a, b| a.id.cmp(&b.id));
        modules
    }

    fn find_groups_for_modules(&self, modules: &[&Module]) -> Vec<&modmap::ModuleGroup> {
        let mut groups = Vec::new();
        let mut seen = HashSet::new();

        for module in modules {
            if let Some(group) = self.workspace.find_group_for_module(&module.id)
                && seen.insert(&group.id)
            {
                groups.push(group);
            }
        }

        groups.sort_by(|a, b| a.id.cmp(&b.id));
        groups
    }

    fn find_domains_for_groups(&self, groups: &[&modmap::ModuleGroup]) -> Vec<&modmap::Domain> {
        let mut domains = Vec::new();
        let mut seen = HashSet::new();

        for group in groups {
            if let Some(domain) = self.workspace.find_domain_for_group(&group.id)
                && seen.insert(&domain.id)
            {
                domains.push(domain);
            }
        }

        domains.sort_by(|a, b| a.id.cmp(&b.id));
        domains
    }

    pub fn handle_unmapped_files(&self, files: &[&Path]) -> EdgeCaseResolution {
        let mut unmapped = Vec::new();

        for file in files {
            if self.workspace.find_module_for_file(file).is_none() {
                unmapped.push((*file).to_path_buf());
            }
        }

        if unmapped.is_empty() {
            return EdgeCaseResolution::AllMapped;
        }

        if unmapped.len() as f64 / files.len() as f64 > 0.5 {
            EdgeCaseResolution::EscalateToHuman { unmapped }
        } else {
            EdgeCaseResolution::WorkspaceFallback { unmapped }
        }
    }
}

#[derive(Debug, Clone)]
pub enum EdgeCaseResolution {
    AllMapped,
    WorkspaceFallback { unmapped: Vec<std::path::PathBuf> },
    EscalateToHuman { unmapped: Vec<std::path::PathBuf> },
}

// ============================================================================
// Multi-Workspace Participant Selector
// ============================================================================

use super::workspace_registry::WorkspaceRegistry;

/// Multi-workspace participant selector for cross-workspace consensus.
///
/// Unlike `ParticipantSelector` which works with a single workspace,
/// this selector can gather participants from multiple workspaces
/// for cross-workspace consensus scenarios.
pub struct MultiWorkspaceParticipantSelector<'a> {
    workspaces: HashMap<String, &'a Workspace>,
    registry: Option<&'a WorkspaceRegistry>,
}

impl<'a> MultiWorkspaceParticipantSelector<'a> {
    /// Create selector with multiple workspaces.
    pub fn new() -> Self {
        Self {
            workspaces: HashMap::new(),
            registry: None,
        }
    }

    /// Add a workspace with its ID.
    pub fn with_workspace(mut self, id: impl Into<String>, workspace: &'a Workspace) -> Self {
        self.workspaces.insert(id.into(), workspace);
        self
    }

    /// Set the workspace registry for file-to-workspace mapping.
    pub fn with_registry(mut self, registry: &'a WorkspaceRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Select participants from all relevant workspaces based on affected files.
    ///
    /// Returns a `ParticipantSet` with qualified module agents (workspace::module format)
    /// for cross-workspace consensus.
    pub fn select(&self, files: &[&Path]) -> ParticipantSet {
        let mut set = ParticipantSet::new();

        // Group files by workspace
        let files_by_workspace = self.group_files_by_workspace(files);

        // For each workspace, use its ParticipantSelector
        for (ws_id, ws_files) in &files_by_workspace {
            if let Some(workspace) = self.workspaces.get(ws_id) {
                let selector = ParticipantSelector::new(workspace);
                let file_refs: Vec<&Path> = ws_files.iter().map(|p| p.as_path()).collect();
                let ws_participants = selector.select(&file_refs);

                // Convert to qualified module agents
                self.merge_with_qualification(&mut set, ws_id, &ws_participants);
            }
        }

        // Add workspace coordinators if multiple workspaces involved
        if files_by_workspace.len() > 1 {
            for ws_id in files_by_workspace.keys() {
                set.add_workspace_coordinator(ws_id.clone());
            }
        }

        set
    }

    /// Group files by their containing workspace.
    fn group_files_by_workspace(
        &self,
        files: &[&Path],
    ) -> HashMap<String, Vec<std::path::PathBuf>> {
        let mut result: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();

        for file in files {
            // First try registry if available
            if let Some(registry) = self.registry
                && let Some(ws_id) = registry.find_for_file(file)
            {
                result.entry(ws_id).or_default().push(file.to_path_buf());
                continue;
            }

            // Fallback: check each workspace's modules
            for (ws_id, workspace) in &self.workspaces {
                if workspace.find_module_for_file(file).is_some() {
                    result
                        .entry(ws_id.clone())
                        .or_default()
                        .push(file.to_path_buf());
                    break;
                }
            }
        }

        result
    }

    /// Merge participants from a single workspace with workspace qualification.
    fn merge_with_qualification(
        &self,
        target: &mut ParticipantSet,
        workspace_id: &str,
        source: &ParticipantSet,
    ) {
        // Add qualified module agents (using :: separator for consistency)
        for (module_id, agents) in source.module_agents_multi() {
            let qualified_id = format!("{}::{}", workspace_id, module_id);
            target.add_qualified_module_agents(qualified_id, agents.clone());
        }

        // Propagate group coordinators (qualified)
        for (group_id, agent) in source.group_coordinators() {
            let qualified_group = format!("{}::{}", workspace_id, group_id);
            target
                .group_coordinators
                .insert(qualified_group, agent.clone());
            target.try_register(agent.clone());
        }

        // Propagate domain coordinators (qualified)
        for (domain_id, agent) in source.domain_coordinators() {
            let qualified_domain = format!("{}::{}", workspace_id, domain_id);
            target
                .domain_coordinators
                .insert(qualified_domain, agent.clone());
            target.try_register(agent.clone());
        }
    }

    /// Handle unmapped files across all workspaces.
    pub fn handle_unmapped_files(&self, files: &[&Path]) -> EdgeCaseResolution {
        let mut unmapped = Vec::new();

        for file in files {
            let mut found = false;

            // Check registry first
            if let Some(registry) = self.registry
                && registry.find_for_file(file).is_some()
            {
                found = true;
            }

            // Then check each workspace
            if !found {
                for workspace in self.workspaces.values() {
                    if workspace.find_module_for_file(file).is_some() {
                        found = true;
                        break;
                    }
                }
            }

            if !found {
                unmapped.push(file.to_path_buf());
            }
        }

        if unmapped.is_empty() {
            EdgeCaseResolution::AllMapped
        } else if files.is_empty() || unmapped.len() * 2 > files.len() {
            // Use integer math to avoid floating point: unmapped/total > 0.5 ≡ unmapped*2 > total
            EdgeCaseResolution::EscalateToHuman { unmapped }
        } else {
            EdgeCaseResolution::WorkspaceFallback { unmapped }
        }
    }
}

impl Default for MultiWorkspaceParticipantSelector<'_> {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Hierarchical Aggregator
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
    pub fn get_tier(&self, tier_level: TierLevel) -> Option<&TierAggregation> {
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
        tiers.sort_by_key(|(level, _)| level.order());

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ConsensusConfig {
        ConsensusConfig {
            flat_threshold: 5,
            hierarchical_threshold: 15,
            ..Default::default()
        }
    }

    #[test]
    fn test_direct_strategy_single_participant() {
        let selector = StrategySelector::new(test_config());
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());

        let scope = AgentScope::Module {
            workspace: "test".to_string(),
            module: "auth".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        assert!(matches!(strategy, ConsensusStrategy::Direct { .. }));
        assert_eq!(strategy.name(), "direct");
    }

    #[test]
    fn test_flat_strategy_few_participants() {
        let selector = StrategySelector::new(test_config());
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());
        participants.add_module_agent("db".to_string());
        participants.add_module_agent("api".to_string());

        let scope = AgentScope::Group {
            workspace: "test".to_string(),
            group: "core".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        assert!(matches!(strategy, ConsensusStrategy::Flat { .. }));
        assert_eq!(strategy.participant_count(), 3);
    }

    #[test]
    fn test_participant_set_basics() {
        let mut set = ParticipantSet::new();

        set.add_module_agent("auth".to_string());
        set.add_module_agent("db".to_string());
        set.add_group_coordinator("core".to_string(), "auth".to_string());

        assert_eq!(set.len(), 3);
        assert_eq!(set.distinct_groups(), 1);
        assert!(!set.spans_multiple_domains());
    }

    #[test]
    fn test_participant_set_dedup() {
        let mut set = ParticipantSet::new();

        set.add_module_agent("auth".to_string());
        set.add_module_agent("auth".to_string());

        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_tier_level_display() {
        assert_eq!(TierLevel::Module.as_str(), "module");
        assert_eq!(TierLevel::Group.as_str(), "group");
        assert_eq!(TierLevel::Domain.as_str(), "domain");
        assert_eq!(TierLevel::Workspace.as_str(), "workspace");
        assert_eq!(TierLevel::CrossWorkspace.as_str(), "cross_workspace");
    }

    #[test]
    fn test_multi_instance_agents_per_module() {
        let mut set = ParticipantSet::new();

        // Add multiple agents for the same module (e.g., planning-0, coder-0)
        set.add_module_agents(
            "auth".to_string(),
            vec![
                AgentId::new("planning-0"),
                AgentId::new("coder-0"),
                AgentId::new("verifier-0"),
            ],
        );

        set.add_module_agents(
            "db".to_string(),
            vec![AgentId::new("planning-1"), AgentId::new("coder-1")],
        );

        // Should have 5 total participants (3 + 2)
        assert_eq!(set.len(), 5);

        // But only 2 distinct modules
        assert_eq!(set.module_count(), 2);

        // All agents should be accessible
        let all = set.all();
        assert!(all.iter().any(|a| a.as_str() == "planning-0"));
        assert!(all.iter().any(|a| a.as_str() == "coder-1"));
    }

    #[test]
    fn test_enrich_from_pool_planning_only() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());
        base_set.add_module_agent("db".to_string());

        // Pool key format: "{module}:{role}" (e.g., "auth:planning")
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "auth:planning" => vec![AgentId::new("auth:planning-0")],
                "db:planning" => vec![AgentId::new("db:planning-0")],
                "auth:coder" => vec![AgentId::new("auth:coder-0")],
                "db:coder" => vec![AgentId::new("db:coder-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        assert_eq!(base_set.len(), 2);
        assert_eq!(enriched.len(), 2);
        assert_eq!(enriched.module_count(), 2);

        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:planning-0"));
        assert!(all.iter().any(|a| a.as_str() == "db:planning-0"));
        assert!(!all.iter().any(|a| a.as_str().contains("coder")));
    }

    #[test]
    fn test_enrich_from_pool_coder_role() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());
        base_set.add_module_agent("db".to_string());

        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "auth:coder" => vec![AgentId::new("auth:coder-0")],
                "db:coder" => vec![AgentId::new("db:coder-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("coder"));

        assert_eq!(enriched.len(), 2);
        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:coder-0"));
        assert!(all.iter().any(|a| a.as_str() == "db:coder-0"));
    }

    #[test]
    fn test_enrich_from_pool_fallback_to_global() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());

        // No module-scoped agents, but global planning agents exist
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "planning" => vec![
                    AgentId::new("auth:planning-0"),
                    AgentId::new("db:planning-0"),
                ],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        // Should find "auth:planning-0" via global fallback filtered by module
        assert_eq!(enriched.len(), 1);
        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:planning-0"));
    }

    #[test]
    fn test_enrich_qualified_modules() {
        let mut base_set = ParticipantSet::new();
        base_set.add_qualified_module_agents(
            "project-a::auth".to_string(),
            vec![AgentId::new("placeholder")],
        );

        // Qualified module pool key: "project-a:auth:planning"
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "project-a:auth:planning" => vec![AgentId::new("project-a:auth:planning-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        assert_eq!(enriched.len(), 1);
        let all = enriched.all();
        assert!(
            all.iter()
                .any(|a| a.as_str() == "project-a:auth:planning-0")
        );
    }

    #[test]
    fn test_agents_by_role_selection() {
        let mut set = ParticipantSet::new();

        // Add mixed agents from different modules
        set.add_module_agents(
            "auth".to_string(),
            vec![
                AgentId::new("module-auth-planning-0"),
                AgentId::new("module-auth-coder-0"),
            ],
        );
        set.add_module_agents(
            "db".to_string(),
            vec![
                AgentId::new("module-db-planning-0"),
                AgentId::new("module-db-coder-0"),
            ],
        );

        // Select all planning agents across modules
        let planning = set.agents_by_role("planning");
        assert_eq!(planning.len(), 2);
        assert!(planning.iter().all(|a| a.as_str().contains("planning")));

        // Select all coder agents across modules
        let coders = set.agents_by_role("coder");
        assert_eq!(coders.len(), 2);
        assert!(coders.iter().all(|a| a.as_str().contains("coder")));
    }

    #[test]
    fn test_cross_workspace_qualified_modules() {
        let mut set = ParticipantSet::new();

        // Add agents from different workspaces (using :: separator)
        set.add_qualified_module_agents(
            "project-a::auth".to_string(),
            vec![AgentId::new("project-a::auth::planning-0")],
        );
        set.add_qualified_module_agents(
            "project-b::api".to_string(),
            vec![AgentId::new("project-b::api::planning-0")],
        );

        // Should span multiple workspaces
        assert!(set.spans_multiple_workspaces());

        // Total participants
        assert_eq!(set.len(), 2);

        // CrossWorkspace tier should include these
        let cross_ws_participants = set.participants_at_tier(TierLevel::CrossWorkspace);
        assert_eq!(cross_ws_participants.len(), 2);
    }

    #[test]
    fn test_consensus_units_with_multi_instance() {
        let mut set = ParticipantSet::new();

        set.add_module_agents_in_group(
            "auth".to_string(),
            "core".to_string(),
            vec![
                AgentId::new("auth-planning-0"),
                AgentId::new("auth-coder-0"),
            ],
        );

        let units = set.to_consensus_units(TierLevel::Module);

        // Should have 1 unit for the auth module
        assert_eq!(units.len(), 1);

        // The unit should contain both agents
        assert_eq!(units[0].participants.len(), 2);
    }

    #[test]
    fn test_single_collective_module_tier() {
        let config = ConsensusConfig {
            flat_threshold: 3,
            hierarchical_threshold: 8,
            ..Default::default()
        };
        let selector = StrategySelector::new(config);

        let mut participants = ParticipantSet::new();
        // Add agents from multiple modules in different groups to trigger hierarchical
        participants.add_module_agent_in_group("auth".to_string(), "security".to_string());
        participants.add_module_agent_in_group("api".to_string(), "core".to_string());
        participants.add_module_agent_in_group("db".to_string(), "data".to_string());
        participants.add_module_agent_in_group("cache".to_string(), "data".to_string());
        participants.add_group_coordinator("security".to_string(), "auth".to_string());
        participants.add_group_coordinator("core".to_string(), "api".to_string());
        participants.add_group_coordinator("data".to_string(), "db".to_string());

        let scope = AgentScope::Workspace {
            workspace: "test".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        // Should be hierarchical since we have 3 groups
        if let ConsensusStrategy::Hierarchical { tiers } = strategy {
            // Find module tier
            let module_tier = tiers.iter().find(|t| t.level == TierLevel::Module);
            assert!(module_tier.is_some(), "Module tier should exist");

            let module_tier = module_tier.unwrap();
            // v7: Should have SINGLE collective unit, not 4 separate units
            assert_eq!(
                module_tier.units.len(),
                1,
                "Should have single collective unit"
            );

            let collective = &module_tier.units[0];
            assert_eq!(collective.id, "unit-module-collective");
            assert_eq!(
                collective.participants.len(),
                4,
                "All 4 module agents in one unit"
            );
            assert!(
                collective.cross_visibility,
                "Cross-visibility should be enabled"
            );
            assert_eq!(collective.quorum, Quorum::Supermajority);
        } else {
            panic!("Expected hierarchical strategy, got {:?}", strategy.name());
        }
    }

    #[test]
    fn test_consensus_unit_quorum() {
        let unit = ConsensusUnit::new(
            "test",
            vec![AgentId::new("a"), AgentId::new("b"), AgentId::new("c")],
        )
        .with_quorum(Quorum::Supermajority)
        .with_cross_visibility(true);

        assert_eq!(unit.quorum, Quorum::Supermajority);
        assert!(unit.cross_visibility);
        assert!(unit.is_collective());

        // Check quorum threshold
        assert_eq!(unit.quorum.threshold(3), 2); // 2/3 of 3 = 2
        assert!(unit.quorum.is_met(2, 3));
        assert!(!unit.quorum.is_met(1, 3));
    }

    #[test]
    fn test_domain_tier_includes_group_coordinators() {
        let mut set = ParticipantSet::new();

        // Add modules in groups belonging to a domain
        set.add_module_agent_in_group("auth".to_string(), "security".to_string());
        set.add_module_agent_in_group("crypto".to_string(), "security".to_string());
        set.add_module_agent_in_group("api".to_string(), "core".to_string());

        // Add group coordinators with domain assignment
        set.add_group_coordinator_in_domain(
            "security".to_string(),
            "auth".to_string(),
            Some("backend".to_string()),
        );
        set.add_group_coordinator_in_domain(
            "core".to_string(),
            "api".to_string(),
            Some("backend".to_string()),
        );

        // Add domain coordinator
        set.add_domain_coordinator("backend".to_string());

        // Get domain tier units
        let units = set.to_consensus_units(TierLevel::Domain);

        // Should have 1 domain unit
        assert_eq!(units.len(), 1, "Should have 1 domain unit");

        let domain_unit = &units[0];
        assert!(
            domain_unit.id.contains("backend"),
            "Unit ID should contain domain ID"
        );

        // Domain unit should include both group coordinators, not just the domain coordinator
        assert_eq!(
            domain_unit.participants.len(),
            2,
            "Domain unit should include both group coordinators belonging to the domain"
        );

        // Verify coordinators are present
        let participant_ids: Vec<_> = domain_unit
            .participants
            .iter()
            .map(|a| a.as_str())
            .collect();
        assert!(
            participant_ids.iter().any(|id| id.contains("security")),
            "Should include security group coordinator"
        );
        assert!(
            participant_ids.iter().any(|id| id.contains("core")),
            "Should include core group coordinator"
        );
    }
}
