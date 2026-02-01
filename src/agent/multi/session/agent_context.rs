//! Agent context for stateless agent invocation.
//!
//! The `AgentContext` provides all the information an agent needs for a single
//! invocation, including participant awareness, relevant notifications, and
//! task-specific data. This implements the Stateless Agent Invocation Pattern.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::notification::Notification;
use super::orchestration::OrchestrationSession;
use super::participant::ParticipantSummary;
use super::task_graph::{TaskInfo, TaskNode};
use crate::agent::multi::AgentId;

/// Context provided to an agent for a single invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContext {
    /// ID of the agent receiving this context.
    pub agent_id: String,
    /// Current session ID.
    pub session_id: String,
    /// Current phase of the orchestration.
    pub phase: String,
    /// The task assigned to this agent.
    pub assigned_task: Option<TaskAssignment>,
    /// Summary of all active participants.
    pub participants: Vec<ParticipantSummary>,
    /// Recent relevant notifications.
    pub notifications: Vec<NotificationSummary>,
    /// Last acknowledged notification ID (for incremental updates).
    pub last_notification_id: u64,
    /// Files this agent is working on.
    pub working_files: Vec<String>,
    /// Files owned/locked by other agents.
    pub locked_files: HashMap<String, String>,
    /// Conflicts that involve this agent.
    pub active_conflicts: Vec<ConflictSummary>,
    /// Related tasks (dependencies, dependents).
    pub related_tasks: Vec<TaskSummary>,
    /// Working directory.
    pub working_dir: PathBuf,
    /// Custom metadata.
    pub metadata: HashMap<String, String>,
}

