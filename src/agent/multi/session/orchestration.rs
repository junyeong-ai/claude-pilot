//! Orchestration session - the central coordination unit.
//!
//! The `OrchestrationSession` manages the entire lifecycle of a multi-agent mission,
//! including participant tracking, task dependencies, notifications, checkpoints,
//! and phase transitions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::checkpoint::{
    Checkpoint, CheckpointManager, CheckpointReason, CheckpointRef, RecoveryContext,
};
use super::notification::{Notification, NotificationFilter, NotificationLog, NotificationType};
use super::participant::{AgentCapabilities, Participant, ParticipantRegistry, ParticipantSummary};
use super::task_graph::{
    DependencyType, TaskDAG, TaskDependency, TaskInfo, TaskResult, TaskStats, TaskStatus,
};
use crate::agent::multi::AgentId;

/// Session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// Session is being initialized.
    Initializing,
    /// Session is active and running.
    Active,
    /// Session is paused.
    Paused,
    /// Session completed successfully.
    Completed,
    /// Session failed.
    Failed,
    /// Session was cancelled.
    Cancelled,
}

impl SessionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn can_accept_tasks(&self) -> bool {
        matches!(self, Self::Initializing | Self::Active)
    }
}

/// Phase of the orchestration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Initial setup and participant registration.
    Setup,
    /// Planning phase - agents discuss and reach consensus.
    Planning,
    /// Implementation phase - coders execute tasks.
    Implementation,
    /// Verification phase - reviewers check work.
    Verification,
    /// Cleanup and finalization.
    Finalization,
    /// Custom phase.
    Custom(String),
}

impl Phase {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Setup => "setup",
            Self::Planning => "planning",
            Self::Implementation => "implementation",
            Self::Verification => "verification",
            Self::Finalization => "finalization",
            Self::Custom(s) => s,
        }
    }

    pub fn next(&self) -> Option<Self> {
        match self {
            Self::Setup => Some(Self::Planning),
            Self::Planning => Some(Self::Implementation),
            Self::Implementation => Some(Self::Verification),
            Self::Verification => Some(Self::Finalization),
            Self::Finalization => None,
            Self::Custom(_) => None,
        }
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Configuration for an orchestration session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum session duration.
    pub max_duration: Duration,
    /// Auto-checkpoint interval.
    pub checkpoint_interval: Duration,
    /// Context budget threshold for checkpointing (0.0-1.0).
    pub context_budget_threshold: f32,
    /// Maximum parallel tasks.
    pub max_parallel_tasks: usize,
    /// Maximum retries per task.
    pub max_task_retries: u32,
    /// Enable cross-workspace coordination.
    pub enable_cross_workspace: bool,
    /// Maximum notification log entries.
    pub max_notifications: usize,
    /// Notification max age.
    pub notification_max_age: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_duration: Duration::from_secs(86400),      // 24 hours
            checkpoint_interval: Duration::from_secs(300), // 5 minutes
            context_budget_threshold: 0.8,
            max_parallel_tasks: 10,
            max_task_retries: 3,
            enable_cross_workspace: true,
            max_notifications: 10000,
            notification_max_age: Duration::from_secs(3600),
        }
    }
}

/// The central orchestration session.
pub struct OrchestrationSession {
    /// Unique session ID.
    pub id: String,
    /// Mission description.
    pub mission: String,
    /// Current status.
    status: SessionStatus,
    /// Current phase.
    phase: Phase,
    /// Configuration.
    config: SessionConfig,
    /// Participant registry.
    participants: ParticipantRegistry,
    /// Task DAG.
    tasks: TaskDAG,
    /// Notification log.
    notifications: NotificationLog,
    /// Checkpoint manager.
    checkpoints: CheckpointManager,
    /// Context budget tracking.
    context_budget: ContextBudget,
    /// Session start time.
    started_at: Instant,
    /// Working directory.
    working_dir: PathBuf,
    /// Metadata.
    metadata: HashMap<String, String>,
}

/// Context budget tracking.
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Total available tokens.
    pub total: u32,
    /// Currently used tokens.
    pub used: u32,
    /// Reserved for system overhead.
    pub reserved: u32,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            total: 200000, // 200k default
            used: 0,
            reserved: 10000, // 10k reserved
        }
    }
}

impl ContextBudget {
    pub fn available(&self) -> u32 {
        self.total
            .saturating_sub(self.used)
            .saturating_sub(self.reserved)
    }

    pub fn usage_ratio(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        self.used as f32 / self.total as f32
    }

