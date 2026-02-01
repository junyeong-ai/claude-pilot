//! Notification log for stateless agent communication.
//!
//! Implements the Notification Log pattern where notifications are stored
//! and queried by the orchestrator, then injected into agent context.
//! This solves the stateless agent + broadcast incompatibility problem.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::multi::AgentId;

/// Type of notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationType {
    /// Task was assigned to an agent.
    TaskAssigned { task_id: String, agent_id: String },
    /// Task completed successfully.
    TaskCompleted {
        task_id: String,
        agent_id: String,
        files_modified: Vec<String>,
    },
    /// Task failed.
    TaskFailed {
        task_id: String,
        agent_id: String,
        error: String,
    },
    /// Conflict detected between agents.
    ConflictDetected {
        conflict_id: String,
        agents: Vec<String>,
        files: Vec<String>,
        description: String,
    },
    /// Conflict was resolved.
    ConflictResolved {
        conflict_id: String,
        resolution: String,
    },
    /// Progress update from an agent.
    ProgressUpdate {
        agent_id: String,
        task_id: String,
        progress_percent: u32,
        message: String,
    },
    /// Resource lock acquired.
    LockAcquired { resource: String, holder: String },
    /// Resource lock released.
    LockReleased { resource: String, holder: String },
    /// Coordination request.
    CoordinationRequest {
        request_id: String,
        requester: String,
        coordination_type: String,
        data: String,
    },
    /// Phase transition.
    PhaseChanged {
        from_phase: String,
        to_phase: String,
    },
    /// Checkpoint created.
    CheckpointCreated {
        checkpoint_id: String,
        reason: String,
    },
    /// Generic text notification.
    Text { message: String },
}

impl NotificationType {
    pub fn is_conflict_related(&self) -> bool {
        matches!(
            self,
            Self::ConflictDetected { .. } | Self::ConflictResolved { .. }
        )
    }

    pub fn is_task_related(&self) -> bool {
        matches!(
            self,
            Self::TaskAssigned { .. }
                | Self::TaskCompleted { .. }
                | Self::TaskFailed { .. }
                | Self::ProgressUpdate { .. }
        )
    }

    pub fn is_lock_related(&self) -> bool {
        matches!(self, Self::LockAcquired { .. } | Self::LockReleased { .. })
    }

    pub fn affected_task(&self) -> Option<&str> {
        match self {
            Self::TaskAssigned { task_id, .. }
            | Self::TaskCompleted { task_id, .. }
            | Self::TaskFailed { task_id, .. }
            | Self::ProgressUpdate { task_id, .. } => Some(task_id),
            _ => None,
        }
    }

    pub fn affected_files(&self) -> Vec<&str> {
        match self {
            Self::TaskCompleted { files_modified, .. } => {
                files_modified.iter().map(|s| s.as_str()).collect()
            }
            Self::ConflictDetected { files, .. } => files.iter().map(|s| s.as_str()).collect(),
            _ => Vec::new(),
        }
    }
}

/// A notification entry in the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Unique notification ID.
    pub id: u64,
    /// Notification timestamp.
    #[serde(with = "instant_serde")]
    pub timestamp: Instant,
    /// Source of the notification.
    pub source: String,
    /// Target ("*" for broadcast, or specific agent ID).
    pub target: String,
    /// Notification type and payload.
    pub notification_type: NotificationType,
    /// Priority (higher = more important).
    pub priority: u32,
    /// Agents that have acknowledged this notification.
    #[serde(skip)]
    pub acknowledged_by: HashSet<String>,
}

mod instant_serde {
    use std::time::{Duration, Instant};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(instant: &Instant, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        instant.elapsed().as_millis().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Instant, D::Error>
    where
        D: Deserializer<'de>,
    {
        let elapsed_ms = u128::deserialize(deserializer)?;
        Ok(Instant::now() - Duration::from_millis(elapsed_ms as u64))
    }
}

impl Notification {
    pub fn new(id: u64, source: &str, target: &str, notification_type: NotificationType) -> Self {
        Self {
            id,
            timestamp: Instant::now(),
            source: source.to_string(),
            target: target.to_string(),
            notification_type,
            priority: 0,
            acknowledged_by: HashSet::new(),
        }
    }

    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    pub fn is_broadcast(&self) -> bool {
        self.target == "*"
    }

