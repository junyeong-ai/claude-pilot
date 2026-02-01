//! Agent trait and supporting types.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::TaskAgent;
use crate::agent::multi::messaging::AgentMessageBus;
use crate::error::Result;

use super::result::AgentTaskResult;
use super::task::AgentTask;
use crate::agent::multi::identity::AgentIdentifier;
use crate::agent::multi::shared::{PermissionProfile, RoleCategory};

/// Dynamic agent role supporting core and module-specific roles.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentRole {
    pub id: String,
    pub category: RoleCategory,
}

impl AgentRole {
    /// Create a role with the given ID and default Core category.
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            category: RoleCategory::Core,
        }
    }

    /// Get the role ID.
    pub fn name(&self) -> &str {
        &self.id
    }

    pub fn core_research() -> Self {
        Self {
            id: "research".into(),
            category: RoleCategory::Core,
        }
    }

    pub fn core_planning() -> Self {
        Self {
            id: "planning".into(),
            category: RoleCategory::Core,
        }
    }

    pub fn core_coder() -> Self {
        Self {
            id: "coder".into(),
            category: RoleCategory::Core,
        }
    }

    pub fn core_verifier() -> Self {
        Self {
            id: "verifier".into(),
            category: RoleCategory::Core,
        }
    }

    pub fn module(module_id: &str) -> Self {
        Self {
            id: format!("module-{}", module_id),
            category: RoleCategory::Module,
        }
    }

    pub fn reviewer() -> Self {
        Self {
            id: "reviewer".into(),
            category: RoleCategory::Reviewer,
        }
    }

    pub fn architect() -> Self {
        Self {
            id: "architect".into(),
            category: RoleCategory::Architecture,
        }
    }

    pub fn is_core(&self) -> bool {
        self.category == RoleCategory::Core
    }

    pub fn is_planning_relevant(&self) -> bool {
        match self.category {
            RoleCategory::Module | RoleCategory::Architecture | RoleCategory::Advisor => true,
            RoleCategory::Core => matches!(self.id.as_str(), "research" | "planning" | "architect"),
            RoleCategory::Reviewer => false,
        }
    }

    pub fn is_module(&self) -> bool {
        self.category == RoleCategory::Module
    }

    /// Extract module ID from role.
    ///
    /// Handles both legacy and qualified formats:
    /// - Legacy: "module-auth" → "auth"
    /// - Qualified: "ws:auth:coder" → "auth"
    pub fn module_id(&self) -> Option<&str> {
        if !self.is_module() {
            return None;
        }

        // Legacy format: "module-{id}"
        if let Some(id) = self.id.strip_prefix("module-") {
            return Some(id);
        }

        // Qualified format: "workspace:module:coder" or "module:coder"
        if self.id.ends_with(":coder") {
            let parts: Vec<&str> = self.id.split(':').collect();
            return match parts.len() {
                3 => Some(parts[1]), // ws:module:coder
                2 => Some(parts[0]), // module:coder
                _ => None,
            };
        }

        None
    }

    /// Create a module coder role from a qualified module ID.
    ///
    /// Qualified format: `"workspace::domain::module"` or `"workspace::module"`
    /// Pool key format: `"workspace:module:coder"`
    ///
    /// This enables proper routing of consensus tasks to workspace-namespaced coder agents.
    pub fn module_coder_from_qualified(qualified_module: &str) -> Self {
        // Parse qualified format: "workspace::domain::module" or "workspace::module"
        let parts: Vec<&str> = qualified_module.split("::").collect();

        let pool_key = match parts.as_slice() {
            // Full qualified: workspace::domain::module -> workspace:module:coder
            [workspace, _domain, module] => format!("{}:{}:coder", workspace, module),
            // Partial qualified: workspace::module -> workspace:module:coder
            [workspace, module] => format!("{}:{}:coder", workspace, module),
            // Unqualified: just module name -> module:coder (will likely fallback to core_coder)
            [module] => format!("{}:coder", module),
            // Empty or malformed -> fallback
            _ => "coder".to_string(),
        };

        Self {
            id: pool_key,
            category: RoleCategory::Module,
        }
    }

    /// Check if this role ID represents a qualified module coder.
    pub fn is_qualified_module_coder(&self) -> bool {
        self.category == RoleCategory::Module && self.id.ends_with(":coder")
    }

    /// Extract workspace from qualified module coder role.
    pub fn workspace(&self) -> Option<&str> {
        if self.is_qualified_module_coder() {
            let parts: Vec<&str> = self.id.split(':').collect();
            if parts.len() >= 3 {
                return Some(parts[0]);
            }
        }
        None
    }

    pub fn permission_profile(&self) -> PermissionProfile {
        match self.category {
            RoleCategory::Core => match self.id.as_str() {
                "coder" => PermissionProfile::FileModify,
                "verifier" => PermissionProfile::VerifyExecute,
                _ => PermissionProfile::ReadOnly,
            },
            RoleCategory::Module => PermissionProfile::FileModify,
            RoleCategory::Reviewer | RoleCategory::Architecture | RoleCategory::Advisor => {
                PermissionProfile::ReadOnly
            }
        }
    }
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

