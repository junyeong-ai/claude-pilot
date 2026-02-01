//! Participant management for orchestration sessions.
//!
//! Manages agent registration, status tracking, and capability discovery
//! for all participants in a multi-agent session.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::multi::{AgentId, AgentRole, TierLevel};

/// Agent status within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// Agent is registered but not yet active.
    Registered,
    /// Agent is ready to accept tasks.
    Ready,
    /// Agent is currently executing a task.
    Busy,
    /// Agent completed its assigned work.
    Completed,
    /// Agent failed and cannot continue.
    Failed,
    /// Agent was removed from the session.
    Removed,
}

impl AgentStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Ready | Self::Busy)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Removed)
    }
}

/// Capabilities that an agent can provide.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Roles the agent can fulfill.
    pub roles: Vec<AgentRole>,
    /// Maximum concurrent tasks.
    pub max_concurrent_tasks: u32,
    /// Estimated context budget (tokens).
    pub context_budget: u32,
    /// Specialized domains (e.g., "auth", "api", "database").
    pub domains: Vec<String>,
    /// Languages the agent specializes in.
    pub languages: Vec<String>,
}

impl AgentCapabilities {
    pub fn can_fulfill_role(&self, role: &AgentRole) -> bool {
        self.roles.iter().any(|r| r.name() == role.name())
    }

    pub fn has_domain(&self, domain: &str) -> bool {
        self.domains.iter().any(|d| d.eq_ignore_ascii_case(domain))
    }
}

/// A participant in the orchestration session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    /// Unique identifier for this participant.
    pub id: AgentId,
    /// Module this agent belongs to.
    pub module: String,
    /// Optional group (domain) membership.
    pub group: Option<String>,
    /// Workspace identifier.
    pub workspace: String,
    /// Current status.
    pub status: AgentStatus,
    /// Agent capabilities.
    pub capabilities: AgentCapabilities,
    /// Tasks assigned to this agent.
    pub assigned_tasks: Vec<String>,
    /// Registration timestamp (not serialized).
    #[serde(skip)]
    pub registered_at: Option<Instant>,
    /// Last activity timestamp (not serialized).
    #[serde(skip)]
    pub last_activity: Option<Instant>,
}

impl Participant {
    pub fn new(id: AgentId, module: String, workspace: String) -> Self {
        Self {
            id,
            module,
            group: None,
            workspace,
            status: AgentStatus::Registered,
            capabilities: AgentCapabilities::default(),
            assigned_tasks: Vec::new(),
            registered_at: Some(Instant::now()),
            last_activity: Some(Instant::now()),
        }
    }

    pub fn with_group(mut self, group: String) -> Self {
        self.group = Some(group);
        self
    }

    pub fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn mark_ready(&mut self) {
        self.status = AgentStatus::Ready;
        self.last_activity = Some(Instant::now());
    }

    pub fn mark_busy(&mut self) {
        self.status = AgentStatus::Busy;
        self.last_activity = Some(Instant::now());
    }

    pub fn mark_completed(&mut self) {
        self.status = AgentStatus::Completed;
        self.last_activity = Some(Instant::now());
    }

    pub fn mark_failed(&mut self) {
        self.status = AgentStatus::Failed;
        self.last_activity = Some(Instant::now());
    }

    pub fn assign_task(&mut self, task_id: String) {
        self.assigned_tasks.push(task_id);
        self.mark_busy();
    }

    pub fn complete_task(&mut self, task_id: &str) {
        self.assigned_tasks.retain(|t| t != task_id);
        if self.assigned_tasks.is_empty() {
            self.mark_ready();
        }
    }

    pub fn current_load(&self) -> u32 {
        self.assigned_tasks.len() as u32
    }

    pub fn has_capacity(&self) -> bool {
        self.current_load() < self.capabilities.max_concurrent_tasks
    }

    pub fn idle_duration(&self) -> Option<Duration> {
        self.last_activity.map(|t| t.elapsed())
    }
}

/// Summary of a participant for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantSummary {
    pub id: String,
    pub module: String,
    pub roles: Vec<String>,
    pub domains: Vec<String>,
    pub status: AgentStatus,
}

impl From<&Participant> for ParticipantSummary {
    fn from(p: &Participant) -> Self {
        Self {
            id: p.id.as_str().to_string(),
            module: p.module.clone(),
            roles: p
                .capabilities
                .roles
                .iter()
                .map(|r| r.name().to_string())
                .collect(),
            domains: p.capabilities.domains.clone(),
            status: p.status,
        }
    }
}

