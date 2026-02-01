//! Cross-workspace discovery and registration for distributed consensus.
//!
//! Enables detection of tasks spanning multiple workspaces and provides
//! workspace health monitoring for graceful degradation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::hierarchy::AgentId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Active,
    Degraded,
    Unavailable,
    Unknown,
}

impl WorkspaceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub id: String,
    pub root_path: PathBuf,
    pub status: WorkspaceStatus,
    pub registered_at: Instant,
    pub last_heartbeat: Instant,
    pub agents: Vec<AgentId>,
    pub modules: Vec<String>,
}

impl WorkspaceInfo {
    pub fn new(id: impl Into<String>, root_path: impl AsRef<Path>) -> Self {
        let now = Instant::now();
        Self {
            id: id.into(),
            root_path: root_path.as_ref().to_path_buf(),
            status: WorkspaceStatus::Active,
            registered_at: now,
            last_heartbeat: now,
            agents: Vec::new(),
            modules: Vec::new(),
        }
    }

    pub fn with_modules(mut self, modules: Vec<String>) -> Self {
        self.modules = modules;
        self
    }

    pub fn is_available(&self) -> bool {
        matches!(
            self.status,
            WorkspaceStatus::Active | WorkspaceStatus::Degraded
        )
    }

    pub fn time_since_heartbeat(&self) -> Duration {
        self.last_heartbeat.elapsed()
    }
}

pub struct WorkspaceRegistry {
    workspaces: DashMap<String, WorkspaceInfo>,
    file_to_workspace: DashMap<PathBuf, String>,
    heartbeat_timeout: Duration,
    unavailable_timeout: Duration,
}

impl WorkspaceRegistry {
    pub fn new() -> Self {
        Self {
            workspaces: DashMap::new(),
            file_to_workspace: DashMap::new(),
            heartbeat_timeout: Duration::from_secs(60),
            unavailable_timeout: Duration::from_secs(300),
        }
    }

    pub fn with_timeouts(
        mut self,
        heartbeat_timeout: Duration,
        unavailable_timeout: Duration,
    ) -> Self {
        self.heartbeat_timeout = heartbeat_timeout;
        self.unavailable_timeout = unavailable_timeout;
        self
    }

    /// Register a workspace with its modules (stores module IDs as paths).
    ///
    /// For proper file path mapping, prefer `register_with_paths` instead.
    pub fn register(&self, workspace: WorkspaceInfo) {
        // Map root path and module IDs before moving workspace
        self.file_to_workspace
            .insert(workspace.root_path.clone(), workspace.id.clone());

        for module in &workspace.modules {
            self.file_to_workspace
                .insert(PathBuf::from(module), workspace.id.clone());
        }

        debug!(workspace_id = %workspace.id, "Workspace registered");
        self.workspaces.insert(workspace.id.clone(), workspace);
    }

    /// Register a workspace with actual file paths per module.
    ///
    /// This method properly maps file paths to the workspace, enabling
    /// accurate cross-workspace detection based on file locations.
    pub fn register_with_paths(
        &self,
        workspace: WorkspaceInfo,
        module_paths: HashMap<String, Vec<PathBuf>>,
    ) {
        // Map root path before iterating
        self.file_to_workspace
            .insert(workspace.root_path.clone(), workspace.id.clone());

        // Map all file paths to this workspace
        for paths in module_paths.into_values() {
            for path in paths {
                self.file_to_workspace.insert(path, workspace.id.clone());
            }
        }

        debug!(workspace_id = %workspace.id, "Workspace registered with file paths");
        self.workspaces.insert(workspace.id.clone(), workspace);
    }

    /// Unregister a workspace.
    pub fn unregister(&self, workspace_id: &str) -> Option<WorkspaceInfo> {
        if let Some((_, info)) = self.workspaces.remove(workspace_id) {
            // Remove all file mappings for this workspace
            self.file_to_workspace
                .retain(|_, ws_id| ws_id != workspace_id);
            debug!(workspace_id = %workspace_id, "Workspace unregistered");
            Some(info)
        } else {
            None
        }
    }

    /// Record a heartbeat from a workspace.
    pub fn heartbeat(&self, workspace_id: &str) -> bool {
        if let Some(mut info) = self.workspaces.get_mut(workspace_id) {
            info.last_heartbeat = Instant::now();
            if info.status == WorkspaceStatus::Degraded {
                info.status = WorkspaceStatus::Active;
            }
            true
        } else {
            false
        }
    }