    pub fn add_usage(&mut self, tokens: u32) {
        self.used = self.used.saturating_add(tokens);
    }

    pub fn reset(&mut self) {
        self.used = 0;
    }
}

impl OrchestrationSession {
    /// Create a new orchestration session.
    pub fn new(mission: &str, working_dir: PathBuf) -> Self {
        let id = generate_session_id();
        Self {
            id: id.clone(),
            mission: mission.to_string(),
            status: SessionStatus::Initializing,
            phase: Phase::Setup,
            config: SessionConfig::default(),
            participants: ParticipantRegistry::new(),
            tasks: TaskDAG::new(),
            notifications: NotificationLog::default(),
            checkpoints: CheckpointManager::new(&id),
            context_budget: ContextBudget::default(),
            started_at: Instant::now(),
            working_dir,
            metadata: HashMap::new(),
        }
    }

    /// Create with custom configuration.
    pub fn with_config(mut self, config: SessionConfig) -> Self {
        self.config = config.clone();
        self.notifications =
            NotificationLog::new(config.max_notifications, config.notification_max_age);
        self.checkpoints = self
            .checkpoints
            .with_auto_interval(config.checkpoint_interval)
            .with_budget_threshold(config.context_budget_threshold);
        self
    }

    /// Set context budget.
    pub fn with_context_budget(mut self, total: u32, reserved: u32) -> Self {
        self.context_budget = ContextBudget {
            total,
            used: 0,
            reserved,
        };
        self
    }

    /// Add metadata.
    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    // === Status and Phase Management ===

    /// Get current status.
    pub fn status(&self) -> SessionStatus {
        self.status
    }

    /// Get current phase.
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Start the session.
    pub fn start(&mut self) {
        if self.status == SessionStatus::Initializing {
            self.status = SessionStatus::Active;
            self.notify_phase_change("setup", "planning");
        }
    }

    /// Transition to the next phase.
    pub fn advance_phase(&mut self) -> Option<&Phase> {
        if let Some(next) = self.phase.next() {
            let from = self.phase.as_str().to_string();
            let to = next.as_str().to_string();

            // Create checkpoint before phase transition
            self.create_phase_checkpoint(&from, &to);

            self.phase = next;
            self.notify_phase_change(&from, &to);
            Some(&self.phase)
        } else {
            None
        }
    }

    /// Set phase explicitly.
    pub fn set_phase(&mut self, phase: Phase) {
        let from = self.phase.as_str().to_string();
        let to = phase.as_str().to_string();

        if from != to {
            self.create_phase_checkpoint(&from, &to);
            self.phase = phase;
            self.notify_phase_change(&from, &to);
        }
    }

    /// Mark session as completed.
    pub fn complete(&mut self) {
        self.status = SessionStatus::Completed;
        self.set_phase(Phase::Finalization);
    }

    /// Mark session as failed.
    pub fn fail(&mut self, reason: &str) {
        self.status = SessionStatus::Failed;
        self.metadata
            .insert("failure_reason".to_string(), reason.to_string());
    }

    /// Pause the session.
    pub fn pause(&mut self) {
        if self.status == SessionStatus::Active {
            self.status = SessionStatus::Paused;
        }
    }

    /// Resume the session.
    pub fn resume(&mut self) {
        if self.status == SessionStatus::Paused {
            self.status = SessionStatus::Active;
        }
    }

    // === Participant Management ===

    /// Register a participant.
    pub fn register_participant(&mut self, participant: Participant) {
        let id = participant.id.clone();
        self.participants.register(participant);

        self.notifications.add(
            "session",
            "*",
            NotificationType::Text {
                message: format!("Participant registered: {}", id.as_str()),
            },
        );
    }

    /// Register a participant with full details.
    pub fn register_agent(
        &mut self,
        id: &str,
        module: &str,
        workspace: &str,
        capabilities: AgentCapabilities,
    ) {
        let participant =
            Participant::new(AgentId::new(id), module.to_string(), workspace.to_string())
                .with_capabilities(capabilities);
        self.register_participant(participant);
    }

    /// Get participant by ID.
    pub fn participant(&self, id: &AgentId) -> Option<&Participant> {
        self.participants.get(id)
    }

    /// Get mutable participant.
    pub fn participant_mut(&mut self, id: &AgentId) -> Option<&mut Participant> {
        self.participants.get_mut(id)
    }

    /// Get all participants.
    pub fn participants(&self) -> &ParticipantRegistry {
        &self.participants
    }