impl AgentContext {
    /// Create a new empty context.
    pub fn new(agent_id: &str, session_id: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            phase: String::new(),
            assigned_task: None,
            participants: Vec::new(),
            notifications: Vec::new(),
            last_notification_id: 0,
            working_files: Vec::new(),
            locked_files: HashMap::new(),
            active_conflicts: Vec::new(),
            related_tasks: Vec::new(),
            working_dir: PathBuf::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a participant summary.
    pub fn with_participants(mut self, participants: Vec<ParticipantSummary>) -> Self {
        self.participants = participants;
        self
    }

    /// Add notifications.
    pub fn with_notifications(mut self, notifications: Vec<NotificationSummary>) -> Self {
        if let Some(last) = notifications.iter().map(|n| n.id).max() {
            self.last_notification_id = last;
        }
        self.notifications = notifications;
        self
    }

    /// Set assigned task.
    pub fn with_task(mut self, task: TaskAssignment) -> Self {
        self.assigned_task = Some(task);
        self
    }

    /// Set phase.
    pub fn with_phase(mut self, phase: &str) -> Self {
        self.phase = phase.to_string();
        self
    }

    /// Set working directory.
    pub fn with_working_dir(mut self, path: PathBuf) -> Self {
        self.working_dir = path;
        self
    }

    /// Add working files.
    pub fn with_working_files(mut self, files: Vec<String>) -> Self {
        self.working_files = files;
        self
    }

    /// Add locked files.
    pub fn with_locked_files(mut self, files: HashMap<String, String>) -> Self {
        self.locked_files = files;
        self
    }

    /// Add conflicts.
    pub fn with_conflicts(mut self, conflicts: Vec<ConflictSummary>) -> Self {
        self.active_conflicts = conflicts;
        self
    }

    /// Add related tasks.
    pub fn with_related_tasks(mut self, tasks: Vec<TaskSummary>) -> Self {
        self.related_tasks = tasks;
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    /// Get other agents working on the same files.
    pub fn agents_on_same_files(&self) -> Vec<&str> {
        let my_files: std::collections::HashSet<_> = self.working_files.iter().collect();
        self.locked_files
            .iter()
            .filter(|(file, _)| my_files.contains(file))
            .map(|(_, agent)| agent.as_str())
            .collect()
    }

    /// Check if there are any active conflicts.
    pub fn has_conflicts(&self) -> bool {
        !self.active_conflicts.is_empty()
    }

    /// Get notification count.
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    /// Format context for injection into agent prompt.
    pub fn format_for_prompt(&self) -> String {
        let mut output = String::new();

        // Session info
        output.push_str(&format!(
            "## Session: {} (Phase: {})\n\n",
            self.session_id, self.phase
        ));

        // Assigned task
        if let Some(task) = &self.assigned_task {
            output.push_str("## Assigned Task\n\n");
            output.push_str(&format!("**ID**: {}\n", task.id));
            output.push_str(&format!("**Description**: {}\n", task.description));
            if !task.files.is_empty() {
                output.push_str(&format!("**Files**: {}\n", task.files.join(", ")));
            }
            if !task.dependencies.is_empty() {
                output.push_str(&format!(
                    "**Depends on**: {}\n",
                    task.dependencies.join(", ")
                ));
            }
            output.push('\n');
        }

        // Participants
        if !self.participants.is_empty() {
            output.push_str("## Active Participants\n\n");
            for p in &self.participants {
                if p.id != self.agent_id {
                    output.push_str(&format!(
                        "- **{}** ({}) - Roles: {} - Status: {:?}\n",
                        p.id,
                        p.module,
                        p.roles.join(", "),
                        p.status
                    ));
                }
            }
            output.push('\n');
        }

        // Conflicts
        if !self.active_conflicts.is_empty() {
            output.push_str("## ⚠️ Active Conflicts\n\n");
            for c in &self.active_conflicts {
                output.push_str(&format!(
                    "- **{}**: {} (agents: {})\n",
                    c.id,
                    c.description,
                    c.agents.join(", ")
                ));
            }
            output.push('\n');
        }

        // Locked files
        if !self.locked_files.is_empty() {
            output.push_str("## Locked Files (by other agents)\n\n");
            for (file, agent) in &self.locked_files {
                output.push_str(&format!("- `{}` - locked by {}\n", file, agent));
            }
            output.push('\n');
        }

        // Recent notifications
        if !self.notifications.is_empty() {
            output.push_str("## Recent Notifications\n\n");
            for n in self.notifications.iter().take(10) {
                output.push_str(&format!("- [#{}] {}\n", n.id, n.summary));
            }
            output.push('\n');
        }

        // Related tasks
        if !self.related_tasks.is_empty() {
            output.push_str("## Related Tasks\n\n");
            for t in &self.related_tasks {
                output.push_str(&format!(
                    "- **{}**: {} ({:?})\n",
                    t.id, t.description, t.status
                ));
            }
            output.push('\n');
        }

        output
    }
}

/// Task assignment for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAssignment {
    /// Task ID.
    pub id: String,
    /// Task description.
    pub description: String,
    /// Module this task belongs to.
    pub module: String,
    /// Role required.
    pub role: String,
    /// Files affected by this task.
    pub files: Vec<String>,
    /// Dependencies (task IDs).
    pub dependencies: Vec<String>,
    /// Priority.
    pub priority: u32,
    /// Additional instructions.
    pub instructions: Option<String>,
}

impl TaskAssignment {
    pub fn from_task_info(info: &TaskInfo) -> Self {
        Self {
            id: info.id.clone(),
            description: info.description.clone(),
            module: info.module.clone(),
            role: info.required_role.clone(),
            files: info.affected_files.clone(),
            dependencies: Vec::new(),
            priority: info.priority,
            instructions: None,
        }
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn with_instructions(mut self, instructions: &str) -> Self {
        self.instructions = Some(instructions.to_string());
        self
    }
}

/// Summary of a notification for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSummary {
    /// Notification ID.
    pub id: u64,
    /// One-line summary.
    pub summary: String,
    /// Priority.
    pub priority: u32,
    /// Whether it's been acknowledged.
    pub acknowledged: bool,
}

impl NotificationSummary {
    pub fn from_notification(n: &Notification, agent_id: &str) -> Self {
        Self {
            id: n.id,
            summary: notification_to_summary(n),
            priority: n.priority,
            acknowledged: n.is_acknowledged_by(agent_id),
        }
    }
}

fn notification_to_summary(n: &Notification) -> String {
    use super::notification::NotificationType;

    match &n.notification_type {
        NotificationType::TaskAssigned { task_id, agent_id } => {
            format!("Task '{}' assigned to '{}'", task_id, agent_id)
        }
        NotificationType::TaskCompleted {
            task_id, agent_id, ..
        } => {
            format!("Task '{}' completed by '{}'", task_id, agent_id)
        }
        NotificationType::TaskFailed {
            task_id,
            agent_id,
            error,
        } => {
            format!("Task '{}' failed by '{}': {}", task_id, agent_id, error)
        }
        NotificationType::ConflictDetected {
            conflict_id,
            description,
            ..
        } => {
            format!("CONFLICT [{}]: {}", conflict_id, description)
        }
        NotificationType::ConflictResolved {
            conflict_id,
            resolution,
        } => {
            format!("Conflict '{}' resolved: {}", conflict_id, resolution)
        }
        NotificationType::ProgressUpdate {
            agent_id,
            task_id,
            progress_percent,
            message,
        } => {
            format!(
                "{} on '{}': {}% - {}",
                agent_id, task_id, progress_percent, message
            )
        }
        NotificationType::LockAcquired { resource, holder } => {
            format!("Lock acquired: '{}' by '{}'", resource, holder)
        }
        NotificationType::LockReleased { resource, holder } => {
            format!("Lock released: '{}' by '{}'", resource, holder)
        }
        NotificationType::CoordinationRequest {
            request_id,
            requester,
            coordination_type,
            ..
        } => {
            format!(
                "Coordination [{}] from '{}': {}",
                request_id, requester, coordination_type
            )
        }
        NotificationType::PhaseChanged {
            from_phase,
            to_phase,
        } => {
            format!("Phase: {} -> {}", from_phase, to_phase)
        }
        NotificationType::CheckpointCreated {
            checkpoint_id,
            reason,
        } => {
            format!("Checkpoint [{}]: {}", checkpoint_id, reason)
        }
        NotificationType::Text { message } => message.clone(),
    }
}

/// Summary of a conflict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictSummary {
    /// Conflict ID.
    pub id: String,
    /// Agents involved.
    pub agents: Vec<String>,
    /// Files involved.
    pub files: Vec<String>,
    /// Description.
    pub description: String,
}

