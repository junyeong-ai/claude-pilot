//! Participant selection from workspaces - single and multi-workspace.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use modmap::Module;

use crate::agent::multi::workspace_registry::WorkspaceRegistry;
use crate::workspace::Workspace;

use super::types::{EdgeCaseResolution, ParticipantSet};

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

// ============================================================================
// Multi-Workspace Participant Selector
// ============================================================================

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

        // Propagate module-to-group mappings (qualified)
        for (module_id, group_id) in source.module_to_group_mappings() {
            let qualified_module = format!("{}::{}", workspace_id, module_id);
            let qualified_group = format!("{}::{}", workspace_id, group_id);
            target.set_module_to_group(qualified_module, qualified_group);
        }

        // Propagate group-to-domain mappings (qualified)
        for (group_id, domain_id) in source.group_to_domain_mappings() {
            let qualified_group = format!("{}::{}", workspace_id, group_id);
            let qualified_domain = format!("{}::{}", workspace_id, domain_id);
            target.set_group_to_domain(qualified_group, qualified_domain);
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