    /// Get participant summaries for context injection.
    pub fn participant_summaries(&self) -> Vec<ParticipantSummary> {
        self.participants.summaries()
    }

    /// Check if session spans multiple workspaces.
    pub fn is_cross_workspace(&self) -> bool {
        self.config.enable_cross_workspace && self.participants.spans_multiple_workspaces()
    }

    // === Task Management ===

    /// Add a task.
    pub fn add_task(&mut self, info: TaskInfo) {
        self.tasks.add_task(info);
        self.tasks.refresh_ready_status();
    }

    /// Add a dependency between tasks.
    pub fn add_task_dependency(
        &mut self,
        prerequisite: &str,
        dependent: &str,
        dependency_type: DependencyType,
    ) -> Result<(), String> {
        self.tasks.add_dependency(TaskDependency {
            prerequisite: prerequisite.to_string(),
            dependent: dependent.to_string(),
            dependency_type,
        })
    }

    /// Get ready tasks.
    pub fn ready_tasks(&self) -> Vec<&super::task_graph::TaskNode> {
        self.tasks.ready_tasks()
    }

    /// Get ready tasks for a module.
    pub fn ready_tasks_for_module(&self, module: &str) -> Vec<&super::task_graph::TaskNode> {
        self.tasks.ready_tasks_for_module(module)
    }

    /// Assign a task to an agent.
    pub fn assign_task(&mut self, task_id: &str, agent_id: &AgentId) -> Result<(), String> {
        // Update task
        self.tasks.start_task(task_id, agent_id.clone())?;

        // Update participant
        if let Some(p) = self.participants.get_mut(agent_id) {
            p.assign_task(task_id.to_string());
        }

        // Notify
        self.notifications.add(
            "session",
            "*",
            NotificationType::TaskAssigned {
                task_id: task_id.to_string(),
                agent_id: agent_id.as_str().to_string(),
            },
        );

        Ok(())
    }

    /// Complete a task.
    pub fn complete_task(
        &mut self,
        task_id: &str,
        agent_id: &AgentId,
        result: TaskResult,
    ) -> Result<(), String> {
        let files_modified = result.modified_files.clone();

        // Update task
        self.tasks.complete_task(task_id, result)?;

        // Update participant
        if let Some(p) = self.participants.get_mut(agent_id) {
            p.complete_task(task_id);
        }

        // Notify
        self.notifications.add(
            agent_id.as_str(),
            "*",
            NotificationType::TaskCompleted {
                task_id: task_id.to_string(),
                agent_id: agent_id.as_str().to_string(),
                files_modified,
            },
        );

        Ok(())
    }

    /// Fail a task.
    pub fn fail_task(
        &mut self,
        task_id: &str,
        agent_id: &AgentId,
        error: &str,
    ) -> Result<(), String> {
        // Update task
        self.tasks.fail_task(task_id, error.to_string())?;

        // Update participant
        if let Some(p) = self.participants.get_mut(agent_id) {
            p.complete_task(task_id);
        }

        // Notify
        self.notifications.add(
            agent_id.as_str(),
            "*",
            NotificationType::TaskFailed {
                task_id: task_id.to_string(),
                agent_id: agent_id.as_str().to_string(),
                error: error.to_string(),
            },
        );

        Ok(())
    }

    /// Retry a failed task.
    pub fn retry_task(&mut self, task_id: &str) -> Result<(), String> {
        let node = self
            .tasks
            .get(task_id)
            .ok_or_else(|| format!("Task not found: {}", task_id))?;
        if node.retry_count >= self.config.max_task_retries {
            return Err(format!(
                "Task {} exceeded max retries ({})",
                task_id, self.config.max_task_retries
            ));
        }
        self.tasks.retry_task(task_id)
    }

    /// Get task statistics.
    pub fn task_stats(&self) -> TaskStats {
        self.tasks.stats()
    }

    /// Check if all tasks are complete.
    pub fn all_tasks_complete(&self) -> bool {
        self.tasks.is_complete()
    }

    // === Notification Management ===

    /// Add a notification.
    pub fn notify(
        &mut self,
        source: &str,
        target: &str,
        notification_type: NotificationType,
    ) -> u64 {
        self.notifications.add(source, target, notification_type)
    }

    /// Notify conflict detected.
    pub fn notify_conflict(
        &mut self,
        conflict_id: &str,
        agents: Vec<String>,
        files: Vec<String>,
        description: &str,
    ) {
        self.notifications.add_with_priority(
            "session",
            "*",
            NotificationType::ConflictDetected {
                conflict_id: conflict_id.to_string(),
                agents,
                files,
                description: description.to_string(),
            },
            100, // High priority
        );
    }