    /// Get workspace by ID.
    pub fn get(&self, workspace_id: &str) -> Option<WorkspaceInfo> {
        self.workspaces.get(workspace_id).map(|r| r.clone())
    }

    /// Find workspace containing a file path.
    pub fn find_for_file(&self, file_path: &Path) -> Option<String> {
        // Direct match
        if let Some(ws_id) = self.file_to_workspace.get(file_path) {
            return Some(ws_id.clone());
        }

        // Check parent directories
        for ancestor in file_path.ancestors().skip(1) {
            if let Some(ws_id) = self.file_to_workspace.get(ancestor) {
                return Some(ws_id.clone());
            }
        }

        // Check if any workspace root contains this file
        for entry in self.workspaces.iter() {
            if file_path.starts_with(&entry.root_path) {
                return Some(entry.id.clone());
            }
        }

        None
    }

    /// Check if files span multiple workspaces (requires cross-workspace consensus).
    ///
    /// Optimized with early return: exits as soon as 2 different workspaces are found.
    pub fn requires_cross_workspace(&self, files: &[PathBuf]) -> bool {
        let mut first_workspace: Option<String> = None;

        for file in files {
            if let Some(ws_id) = self.find_for_file(file) {
                match &first_workspace {
                    None => first_workspace = Some(ws_id),
                    Some(first) if first != &ws_id => return true,
                    _ => {}
                }
            }
        }

        false
    }

    /// Get all workspaces involved in a set of files.
    pub fn workspaces_for_files(&self, files: &[PathBuf]) -> Vec<String> {
        let mut workspaces: HashSet<String> = HashSet::new();

        for file in files {
            if let Some(ws_id) = self.find_for_file(file) {
                workspaces.insert(ws_id);
            }
        }

        workspaces.into_iter().collect()
    }

    /// Update workspace statuses based on heartbeat timeouts.
    pub fn update_statuses(&self) {
        for mut entry in self.workspaces.iter_mut() {
            let elapsed = entry.time_since_heartbeat();

            if elapsed > self.unavailable_timeout {
                if entry.status != WorkspaceStatus::Unavailable {
                    warn!(
                        workspace_id = %entry.id,
                        elapsed_secs = elapsed.as_secs(),
                        "Workspace marked unavailable"
                    );
                    entry.status = WorkspaceStatus::Unavailable;
                }
            } else if elapsed > self.heartbeat_timeout && entry.status == WorkspaceStatus::Active {
                warn!(
                    workspace_id = %entry.id,
                    elapsed_secs = elapsed.as_secs(),
                    "Workspace degraded"
                );
                entry.status = WorkspaceStatus::Degraded;
            }
        }
    }

    /// Get all available workspaces.
    pub fn available_workspaces(&self) -> Vec<WorkspaceInfo> {
        self.update_statuses();
        self.workspaces
            .iter()
            .filter(|entry| entry.is_available())
            .map(|entry| entry.clone())
            .collect()
    }

    /// Get unavailable workspaces.
    pub fn unavailable_workspaces(&self) -> Vec<WorkspaceInfo> {
        self.update_statuses();
        self.workspaces
            .iter()
            .filter(|entry| !entry.is_available())
            .map(|entry| entry.clone())
            .collect()
    }

    /// Calculate availability ratio.
    pub fn availability_ratio(&self) -> f64 {
        self.update_statuses();
        let total = self.workspaces.len();
        if total == 0 {
            return 1.0;
        }

        let available = self
            .workspaces
            .iter()
            .filter(|entry| entry.is_available())
            .count();

        available as f64 / total as f64
    }

    /// List all registered workspace IDs.
    pub fn list_workspace_ids(&self) -> Vec<String> {
        self.workspaces.iter().map(|e| e.id.clone()).collect()
    }

    /// Get workspace count.
    pub fn count(&self) -> usize {
        self.workspaces.len()
    }

    /// Add an agent to a workspace.
    pub fn add_agent(&self, workspace_id: &str, agent_id: AgentId) -> bool {
        if let Some(mut info) = self.workspaces.get_mut(workspace_id) {
            if !info.agents.contains(&agent_id) {
                info.agents.push(agent_id);
            }
            true
        } else {
            false
        }
    }