/// Trait for specialized agents.
#[async_trait]
pub trait SpecializedAgent: Send + Sync {
    fn role(&self) -> &AgentRole;
    fn id(&self) -> &str;
    fn system_prompt(&self) -> &str;

    /// Returns the unified agent identifier with workspace namespacing.
    ///
    /// Default implementation constructs from legacy id() for backward compatibility.
    /// New agents should override this to return their stored AgentIdentifier.
    fn identifier(&self) -> AgentIdentifier {
        AgentIdentifier::from(self.id())
    }

    fn permission_profile(&self) -> PermissionProfile {
        self.role().permission_profile()
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult>;
    fn current_load(&self) -> u32;

    /// Execute task with P2P conflict resolution via MessageBus.
    ///
    /// Agents that support P2P conflict handling (e.g., coder) override this
    /// to subscribe to ConflictAlert messages and yield when appropriate.
    /// Default implementation falls back to direct execute().
    async fn execute_with_messaging(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        _bus: &AgentMessageBus,
    ) -> Result<AgentTaskResult> {
        self.execute(task, working_dir).await
    }
}

/// Builder for constructing agent prompts with consistent structure.
pub struct AgentPromptBuilder {
    sections: Vec<String>,
}

impl AgentPromptBuilder {
    pub fn new(system_prompt: &str, task_type: &str, task: &AgentTask) -> Self {
        let mut sections = Vec::new();
        sections.push(format!("{}\n\n---\n", system_prompt));
        sections.push(format!(
            "\n## {} Task: {}\n\n{}\n",
            task_type, task.id, task.description
        ));
        Self { sections }
    }

    pub fn with_context(mut self, task: &AgentTask) -> Self {
        if !task.context.key_findings.is_empty() {
            self.sections.push("\n## Context\n".to_string());
            for finding in &task.context.key_findings {
                self.sections.push(format!("- {}\n", finding));
            }
        }
        self
    }

    pub fn with_related_files(mut self, task: &AgentTask) -> Self {
        if !task.context.related_files.is_empty() {
            self.sections.push("\n## Related Files\n".to_string());
            for file in &task.context.related_files {
                self.sections.push(format!("- {}\n", file));
            }
        }
        self
    }

    pub fn with_blockers(mut self, task: &AgentTask) -> Self {
        if !task.context.blockers.is_empty() {
            self.sections.push("\n## Known Blockers\n".to_string());
            for blocker in &task.context.blockers {
                self.sections.push(format!("- {}\n", blocker));
            }
        }
        self
    }

    pub fn with_section(mut self, title: &str, items: &[&str]) -> Self {
        if !items.is_empty() {
            self.sections.push(format!("\n## {}\n", title));
            for item in items {
                self.sections.push(format!("{}\n", item));
            }
        }
        self
    }

    pub fn build(self) -> String {
        self.sections.concat()
    }
}

/// Base implementation for load tracking.
pub struct LoadTracker {
    active_tasks: AtomicU32,
}

impl LoadTracker {
    pub fn new() -> Self {
        Self {
            active_tasks: AtomicU32::new(0),
        }
    }