    /// Get notifications for an agent.
    pub fn notifications_for_agent(&self, agent_id: &AgentId, limit: usize) -> Vec<&Notification> {
        self.notifications.for_agent(agent_id, limit)
    }

    /// Query notifications.
    pub fn query_notifications(&self, filter: &NotificationFilter) -> Vec<&Notification> {
        self.notifications.query(filter)
    }

    /// Format notifications for context injection.
    pub fn format_notifications_for_context(&self, agent_id: &AgentId, limit: usize) -> String {
        let notifications = self.notifications_for_agent(agent_id, limit);
        self.notifications.format_for_context(&notifications)
    }

    /// Acknowledge notification.
    pub fn acknowledge_notification(&mut self, notification_id: u64, agent_id: &str) {
        self.notifications.acknowledge(notification_id, agent_id);
    }

    fn notify_phase_change(&mut self, from: &str, to: &str) {
        self.notifications.add(
            "session",
            "*",
            NotificationType::PhaseChanged {
                from_phase: from.to_string(),
                to_phase: to.to_string(),
            },
        );
    }

    // === Checkpoint Management ===

    /// Create a manual checkpoint.
    pub fn create_checkpoint(&mut self, reason: CheckpointReason) -> &Checkpoint {
        let refs = self.create_checkpoint_refs();
        let (completed, failed, in_progress) = self.task_id_lists();

        let cp = Checkpoint::new(&self.id, reason)
            .with_refs(refs)
            .with_phase(self.phase.as_str())
            .with_tasks(completed, failed, in_progress)
            .with_context_used(self.context_budget.used);

        self.checkpoints.create(cp)
    }

    /// Create recovery context from latest checkpoint.
    pub fn create_recovery_context(&self) -> Option<RecoveryContext> {
        self.checkpoints.create_recovery_context()
    }

    /// Check if checkpoint is needed for context budget.
    pub fn should_checkpoint_for_budget(&self) -> bool {
        self.checkpoints
            .should_checkpoint_for_budget(self.context_budget.used, self.context_budget.total)
    }

    /// Maybe create scheduled checkpoint.
    pub fn maybe_checkpoint(&mut self) -> Option<&Checkpoint> {
        let refs = self.create_checkpoint_refs();
        let (completed, failed, in_progress) = self.task_id_lists();

        self.checkpoints.maybe_create_scheduled(
            refs,
            self.phase.as_str(),
            completed,
            failed,
            in_progress,
        )
    }

    fn create_phase_checkpoint(&mut self, from: &str, to: &str) {
        let refs = self.create_checkpoint_refs();
        let (completed, failed, in_progress) = self.task_id_lists();

        self.checkpoints
            .create_phase_transition(from, to, refs, completed, failed, in_progress);
    }

    fn create_checkpoint_refs(&self) -> CheckpointRef {
        CheckpointRef {
            notification_id: self.notifications.latest_id(),
            task_statuses: self.task_status_map(),
            consensus_round: None,          // Set by ConsensusState if active
            file_snapshots: HashMap::new(), // Filled by external component
        }
    }

    fn task_status_map(&self) -> HashMap<String, String> {
        self.tasks
            .task_ids()
            .into_iter()
            .filter_map(|id| {
                self.tasks
                    .get(id)
                    .map(|node| (id.to_string(), format!("{:?}", node.status)))
            })
            .collect()
    }

    fn task_id_lists(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        let mut in_progress = Vec::new();

        for id in self.tasks.task_ids() {
            if let Some(node) = self.tasks.get(id) {
                match node.status {
                    TaskStatus::Completed => completed.push(id.to_string()),
                    TaskStatus::Failed => failed.push(id.to_string()),
                    TaskStatus::InProgress => in_progress.push(id.to_string()),
                    _ => {}
                }
            }
        }

        (completed, failed, in_progress)
    }

    // === Context Budget ===

    /// Get context budget.
    pub fn context_budget(&self) -> &ContextBudget {
        &self.context_budget
    }

    /// Update context usage.
    pub fn add_context_usage(&mut self, tokens: u32) {
        self.context_budget.add_usage(tokens);

        // Check if checkpoint needed
        if self.should_checkpoint_for_budget() {
            let refs = self.create_checkpoint_refs();
            self.checkpoints.create_budget_threshold(
                self.context_budget.used,
                self.context_budget.total,
                refs,
                self.phase.as_str(),
            );
        }
    }