    /// Remove an agent from a workspace.
    pub fn remove_agent(&self, workspace_id: &str, agent_id: &AgentId) -> bool {
        if let Some(mut info) = self.workspaces.get_mut(workspace_id) {
            info.agents.retain(|a| a != agent_id);
            true
        } else {
            false
        }
    }

    /// Get agents in a workspace.
    pub fn agents_in(&self, workspace_id: &str) -> Vec<AgentId> {
        self.workspaces
            .get(workspace_id)
            .map(|info| info.agents.clone())
            .unwrap_or_default()
    }
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_find() {
        let registry = WorkspaceRegistry::new();

        let ws = WorkspaceInfo::new("ws-1", "/project/frontend")
            .with_modules(vec!["auth".to_string(), "api".to_string()]);

        registry.register(ws);

        assert!(registry.get("ws-1").is_some());
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn test_register_with_paths() {
        let registry = WorkspaceRegistry::new();

        let ws = WorkspaceInfo::new("ws-1", "/project")
            .with_modules(vec!["auth".to_string(), "api".to_string()]);

        let mut module_paths = HashMap::new();
        module_paths.insert(
            "auth".to_string(),
            vec![
                PathBuf::from("/project/src/auth/mod.rs"),
                PathBuf::from("/project/src/auth/login.rs"),
            ],
        );
        module_paths.insert(
            "api".to_string(),
            vec![PathBuf::from("/project/src/api/mod.rs")],
        );

        registry.register_with_paths(ws, module_paths);

        let found = registry.find_for_file(Path::new("/project/src/auth/mod.rs"));
        assert_eq!(found, Some("ws-1".to_string()));

        let found = registry.find_for_file(Path::new("/project/src/api/mod.rs"));
        assert_eq!(found, Some("ws-1".to_string()));
    }

    #[test]
    fn test_find_for_file() {
        let registry = WorkspaceRegistry::new();

        let ws = WorkspaceInfo::new("ws-1", "/project");
        registry.register(ws);

        let found = registry.find_for_file(Path::new("/project/src/main.rs"));
        assert_eq!(found, Some("ws-1".to_string()));

        let not_found = registry.find_for_file(Path::new("/other/file.rs"));
        assert!(not_found.is_none());
    }

    #[test]
    fn test_cross_workspace_detection() {
        let registry = WorkspaceRegistry::new();

        registry.register(WorkspaceInfo::new("frontend", "/project/frontend"));
        registry.register(WorkspaceInfo::new("backend", "/project/backend"));

        let same_workspace = vec![
            PathBuf::from("/project/frontend/a.ts"),
            PathBuf::from("/project/frontend/b.ts"),
        ];
        assert!(!registry.requires_cross_workspace(&same_workspace));

        let cross_workspace = vec![
            PathBuf::from("/project/frontend/a.ts"),
            PathBuf::from("/project/backend/b.rs"),
        ];
        assert!(registry.requires_cross_workspace(&cross_workspace));
    }

    #[test]
    fn test_heartbeat() {
        let registry = WorkspaceRegistry::new();

        let ws = WorkspaceInfo::new("ws-1", "/project");
        registry.register(ws);

        assert!(registry.heartbeat("ws-1"));
        assert!(!registry.heartbeat("non-existent"));
    }

    #[test]
    fn test_agent_management() {
        let registry = WorkspaceRegistry::new();

        let ws = WorkspaceInfo::new("ws-1", "/project");
        registry.register(ws);

        let agent = AgentId::new("coder-0");
        assert!(registry.add_agent("ws-1", agent.clone()));

        let agents = registry.agents_in("ws-1");
        assert_eq!(agents.len(), 1);

        assert!(registry.remove_agent("ws-1", &agent));
        let agents = registry.agents_in("ws-1");
        assert_eq!(agents.len(), 0);
    }

    #[test]
    fn test_workspaces_for_files() {
        let registry = WorkspaceRegistry::new();

        registry.register(WorkspaceInfo::new("ws-1", "/project/a"));
        registry.register(WorkspaceInfo::new("ws-2", "/project/b"));

        let files = vec![
            PathBuf::from("/project/a/file1.rs"),
            PathBuf::from("/project/b/file2.rs"),
            PathBuf::from("/project/a/file3.rs"),
        ];

        let workspaces = registry.workspaces_for_files(&files);
        assert_eq!(workspaces.len(), 2);
    }
}