    pub fn is_for_agent(&self, agent_id: &str) -> bool {
        self.target == "*" || self.target == agent_id
    }

    pub fn age(&self) -> Duration {
        self.timestamp.elapsed()
    }

    pub fn acknowledge(&mut self, agent_id: &str) {
        self.acknowledged_by.insert(agent_id.to_string());
    }

    pub fn is_acknowledged_by(&self, agent_id: &str) -> bool {
        self.acknowledged_by.contains(agent_id)
    }
}

/// Query filter for notifications.
#[derive(Debug, Default, Clone)]
pub struct NotificationFilter {
    /// Include only notifications for this agent.
    pub for_agent: Option<String>,
    /// Include only notifications after this ID.
    pub after_id: Option<u64>,
    /// Include only notifications newer than this duration.
    pub max_age: Option<Duration>,
    /// Include only these notification types.
    pub types: Option<Vec<NotificationTypeFilter>>,
    /// Include only notifications related to these tasks.
    pub related_tasks: Option<Vec<String>>,
    /// Include only notifications related to these files.
    pub related_files: Option<Vec<String>>,
    /// Maximum number of notifications to return.
    pub limit: Option<usize>,
    /// Exclude already acknowledged notifications.
    pub exclude_acknowledged: bool,
}

/// Filter for notification types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationTypeFilter {
    TaskRelated,
    ConflictRelated,
    LockRelated,
    PhaseChange,
    All,
}

impl NotificationFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn for_agent(mut self, agent_id: &str) -> Self {
        self.for_agent = Some(agent_id.to_string());
        self
    }

    pub fn after_id(mut self, id: u64) -> Self {
        self.after_id = Some(id);
        self
    }

    pub fn max_age(mut self, duration: Duration) -> Self {
        self.max_age = Some(duration);
        self
    }

    pub fn task_related_only(mut self) -> Self {
        self.types = Some(vec![NotificationTypeFilter::TaskRelated]);
        self
    }

    pub fn conflict_related_only(mut self) -> Self {
        self.types = Some(vec![NotificationTypeFilter::ConflictRelated]);
        self
    }

    pub fn related_to_tasks(mut self, tasks: Vec<String>) -> Self {
        self.related_tasks = Some(tasks);
        self
    }

    pub fn related_to_files(mut self, files: Vec<String>) -> Self {
        self.related_files = Some(files);
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn exclude_acknowledged(mut self) -> Self {
        self.exclude_acknowledged = true;
        self
    }
}

/// The notification log.
#[derive(Debug)]
pub struct NotificationLog {
    notifications: VecDeque<Notification>,
    next_id: u64,
    max_entries: usize,
    max_age: Duration,
    by_task: HashMap<String, Vec<u64>>,
    by_file: HashMap<String, Vec<u64>>,
}

impl Default for NotificationLog {
    fn default() -> Self {
        Self::new(10000, Duration::from_secs(3600)) // 1 hour default
    }
}

impl NotificationLog {
    pub fn new(max_entries: usize, max_age: Duration) -> Self {
        Self {
            notifications: VecDeque::with_capacity(max_entries),
            next_id: 1,
            max_entries,
            max_age,
            by_task: HashMap::new(),
            by_file: HashMap::new(),
        }
    }

    /// Add a notification to the log.
    pub fn add(&mut self, source: &str, target: &str, notification_type: NotificationType) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        // Index by task
        if let Some(task_id) = notification_type.affected_task() {
            self.by_task
                .entry(task_id.to_string())
                .or_default()
                .push(id);
        }

        // Index by files
        for file in notification_type.affected_files() {
            self.by_file.entry(file.to_string()).or_default().push(id);
        }

        let notification = Notification::new(id, source, target, notification_type);
        self.notifications.push_back(notification);

        // Cleanup old entries
        self.cleanup();