    /// Reset context usage (after compaction).
    pub fn reset_context_usage(&mut self) {
        self.context_budget.reset();
    }

    // === Utility ===

    /// Get session duration.
    pub fn duration(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Get working directory.
    pub fn working_dir(&self) -> &PathBuf {
        &self.working_dir
    }

    /// Get configuration.
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Get metadata.
    pub fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }

    /// Generate session summary.
    pub fn summary(&self) -> SessionSummary {
        let stats = self.task_stats();
        SessionSummary {
            id: self.id.clone(),
            mission: self.mission.clone(),
            status: self.status,
            phase: self.phase.clone(),
            duration_secs: self.duration().as_secs(),
            participant_count: self.participants.len(),
            task_stats: stats,
            is_cross_workspace: self.is_cross_workspace(),
            context_usage_ratio: self.context_budget.usage_ratio(),
            checkpoint_count: self.checkpoints.len(),
        }
    }
}

/// Summary of session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub mission: String,
    pub status: SessionStatus,
    pub phase: Phase,
    pub duration_secs: u64,
    pub participant_count: usize,
    pub task_stats: TaskStats,
    pub is_cross_workspace: bool,
    pub context_usage_ratio: f32,
    pub checkpoint_count: usize,
}

fn generate_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("session-{:08x}", now.as_millis() as u32)
}

/// Thread-safe session wrapper.
pub type SharedSession = Arc<RwLock<OrchestrationSession>>;

/// Create a shared session.
pub fn create_shared_session(mission: &str, working_dir: PathBuf) -> SharedSession {
    Arc::new(RwLock::new(OrchestrationSession::new(mission, working_dir)))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::agent::multi::AgentRole;
    use std::path::PathBuf;

    fn create_test_session() -> OrchestrationSession {
        OrchestrationSession::new("Test mission", PathBuf::from("/tmp/test"))
    }

    #[test]
    fn test_session_lifecycle() {
        let mut session = create_test_session();

        assert_eq!(session.status(), SessionStatus::Initializing);
        assert_eq!(session.phase(), &Phase::Setup);

        session.start();
        assert_eq!(session.status(), SessionStatus::Active);

        session.advance_phase(); // Setup -> Planning
        assert_eq!(session.phase(), &Phase::Planning);

        session.advance_phase(); // Planning -> Implementation
        assert_eq!(session.phase(), &Phase::Implementation);

        session.complete();
        assert_eq!(session.status(), SessionStatus::Completed);
    }

    #[test]
    fn test_participant_management() {
        let mut session = create_test_session();

        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("planning")];
        caps.max_concurrent_tasks = 2;

        session.register_agent("auth-planning-0", "auth", "project-a", caps);

        assert_eq!(session.participants().len(), 1);
        assert!(
            session
                .participant(&AgentId::new("auth-planning-0"))
                .is_some()
        );
    }

    #[test]
    fn test_task_workflow() {
        let mut session = create_test_session();

        // Add task
        session.add_task(TaskInfo {
            id: "task-1".to_string(),
            description: "Implement feature".to_string(),
            module: "auth".to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: 1000,
            priority: 1,
            affected_files: vec!["auth.rs".to_string()],
        });

        // Register agent
        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("coder")];
        caps.max_concurrent_tasks = 2;
        session.register_agent("auth-coder-0", "auth", "project-a", caps);

        // Assign task
        let agent_id = AgentId::new("auth-coder-0");
        session.assign_task("task-1", &agent_id).unwrap();

        // Complete task
        session
            .complete_task(
                "task-1",
                &agent_id,
                TaskResult {
                    success: true,
                    output: "Done".to_string(),
                    modified_files: vec!["auth.rs".to_string()],
                    artifacts: Vec::new(),
                },
            )
            .unwrap();

        let stats = session.task_stats();
        assert_eq!(stats.completed, 1);
    }

    #[test]
    fn test_notifications() {
        let mut session = create_test_session();

        session.notify(
            "agent-1",
            "*",
            NotificationType::Text {
                message: "Hello".to_string(),
            },
        );

        let notifications = session.notifications_for_agent(&AgentId::new("anyone"), 10);
        assert_eq!(notifications.len(), 1);
    }

    #[test]
    fn test_context_budget() {
        let mut session = create_test_session().with_context_budget(100000, 5000);

        assert_eq!(session.context_budget().available(), 95000);

        session.add_context_usage(50000);
        assert_eq!(session.context_budget().used, 50000);
        assert!((session.context_budget().usage_ratio() - 0.5).abs() < 0.01);
    }
}