/// Summary of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    /// Task ID.
    pub id: String,
    /// Description.
    pub description: String,
    /// Status.
    pub status: String,
    /// Assigned agent (if any).
    pub assigned_to: Option<String>,
}

impl TaskSummary {
    pub fn from_task_node(node: &TaskNode) -> Self {
        Self {
            id: node.info.id.clone(),
            description: node.info.description.clone(),
            status: format!("{:?}", node.status),
            assigned_to: node.assigned_to.as_ref().map(|a| a.as_str().to_string()),
        }
    }
}

/// Builder for creating AgentContext from OrchestrationSession.
pub struct AgentContextBuilder<'a> {
    session: &'a OrchestrationSession,
    agent_id: AgentId,
    task_id: Option<String>,
    max_notifications: usize,
    include_all_participants: bool,
    include_related_tasks: bool,
}

impl<'a> AgentContextBuilder<'a> {
    pub fn new(session: &'a OrchestrationSession, agent_id: AgentId) -> Self {
        Self {
            session,
            agent_id,
            task_id: None,
            max_notifications: 20,
            include_all_participants: true,
            include_related_tasks: true,
        }
    }

    pub fn with_task(mut self, task_id: &str) -> Self {
        self.task_id = Some(task_id.to_string());
        self
    }

    pub fn with_max_notifications(mut self, max: usize) -> Self {
        self.max_notifications = max;
        self
    }

    pub fn without_all_participants(mut self) -> Self {
        self.include_all_participants = false;
        self
    }

    pub fn without_related_tasks(mut self) -> Self {
        self.include_related_tasks = false;
        self
    }

    pub fn build(self) -> AgentContext {
        let mut ctx = AgentContext::new(self.agent_id.as_str(), &self.session.id)
            .with_phase(self.session.phase().as_str())
            .with_working_dir(self.session.working_dir().clone());

        // Add participants
        if self.include_all_participants {
            ctx = ctx.with_participants(self.session.participant_summaries());
        }

        // Add notifications
        let notifications: Vec<NotificationSummary> = self
            .session
            .notifications_for_agent(&self.agent_id, self.max_notifications)
            .iter()
            .map(|n| NotificationSummary::from_notification(n, self.agent_id.as_str()))
            .collect();
        ctx = ctx.with_notifications(notifications);

        // Add task if specified
        // (Task lookup would be done here if we had access to TaskDAG methods)

        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::session::participant::AgentStatus;

    #[test]
    fn test_agent_context_creation() {
        let ctx = AgentContext::new("agent-1", "session-1")
            .with_phase("implementation")
            .with_working_files(vec!["auth.rs".to_string()]);

        assert_eq!(ctx.agent_id, "agent-1");
        assert_eq!(ctx.session_id, "session-1");
        assert_eq!(ctx.phase, "implementation");
        assert_eq!(ctx.working_files.len(), 1);
    }

    #[test]
    fn test_format_for_prompt() {
        let ctx = AgentContext::new("agent-1", "session-1")
            .with_phase("implementation")
            .with_participants(vec![ParticipantSummary {
                id: "agent-2".to_string(),
                module: "api".to_string(),
                roles: vec!["coder".to_string()],
                domains: vec![],
                status: AgentStatus::Busy,
            }])
            .with_task(TaskAssignment {
                id: "task-1".to_string(),
                description: "Implement authentication".to_string(),
                module: "auth".to_string(),
                role: "coder".to_string(),
                files: vec!["auth.rs".to_string()],
                dependencies: Vec::new(),
                priority: 1,
                instructions: None,
            });

        let prompt = ctx.format_for_prompt();

        assert!(prompt.contains("session-1"));
        assert!(prompt.contains("implementation"));
        assert!(prompt.contains("task-1"));
        assert!(prompt.contains("agent-2"));
    }

    #[test]
    fn test_agents_on_same_files() {
        let ctx = AgentContext::new("agent-1", "session-1")
            .with_working_files(vec!["shared.rs".to_string(), "auth.rs".to_string()])
            .with_locked_files(HashMap::from([
                ("shared.rs".to_string(), "agent-2".to_string()),
                ("other.rs".to_string(), "agent-3".to_string()),
            ]));

        let overlapping = ctx.agents_on_same_files();
        assert_eq!(overlapping.len(), 1);
        assert!(overlapping.contains(&"agent-2"));
    }

    #[test]
    fn test_notification_summary() {
        use super::super::notification::NotificationType;

        let notification = Notification::new(
            1,
            "agent-1",
            "*",
            NotificationType::TaskCompleted {
                task_id: "task-1".to_string(),
                agent_id: "agent-1".to_string(),
                files_modified: vec!["auth.rs".to_string()],
            },
        );

        let summary = NotificationSummary::from_notification(&notification, "agent-2");
        assert_eq!(summary.id, 1);
        assert!(summary.summary.contains("task-1"));
        assert!(!summary.acknowledged);
    }
}