        id
    }

    /// Add a notification with priority.
    pub fn add_with_priority(
        &mut self,
        source: &str,
        target: &str,
        notification_type: NotificationType,
        priority: u32,
    ) -> u64 {
        let id = self.add(source, target, notification_type);
        if let Some(n) = self.notifications.back_mut() {
            n.priority = priority;
        }
        id
    }

    /// Query notifications with a filter.
    pub fn query(&self, filter: &NotificationFilter) -> Vec<&Notification> {
        let mut results: Vec<&Notification> = self
            .notifications
            .iter()
            .filter(|n| self.matches_filter(n, filter))
            .collect();

        // Sort by priority (descending), then by ID (ascending)
        results.sort_by(|a, b| b.priority.cmp(&a.priority).then_with(|| a.id.cmp(&b.id)));

        if let Some(limit) = filter.limit {
            results.truncate(limit);
        }

        results
    }

    fn matches_filter(&self, notification: &Notification, filter: &NotificationFilter) -> bool {
        if let Some(agent) = &filter.for_agent
            && !notification.is_for_agent(agent)
        {
            return false;
        }

        if let Some(after_id) = filter.after_id
            && notification.id <= after_id
        {
            return false;
        }

        if let Some(max_age) = filter.max_age
            && notification.age() > max_age
        {
            return false;
        }

        if let Some(types) = &filter.types {
            let matches_type = types.iter().any(|t| match t {
                NotificationTypeFilter::TaskRelated => {
                    notification.notification_type.is_task_related()
                }
                NotificationTypeFilter::ConflictRelated => {
                    notification.notification_type.is_conflict_related()
                }
                NotificationTypeFilter::LockRelated => {
                    notification.notification_type.is_lock_related()
                }
                NotificationTypeFilter::PhaseChange => {
                    matches!(
                        notification.notification_type,
                        NotificationType::PhaseChanged { .. }
                    )
                }
                NotificationTypeFilter::All => true,
            });
            if !matches_type {
                return false;
            }
        }

        if let Some(tasks) = &filter.related_tasks {
            if let Some(task_id) = notification.notification_type.affected_task() {
                if !tasks.iter().any(|t| t == task_id) {
                    return false;
                }
            } else {
                return false;
            }
        }

        if let Some(files) = &filter.related_files {
            let affected = notification.notification_type.affected_files();
            if !files.iter().any(|f| affected.contains(&f.as_str())) {
                return false;
            }
        }

        if filter.exclude_acknowledged
            && let Some(agent) = &filter.for_agent
            && notification.is_acknowledged_by(agent)
        {
            return false;
        }

        true
    }

    /// Get notifications for an agent (for context injection).
    pub fn for_agent(&self, agent_id: &AgentId, limit: usize) -> Vec<&Notification> {
        self.query(
            &NotificationFilter::new()
                .for_agent(agent_id.as_str())
                .limit(limit),
        )
    }

    /// Get unacknowledged notifications for an agent.
    pub fn unacknowledged_for_agent(&self, agent_id: &str) -> Vec<&Notification> {
        self.query(
            &NotificationFilter::new()
                .for_agent(agent_id)
                .exclude_acknowledged(),
        )
    }

    /// Get notifications related to specific tasks.
    pub fn for_tasks(&self, task_ids: &[String]) -> Vec<&Notification> {
        self.query(&NotificationFilter::new().related_to_tasks(task_ids.to_vec()))
    }

    /// Get notifications related to specific files.
    pub fn for_files(&self, files: &[String]) -> Vec<&Notification> {
        self.query(&NotificationFilter::new().related_to_files(files.to_vec()))
    }

    /// Acknowledge a notification for an agent.
    pub fn acknowledge(&mut self, notification_id: u64, agent_id: &str) {
        if let Some(n) = self
            .notifications
            .iter_mut()
            .find(|n| n.id == notification_id)
        {
            n.acknowledge(agent_id);
        }
    }

    /// Get the latest notification ID.
    pub fn latest_id(&self) -> u64 {
        self.next_id.saturating_sub(1)
    }

    /// Get notification count.
    pub fn len(&self) -> usize {
        self.notifications.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty()
    }

    /// Cleanup old entries.
    fn cleanup(&mut self) {
        // Remove entries exceeding max_entries
        while self.notifications.len() > self.max_entries {
            self.notifications.pop_front();
        }

        // Remove entries older than max_age
        let now = Instant::now();
        while let Some(front) = self.notifications.front() {
            if now.duration_since(front.timestamp) > self.max_age {
                self.notifications.pop_front();
            } else {
                break;
            }
        }
    }

    /// Format notifications for context injection.
    pub fn format_for_context(&self, notifications: &[&Notification]) -> String {
        if notifications.is_empty() {
            return String::new();
        }

        let mut output = String::from("## Recent Notifications\n\n");

        for n in notifications {
            let type_str = match &n.notification_type {
                NotificationType::TaskAssigned { task_id, agent_id } => {
                    format!("Task '{}' assigned to '{}'", task_id, agent_id)
                }
                NotificationType::TaskCompleted {
                    task_id,
                    agent_id,
                    files_modified,
                } => {
                    format!(
                        "Task '{}' completed by '{}' (files: {})",
                        task_id,
                        agent_id,
                        files_modified.join(", ")
                    )
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
                    agents,
                    description,
                    ..
                } => {
                    format!(
                        "CONFLICT [{}]: {} (agents: {})",
                        conflict_id,
                        description,
                        agents.join(", ")
                    )
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
                        "Progress: {} on '{}' - {}% - {}",
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
                        "Coordination request [{}] from '{}': {}",
                        request_id, requester, coordination_type
                    )
                }
                NotificationType::PhaseChanged {
                    from_phase,
                    to_phase,
                } => {
                    format!("Phase changed: {} -> {}", from_phase, to_phase)
                }
                NotificationType::CheckpointCreated {
                    checkpoint_id,
                    reason,
                } => {
                    format!("Checkpoint created [{}]: {}", checkpoint_id, reason)
                }
                NotificationType::Text { message } => message.clone(),
            };

            output.push_str(&format!("- [#{}] {}\n", n.id, type_str));
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_lifecycle() {
        let mut log = NotificationLog::new(100, Duration::from_secs(3600));

        let id1 = log.add(
            "agent-1",
            "*",
            NotificationType::TaskAssigned {
                task_id: "task-1".to_string(),
                agent_id: "agent-1".to_string(),
            },
        );

        let id2 = log.add(
            "agent-1",
            "coordinator",
            NotificationType::TaskCompleted {
                task_id: "task-1".to_string(),
                agent_id: "agent-1".to_string(),
                files_modified: vec!["auth.rs".to_string()],
            },
        );

        assert_eq!(log.len(), 2);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn test_query_for_agent() {
        let mut log = NotificationLog::new(100, Duration::from_secs(3600));

        log.add(
            "coordinator",
            "*",
            NotificationType::Text {
                message: "Broadcast".to_string(),
            },
        );
        log.add(
            "coordinator",
            "agent-1",
            NotificationType::Text {
                message: "For agent-1".to_string(),
            },
        );
        log.add(
            "coordinator",
            "agent-2",
            NotificationType::Text {
                message: "For agent-2".to_string(),
            },
        );

        let agent1_notifications = log.for_agent(&AgentId::new("agent-1"), 10);
        assert_eq!(agent1_notifications.len(), 2); // Broadcast + direct
    }

    #[test]
    fn test_task_related_filter() {
        let mut log = NotificationLog::new(100, Duration::from_secs(3600));

        log.add(
            "agent-1",
            "*",
            NotificationType::TaskCompleted {
                task_id: "task-1".to_string(),
                agent_id: "agent-1".to_string(),
                files_modified: Vec::new(),
            },
        );
        log.add(
            "coordinator",
            "*",
            NotificationType::PhaseChanged {
                from_phase: "planning".to_string(),
                to_phase: "implementation".to_string(),
            },
        );

        let task_notifications = log.query(&NotificationFilter::new().task_related_only());
        assert_eq!(task_notifications.len(), 1);
    }

    #[test]
    fn test_acknowledgment() {
        let mut log = NotificationLog::new(100, Duration::from_secs(3600));

        let id = log.add(
            "coordinator",
            "*",
            NotificationType::Text {
                message: "Test".to_string(),
            },
        );

        let unack = log.unacknowledged_for_agent("agent-1");
        assert_eq!(unack.len(), 1);

        log.acknowledge(id, "agent-1");

        let unack = log.unacknowledged_for_agent("agent-1");
        assert_eq!(unack.len(), 0);
    }

    #[test]
    fn test_format_for_context() {
        let mut log = NotificationLog::new(100, Duration::from_secs(3600));

        log.add(
            "agent-1",
            "*",
            NotificationType::TaskCompleted {
                task_id: "task-1".to_string(),
                agent_id: "agent-1".to_string(),
                files_modified: vec!["auth.rs".to_string()],
            },
        );

        let notifications = log.query(&NotificationFilter::new());
        let formatted = log.format_for_context(&notifications);

        assert!(formatted.contains("## Recent Notifications"));
        assert!(formatted.contains("task-1"));
        assert!(formatted.contains("auth.rs"));
    }
}