    pub fn increment(&self) {
        self.active_tasks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement(&self) {
        self.active_tasks.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn current(&self) -> u32 {
        self.active_tasks.load(Ordering::Relaxed)
    }
}

impl Default for LoadTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard for panic-safe load tracking.
pub struct LoadGuard<'a> {
    load: &'a LoadTracker,
}

impl<'a> LoadGuard<'a> {
    pub fn new(load: &'a LoadTracker) -> Self {
        Self { load }
    }
}

impl Drop for LoadGuard<'_> {
    fn drop(&mut self) {
        self.load.decrement();
    }
}

/// Core fields shared by all specialized agents.
pub struct AgentCore {
    pub id: String,
    pub role: AgentRole,
    pub identifier: AgentIdentifier,
    pub task_agent: Arc<TaskAgent>,
    pub load: LoadTracker,
}

impl AgentCore {
    /// Creates a new AgentCore with legacy string ID (for backward compatibility).
    ///
    /// The identifier is automatically derived from the string ID.
    pub fn new(id: impl Into<String>, role: AgentRole, task_agent: Arc<TaskAgent>) -> Self {
        let id_str = id.into();
        let identifier = AgentIdentifier::from(id_str.as_str());
        Self {
            id: id_str,
            role,
            identifier,
            task_agent,
            load: LoadTracker::new(),
        }
    }

    /// Creates a new AgentCore with a fully specified AgentIdentifier.
    ///
    /// This is the preferred constructor for new agents with workspace namespacing.
    pub fn with_identifier(
        identifier: AgentIdentifier,
        role: AgentRole,
        task_agent: Arc<TaskAgent>,
    ) -> Self {
        let id = identifier.qualified_id();
        Self {
            id,
            role,
            identifier,
            task_agent,
            load: LoadTracker::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn role(&self) -> &AgentRole {
        &self.role
    }

    pub fn identifier(&self) -> &AgentIdentifier {
        &self.identifier
    }

    pub fn begin_execution(&self) -> LoadGuard<'_> {
        self.load.increment();
        LoadGuard::new(&self.load)
    }
}

/// Execution metrics for an agent.
#[derive(Debug, Default)]
pub struct AgentMetrics {
    total_executions: AtomicU64,
    successful_executions: AtomicU64,
    failed_executions: AtomicU64,
    total_duration_ms: AtomicU64,
}

impl AgentMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_execution(&self, success: bool, duration: Duration) {
        self.total_executions.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);

