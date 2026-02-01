//! Agent scope determination based on manifest structure.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::workspace::Workspace;

/// Agent scope derived from manifest hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentScope {
    /// Cross-workspace scope (spanning multiple projects).
    CrossWorkspace { workspaces: Vec<String> },

    /// Workspace-level scope (cross-domain).
    Workspace { workspace: String },

    /// Domain-level scope (from manifest.domains).
    Domain { workspace: String, domain: String },

    /// Group-level scope (from manifest.groups).
    Group { workspace: String, group: String },

    /// Module-level scope (from manifest.modules).
    Module { workspace: String, module: String },
}

impl AgentScope {
    /// Determine the narrowest scope that contains all affected files.
    pub fn from_files(workspace: &Workspace, files: &[PathBuf]) -> Self {
        let affected_modules: HashSet<&str> = files
            .iter()
            .filter_map(|f| workspace.find_module_for_file(f).map(|m| m.id.as_str()))
            .collect();

        if affected_modules.is_empty() {
            return Self::Workspace {
                workspace: workspace.name.clone(),
            };
        }

        if affected_modules.len() == 1 {
            return Self::Module {
                workspace: workspace.name.clone(),
                module: affected_modules.into_iter().next().unwrap().to_string(),
            };
        }

        let affected_groups: HashSet<&str> = affected_modules
            .iter()
            .filter_map(|mid| workspace.find_group_for_module(mid).map(|g| g.id.as_str()))
            .collect();

        if affected_groups.len() == 1 {
            return Self::Group {
                workspace: workspace.name.clone(),
                group: affected_groups.into_iter().next().unwrap().to_string(),
            };
        }

        let affected_domains: HashSet<&str> = affected_groups
            .iter()
            .filter_map(|gid| workspace.find_domain_for_group(gid).map(|d| d.id.as_str()))
            .collect();

        if affected_domains.len() == 1 {
            return Self::Domain {
                workspace: workspace.name.clone(),
                domain: affected_domains.into_iter().next().unwrap().to_string(),
            };
        }

        Self::Workspace {
            workspace: workspace.name.clone(),
        }
    }

    /// Get workspace name (returns first workspace for cross-workspace scope).
    #[inline]
    pub fn workspace_name(&self) -> &str {
        match self {
            Self::CrossWorkspace { workspaces } => {
                workspaces.first().map(|s| s.as_str()).unwrap_or("")
            }
            Self::Workspace { workspace } => workspace,
            Self::Domain { workspace, .. } => workspace,
            Self::Group { workspace, .. } => workspace,
            Self::Module { workspace, .. } => workspace,
        }
    }

    /// Get all workspace names (for cross-workspace scenarios).
    pub fn workspace_names(&self) -> Vec<&str> {
        match self {
            Self::CrossWorkspace { workspaces } => workspaces.iter().map(|s| s.as_str()).collect(),
            Self::Workspace { workspace } => vec![workspace.as_str()],
            Self::Domain { workspace, .. } => vec![workspace.as_str()],
            Self::Group { workspace, .. } => vec![workspace.as_str()],
            Self::Module { workspace, .. } => vec![workspace.as_str()],
        }
    }

    pub fn tier(&self) -> ScopeTier {
        match self {
            Self::CrossWorkspace { .. } => ScopeTier::CrossWorkspace,
            Self::Workspace { .. } => ScopeTier::Workspace,
            Self::Domain { .. } => ScopeTier::Domain,
            Self::Group { .. } => ScopeTier::Group,
            Self::Module { .. } => ScopeTier::Module,
        }
    }
}

/// Tier level for scope comparison.
/// Ordered from broadest (CrossWorkspace) to narrowest (Module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScopeTier {
    CrossWorkspace = 0,
    Workspace = 1,
    Domain = 2,
    Group = 3,
    Module = 4,
}

impl ScopeTier {
    pub fn is_narrower_than(self, other: Self) -> bool {
        self > other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_tier_ordering() {
        assert!(ScopeTier::Module > ScopeTier::Group);
        assert!(ScopeTier::Group > ScopeTier::Domain);
        assert!(ScopeTier::Domain > ScopeTier::Workspace);
        assert!(ScopeTier::Module.is_narrower_than(ScopeTier::Workspace));
    }

    #[test]
    fn test_workspace_name_extraction() {
        let scope = AgentScope::Module {
            workspace: "backend".into(),
            module: "auth".into(),
        };
        assert_eq!(scope.workspace_name(), "backend");
    }
}