/// Registry managing all participants in a session.
#[derive(Debug, Default)]
pub struct ParticipantRegistry {
    participants: HashMap<AgentId, Participant>,
    by_module: HashMap<String, Vec<AgentId>>,
    by_group: HashMap<String, Vec<AgentId>>,
    by_workspace: HashMap<String, Vec<AgentId>>,
    by_role: HashMap<String, Vec<AgentId>>,
}

impl ParticipantRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new participant.
    pub fn register(&mut self, participant: Participant) {
        let id = participant.id.clone();

        // Index by module
        self.by_module
            .entry(participant.module.clone())
            .or_default()
            .push(id.clone());

        // Index by group
        if let Some(group) = &participant.group {
            self.by_group
                .entry(group.clone())
                .or_default()
                .push(id.clone());
        }

        // Index by workspace
        self.by_workspace
            .entry(participant.workspace.clone())
            .or_default()
            .push(id.clone());

        // Index by roles
        for role in &participant.capabilities.roles {
            self.by_role
                .entry(role.name().to_string())
                .or_default()
                .push(id.clone());
        }

        self.participants.insert(id, participant);
    }

    /// Get a participant by ID.
    pub fn get(&self, id: &AgentId) -> Option<&Participant> {
        self.participants.get(id)
    }

    /// Get a mutable reference to a participant.
    pub fn get_mut(&mut self, id: &AgentId) -> Option<&mut Participant> {
        self.participants.get_mut(id)
    }

    /// Remove a participant.
    pub fn remove(&mut self, id: &AgentId) -> Option<Participant> {
        if let Some(mut participant) = self.participants.remove(id) {
            participant.status = AgentStatus::Removed;

            // Clean up indices
            remove_from_index(&mut self.by_module, &participant.module, id);
            if let Some(group) = &participant.group {
                remove_from_index(&mut self.by_group, group, id);
            }
            remove_from_index(&mut self.by_workspace, &participant.workspace, id);
            for role in &participant.capabilities.roles {
                remove_from_index(&mut self.by_role, role.name(), id);
            }

            Some(participant)
        } else {
            None
        }
    }

    /// Get all participants in a module.
    pub fn by_module(&self, module: &str) -> Vec<&Participant> {
        self.by_module
            .get(module)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.participants.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all participants in a group.
    pub fn by_group(&self, group: &str) -> Vec<&Participant> {
        self.by_group
            .get(group)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.participants.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all participants in a workspace.
    pub fn by_workspace(&self, workspace: &str) -> Vec<&Participant> {
        self.by_workspace
            .get(workspace)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.participants.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all participants with a specific role.
    pub fn by_role(&self, role: &str) -> Vec<&Participant> {
        self.by_role
            .get(role)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.participants.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all available participants (status = Ready).
    pub fn available(&self) -> Vec<&Participant> {
        self.participants
            .values()
            .filter(|p| p.status.is_available())
            .collect()
    }

    /// Get all active participants (Ready or Busy).
    pub fn active(&self) -> Vec<&Participant> {
        self.participants
            .values()
            .filter(|p| p.status.is_active())
            .collect()
    }

    /// Get planning agents for consensus.
    pub fn planning_agents(&self) -> Vec<&Participant> {
        self.by_role("planning")
            .into_iter()
            .filter(|p| p.status.is_active())
            .collect()
    }

    /// Get coder agents for implementation.
    pub fn coder_agents(&self) -> Vec<&Participant> {
        self.by_role("coder")
            .into_iter()
            .filter(|p| p.status.is_active())
            .collect()
    }

    /// Check if participants span multiple workspaces.
    pub fn spans_multiple_workspaces(&self) -> bool {
        self.by_workspace.len() > 1
    }

    /// Get all unique workspaces.
    pub fn workspaces(&self) -> Vec<&str> {
        self.by_workspace.keys().map(|s| s.as_str()).collect()
    }

    /// Get all unique groups.
    pub fn groups(&self) -> Vec<&str> {
        self.by_group.keys().map(|s| s.as_str()).collect()
    }

    /// Get all unique modules.
    pub fn modules(&self) -> Vec<&str> {
        self.by_module.keys().map(|s| s.as_str()).collect()
    }

    /// Total number of participants.
    pub fn len(&self) -> usize {
        self.participants.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    /// Get participants for a tier level.
    pub fn at_tier(&self, tier: TierLevel) -> Vec<&Participant> {
        match tier {
            TierLevel::Module => self.participants.values().collect(),
            TierLevel::Group => {
                // Return participants that have group membership
                self.participants
                    .values()
                    .filter(|p| p.group.is_some())
                    .collect()
            }
            TierLevel::Domain => self.at_tier(TierLevel::Group), // Domain == Group in our model
            TierLevel::Workspace => {
                // Return one representative per workspace
                let mut seen = HashSet::new();
                self.participants
                    .values()
                    .filter(|p| seen.insert(p.workspace.clone()))
                    .collect()
            }
            TierLevel::CrossWorkspace => {
                // Return workspace coordinators if available
                if self.spans_multiple_workspaces() {
                    self.by_role("coordinator")
                        .into_iter()
                        .filter(|p| p.status.is_active())
                        .collect()
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Generate summaries for all active participants.
    pub fn summaries(&self) -> Vec<ParticipantSummary> {
        self.active().iter().map(|p| (*p).into()).collect()
    }

    /// Find best candidate for a task based on module and role.
    pub fn find_candidate(&self, module: &str, role: &str) -> Option<&Participant> {
        if let Some(ids) = self.by_module.get(module) {
            for id in ids {
                if let Some(p) = self.participants.get(id)
                    && p.status.is_available()
                    && p.has_capacity()
                    && p.capabilities.can_fulfill_role(&AgentRole::new(role))
                {
                    return Some(p);
                }
            }
        }

        // Fallback to any available agent with matching role
        self.by_role(role)
            .into_iter()
            .find(|p| p.status.is_available() && p.has_capacity())
    }
}

/// Helper function to remove an ID from an index.
fn remove_from_index(index: &mut HashMap<String, Vec<AgentId>>, key: &str, id: &AgentId) {
    if let Some(ids) = index.get_mut(key) {
        ids.retain(|i| i != id);
        if ids.is_empty() {
            index.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_participant(id: &str, module: &str, workspace: &str) -> Participant {
        let mut p = Participant::new(AgentId::new(id), module.to_string(), workspace.to_string());
        p.capabilities.roles = vec![AgentRole::new("planning"), AgentRole::new("coder")];
        p.capabilities.max_concurrent_tasks = 2;
        p.mark_ready();
        p
    }

    #[test]
    fn test_participant_lifecycle() {
        let mut p = create_test_participant("test-agent", "auth", "project-a");

        assert_eq!(p.status, AgentStatus::Ready);
        assert!(p.status.is_available());

        p.assign_task("task-1".to_string());
        assert_eq!(p.status, AgentStatus::Busy);
        assert!(!p.status.is_available());
        assert!(p.status.is_active());

        p.complete_task("task-1");
        assert_eq!(p.status, AgentStatus::Ready);

        p.mark_completed();
        assert!(p.status.is_terminal());
    }

    #[test]
    fn test_registry_indexing() {
        let mut registry = ParticipantRegistry::new();

        registry.register(
            create_test_participant("p1", "auth", "project-a").with_group("core".to_string()),
        );
        registry.register(
            create_test_participant("p2", "api", "project-a").with_group("core".to_string()),
        );
        registry.register(create_test_participant("p3", "db", "project-b"));

        assert_eq!(registry.len(), 3);
        assert!(registry.spans_multiple_workspaces());

        assert_eq!(registry.by_module("auth").len(), 1);
        assert_eq!(registry.by_group("core").len(), 2);
        assert_eq!(registry.by_workspace("project-a").len(), 2);
        assert_eq!(registry.by_workspace("project-b").len(), 1);
        assert_eq!(registry.by_role("planning").len(), 3);
    }

    #[test]
    fn test_find_candidate() {
        let mut registry = ParticipantRegistry::new();

        let mut p1 = create_test_participant("auth-planning-0", "auth", "project-a");
        p1.capabilities.roles = vec![AgentRole::new("planning")];
        registry.register(p1);

        let mut p2 = create_test_participant("auth-coder-0", "auth", "project-a");
        p2.capabilities.roles = vec![AgentRole::new("coder")];
        registry.register(p2);

        // Find planning agent for auth module
        let candidate = registry.find_candidate("auth", "planning");
        assert!(candidate.is_some());
        assert_eq!(candidate.unwrap().id.as_str(), "auth-planning-0");

        // Find coder agent for auth module
        let candidate = registry.find_candidate("auth", "coder");
        assert!(candidate.is_some());
        assert_eq!(candidate.unwrap().id.as_str(), "auth-coder-0");
    }
}
