//! AgentHierarchy - manifest-based agent hierarchy for routing and grouping.

use std::collections::{HashMap, HashSet};

use tracing::debug;

use crate::agent::multi::consensus::ConsensusTask;
use crate::workspace::Workspace;

use super::types::{HierarchyConfig, HierarchyGroup, RoutingDecision, RoutingTier};

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

    fn create_default_group(modules: &[modmap::Module]) -> Vec<HierarchyGroup> {
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