        if success {
            self.successful_executions.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_executions.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.total_executions.load(Ordering::Relaxed);
        let successful = self.successful_executions.load(Ordering::Relaxed);
        let failed = self.failed_executions.load(Ordering::Relaxed);
        let total_ms = self.total_duration_ms.load(Ordering::Relaxed);

        MetricsSnapshot {
            total_executions: total,
            successful_executions: successful,
            failed_executions: failed,
            success_rate: if total > 0 {
                successful as f64 / total as f64
            } else {
                0.0
            },
            avg_duration_ms: if total > 0 { total_ms / total } else { 0 },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MetricsSnapshot {
    pub total_executions: u64,
    pub successful_executions: u64,
    pub failed_executions: u64,
    pub success_rate: f64,
    pub avg_duration_ms: u64,
}

/// Wrapper for boxed specialized agents with Clone support.
pub struct BoxedAgent {
    inner: Arc<dyn SpecializedAgent>,
}

impl BoxedAgent {
    pub fn new(agent: Arc<dyn SpecializedAgent>) -> Self {
        Self { inner: agent }
    }
}

impl Clone for BoxedAgent {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl std::ops::Deref for BoxedAgent {
    type Target = dyn SpecializedAgent;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_role_display() {
        assert_eq!(AgentRole::core_research().to_string(), "research");
        assert_eq!(AgentRole::core_planning().to_string(), "planning");
        assert_eq!(AgentRole::core_coder().to_string(), "coder");
        assert_eq!(AgentRole::core_verifier().to_string(), "verifier");
        assert_eq!(AgentRole::module("auth").to_string(), "module-auth");
        assert_eq!(AgentRole::reviewer().to_string(), "reviewer");
        assert_eq!(AgentRole::architect().to_string(), "architect");
    }

    #[test]
    fn test_role_category() {
        assert!(AgentRole::core_coder().is_core());
        assert!(AgentRole::module("api").is_module());
        assert!(!AgentRole::core_coder().is_module());
        assert_eq!(AgentRole::architect().category, RoleCategory::Architecture);
    }

    #[test]
    fn test_role_equality() {
        assert_eq!(AgentRole::core_research(), AgentRole::core_research());
        assert_ne!(AgentRole::core_research(), AgentRole::core_coder());
        assert_ne!(AgentRole::module("auth"), AgentRole::module("api"));
    }

    #[test]
    fn test_load_tracker() {
        let tracker = LoadTracker::new();
        assert_eq!(tracker.current(), 0);

        tracker.increment();
        assert_eq!(tracker.current(), 1);

        tracker.increment();
        assert_eq!(tracker.current(), 2);

        tracker.decrement();
        assert_eq!(tracker.current(), 1);
    }

    #[test]
    fn test_module_coder_from_qualified_full() {
        // Full qualified format: workspace::domain::module
        let role = AgentRole::module_coder_from_qualified("project-a::backend::auth");
        assert_eq!(role.id, "project-a:auth:coder");
        assert_eq!(role.category, RoleCategory::Module);
        assert!(role.is_qualified_module_coder());
        assert_eq!(role.workspace(), Some("project-a"));
    }

    #[test]
    fn test_module_coder_from_qualified_partial() {
        // Partial qualified format: workspace::module
        let role = AgentRole::module_coder_from_qualified("project-a::auth");
        assert_eq!(role.id, "project-a:auth:coder");
        assert!(role.is_qualified_module_coder());
        assert_eq!(role.workspace(), Some("project-a"));
    }

    #[test]
    fn test_module_coder_from_qualified_unqualified() {
        // Unqualified: just module name
        let role = AgentRole::module_coder_from_qualified("auth");
        assert_eq!(role.id, "auth:coder");
        assert!(role.is_qualified_module_coder());
        // No workspace for unqualified
        assert_eq!(role.workspace(), None);
    }

    #[test]
    fn test_module_coder_from_qualified_cross_workspace() {
        // Cross-workspace scenario: multiple modules from different projects
        let role_a = AgentRole::module_coder_from_qualified("project-a::domain-x::module-c");
        let role_b = AgentRole::module_coder_from_qualified("project-g::domain-h::module-i");

        assert_eq!(role_a.id, "project-a:module-c:coder");
        assert_eq!(role_b.id, "project-g:module-i:coder");
        assert_ne!(role_a.workspace(), role_b.workspace());
    }

    #[test]
    fn test_module_id_legacy_format() {
        // Legacy format: module-{id}
        let role = AgentRole::module("auth");
        assert_eq!(role.module_id(), Some("auth"));
    }

    #[test]
    fn test_module_id_qualified_format() {
        // Qualified format: ws:module:coder
        let role = AgentRole::module_coder_from_qualified("project-a::auth");
        assert_eq!(role.module_id(), Some("auth"));
    }

    #[test]
    fn test_module_id_qualified_full_format() {
        // Full qualified format: ws:module:coder (domain discarded)
        let role = AgentRole::module_coder_from_qualified("project-a::backend::auth");
        assert_eq!(role.module_id(), Some("auth"));
    }

    #[test]
    fn test_module_id_unqualified_coder() {
        // Unqualified coder: module:coder
        let role = AgentRole::module_coder_from_qualified("auth");
        assert_eq!(role.module_id(), Some("auth"));
    }

    #[test]
    fn test_module_id_non_module_role() {
        // Non-module roles return None
        assert_eq!(AgentRole::core_coder().module_id(), None);
        assert_eq!(AgentRole::architect().module_id(), None);
    }
}
