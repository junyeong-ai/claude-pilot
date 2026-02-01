//! Communication Tools for LLM agent interaction.
//!
//! Provides the 5-tool interface that agents use to interact with the
//! session infrastructure during stateless invocations.
//!
//! # Tools
//!
//! 1. `notify` - Send notification to other agents
//! 2. `query` - Query notifications, participants, and state
//! 3. `acquire_lock` - Acquire resource lock (immediate response, no wait)
//! 4. `coordinate` - Request coordination with other agents
//! 5. `save_progress` - Save task progress
//!
//! # Design Principles
//!
//! - **Stateless**: Each tool call is independent, no session state in agent
//! - **Immediate**: All responses are immediate, no blocking waits
//! - **Serializable**: All inputs/outputs are JSON-serializable for LLM

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::notification::{Notification, NotificationFilter, NotificationLog, NotificationType};
use super::participant::{ParticipantRegistry, ParticipantSummary};
use super::priority_bus::Priority;
use super::shared_state::{LockResult, LockType, SharedState};
use crate::agent::multi::AgentId;

/// Result type for communication tool operations.
pub type ToolResult<T> = Result<T, ToolError>;

/// Errors that can occur during tool operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolError {
    /// Resource is locked by another agent.
    ResourceLocked { resource: String, holder: String },
    /// Invalid parameters provided.
    InvalidParams { message: String },
    /// Agent not found.
    AgentNotFound { agent_id: String },
    /// Serialization error.
    SerializationError { message: String },
    /// Internal error.
    Internal { message: String },
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResourceLocked { resource, holder } => {
                write!(f, "Resource '{}' is locked by '{}'", resource, holder)
            }
            Self::InvalidParams { message } => write!(f, "Invalid parameters: {}", message),
            Self::AgentNotFound { agent_id } => write!(f, "Agent not found: {}", agent_id),
            Self::SerializationError { message } => write!(f, "Serialization error: {}", message),
            Self::Internal { message } => write!(f, "Internal error: {}", message),
        }
    }
}

impl std::error::Error for ToolError {}

// ============================================================================
// Tool Inputs
// ============================================================================

/// Input for the `notify` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyInput {
    /// Target agent(s): specific ID, "*" for broadcast.
    pub target: String,
    /// Notification type.
    pub notification_type: NotificationTypeInput,
    /// Optional priority (defaults to Normal).
    pub priority: Option<PriorityInput>,
}

/// Notification type for input - simplified for LLM tool calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationTypeInput {
    /// Text message.
    Text { message: String },
    /// Task completion.
    TaskCompleted {
        task_id: String,
        files_modified: Vec<String>,
    },
    /// Task failure.
    TaskFailed { task_id: String, error: String },
    /// Progress update.
    Progress {
        task_id: String,
        progress_percent: u32,
        message: String,
    },
    /// Conflict detected.
    Conflict {
        conflict_id: String,
        agents: Vec<String>,
        files: Vec<String>,
        description: String,
    },
    /// Coordination request.
    Coordination {
        request_id: String,
        coordination_type: String,
        data: String,
    },
}

impl NotificationTypeInput {
    /// Convert to NotificationType for the notification log.
    pub fn to_notification_type(&self, agent_id: &str) -> NotificationType {
        match self {
            Self::Text { message } => NotificationType::Text {
                message: message.clone(),
            },
            Self::TaskCompleted {
                task_id,
                files_modified,
            } => NotificationType::TaskCompleted {
                task_id: task_id.clone(),
                agent_id: agent_id.to_string(),
                files_modified: files_modified.clone(),
            },
            Self::TaskFailed { task_id, error } => NotificationType::TaskFailed {
                task_id: task_id.clone(),
                agent_id: agent_id.to_string(),
                error: error.clone(),
            },
            Self::Progress {
                task_id,
                progress_percent,
                message,
            } => NotificationType::ProgressUpdate {
                agent_id: agent_id.to_string(),
                task_id: task_id.clone(),
                progress_percent: *progress_percent,
                message: message.clone(),
            },
            Self::Conflict {
                conflict_id,
                agents,
                files,
                description,
            } => NotificationType::ConflictDetected {
                conflict_id: conflict_id.clone(),
                agents: agents.clone(),
                files: files.clone(),
                description: description.clone(),
            },
            Self::Coordination {
                request_id,
                coordination_type,
                data,
            } => NotificationType::CoordinationRequest {
                request_id: request_id.clone(),
                requester: agent_id.to_string(),
                coordination_type: coordination_type.clone(),
                data: data.clone(),
            },
        }
    }

    /// Get the default message bus priority for this type.
    pub fn default_priority(&self) -> Priority {
        match self {
            Self::Conflict { .. } => Priority::Critical,
            Self::TaskFailed { .. } => Priority::High,
            Self::TaskCompleted { .. } | Self::Coordination { .. } => Priority::Normal,
            Self::Progress { .. } => Priority::Low,
            Self::Text { .. } => Priority::Normal,
        }
    }
}

/// Priority for input.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityInput {
    Critical,
    High,
    Normal,
    Low,
    Background,
}

impl From<PriorityInput> for Priority {
    fn from(input: PriorityInput) -> Self {
        match input {
            PriorityInput::Critical => Priority::Critical,
            PriorityInput::High => Priority::High,
            PriorityInput::Normal => Priority::Normal,
            PriorityInput::Low => Priority::Low,
            PriorityInput::Background => Priority::Background,
        }
    }
}

/// Input for the `query` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryInput {
    /// What to query.
    pub query_type: QueryType,
    /// Optional filter parameters.
    #[serde(default)]
    pub filters: QueryFilters,
    /// Maximum results to return.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

/// Types of queries supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryType {
    /// Query notifications.
    Notifications,
    /// Query participants.
    Participants,
    /// Query locks.
    Locks,
    /// Query progress.
    Progress,
    /// Query coordination data.
    Coordination,
}

/// Filters for queries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryFilters {
    /// Filter by agent ID.
    pub agent_id: Option<String>,
    /// Filter by module.
    pub module: Option<String>,
    /// Filter by role.
    pub role: Option<String>,
    /// Filter by task ID.
    pub task_id: Option<String>,
    /// Filter for task-related notifications only.
    #[serde(default)]
    pub task_related_only: bool,
    /// Filter for conflict-related notifications only.
    #[serde(default)]
    pub conflict_related_only: bool,
    /// Exclude acknowledged notifications.
    #[serde(default)]
    pub exclude_acknowledged: bool,
    /// Filter by coordination key.
    pub coordination_key: Option<String>,
}

/// Input for the `acquire_lock` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquireLockInput {
    /// Resource to lock.
    pub resource: String,
    /// Lock type.
    #[serde(default)]
    pub lock_type: LockTypeInput,
    /// Duration in seconds (optional, defaults to 300).
    pub duration_secs: Option<u64>,
}

/// Lock type for input.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockTypeInput {
    #[default]
    Exclusive,
    Shared,
}

impl From<LockTypeInput> for LockType {
    fn from(input: LockTypeInput) -> Self {
        match input {
            LockTypeInput::Exclusive => LockType::Exclusive,
            LockTypeInput::Shared => LockType::Shared,
        }
    }
}

/// Input for the `coordinate` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinateInput {
    /// Coordination action.
    pub action: CoordinationAction,
    /// Key for the coordination data.
    pub key: String,
    /// Data payload (for set/update actions).
    pub data: Option<serde_json::Value>,
}

/// Coordination actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationAction {
    /// Get coordination data.
    Get,
    /// Set coordination data (creates or updates).
    Set,
    /// Delete coordination data.
    Delete,
    /// Increment a counter.
    Increment,
    /// Decrement a counter.
    Decrement,
}

/// Input for the `save_progress` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveProgressInput {
    /// Task ID.
    pub task_id: String,
    /// Progress percentage (0-100).
    pub progress_percent: u32,
    /// Status message.
    pub message: String,
    /// Optional artifacts produced.
    #[serde(default)]
    pub artifacts: Vec<String>,
}

// ============================================================================
// Tool Outputs
// ============================================================================

/// Output for the `notify` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyOutput {
    /// Whether the notification was sent successfully.
    pub success: bool,
    /// Notification ID.
    pub notification_id: u64,
}

/// Output for the `query` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOutput {
    /// Query results.
    pub results: QueryResults,
    /// Total count before limit.
    pub total_count: usize,
    /// Whether results were truncated.
    pub truncated: bool,
}

/// Query results by type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QueryResults {
    Notifications(Vec<QueryNotification>),
    Participants(Vec<ParticipantSummary>),
    Locks(Vec<LockInfo>),
    Progress(Vec<ProgressInfo>),
    Coordination(Vec<CoordinationInfo>),
}

/// Notification data returned by communication tools query.
/// Note: This is distinct from `agent_context::NotificationSummary` which is for context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryNotification {
    pub id: u64,
    pub source: String,
    pub target: String,
    pub notification_type: String,
    pub details: String,
    pub priority: u32,
}

impl QueryNotification {
    pub fn from_notification(n: &Notification) -> Self {
        let (notification_type, details) = Self::format_notification_type(&n.notification_type);
        Self {
            id: n.id,
            source: n.source.clone(),
            target: n.target.clone(),
            notification_type,
            details,
            priority: n.priority,
        }
    }

    fn format_notification_type(nt: &NotificationType) -> (String, String) {
        match nt {
            NotificationType::TaskAssigned { task_id, agent_id } => (
                "task_assigned".to_string(),
                format!("task={}, agent={}", task_id, agent_id),
            ),
            NotificationType::TaskCompleted {
                task_id,
                agent_id,
                files_modified,
            } => (
                "task_completed".to_string(),
                format!(
                    "task={}, agent={}, files={}",
                    task_id,
                    agent_id,
                    files_modified.join(",")
                ),
            ),
            NotificationType::TaskFailed {
                task_id,
                agent_id,
                error,
            } => (
                "task_failed".to_string(),
                format!("task={}, agent={}, error={}", task_id, agent_id, error),
            ),
            NotificationType::ConflictDetected {
                conflict_id,
                agents,
                description,
                ..
            } => (
                "conflict_detected".to_string(),
                format!(
                    "id={}, agents={}, desc={}",
                    conflict_id,
                    agents.join(","),
                    description
                ),
            ),
            NotificationType::ConflictResolved {
                conflict_id,
                resolution,
            } => (
                "conflict_resolved".to_string(),
                format!("id={}, resolution={}", conflict_id, resolution),
            ),
            NotificationType::ProgressUpdate {
                agent_id,
                task_id,
                progress_percent,
                message,
            } => (
                "progress".to_string(),
                format!(
                    "task={}, agent={}, {}%: {}",
                    task_id, agent_id, progress_percent, message
                ),
            ),
            NotificationType::LockAcquired { resource, holder } => (
                "lock_acquired".to_string(),
                format!("resource={}, holder={}", resource, holder),
            ),
            NotificationType::LockReleased { resource, holder } => (
                "lock_released".to_string(),
                format!("resource={}, holder={}", resource, holder),
            ),
            NotificationType::CoordinationRequest {
                request_id,
                requester,
                coordination_type,
                ..
            } => (
                "coordination".to_string(),
                format!(
                    "id={}, from={}, type={}",
                    request_id, requester, coordination_type
                ),
            ),
            NotificationType::PhaseChanged {
                from_phase,
                to_phase,
            } => (
                "phase_changed".to_string(),
                format!("{} -> {}", from_phase, to_phase),
            ),
            NotificationType::CheckpointCreated {
                checkpoint_id,
                reason,
            } => (
                "checkpoint".to_string(),
                format!("id={}, reason={}", checkpoint_id, reason),
            ),
            NotificationType::Text { message } => ("text".to_string(), message.clone()),
        }
    }
}

/// Lock information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockInfo {
    pub resource: String,
    pub holder: String,
    pub lock_type: String,
}

/// Progress information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressInfo {
    pub agent_id: String,
    pub task_id: String,
    pub progress_percent: u32,
    pub message: String,
    pub updated_at_ms: u64,
}

/// Coordination data information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationInfo {
    pub key: String,
    pub data: serde_json::Value,
    pub owner: String,
    pub updated_at_ms: u64,
}

/// Output for the `acquire_lock` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquireLockOutput {
    /// Whether the lock was acquired.
    pub acquired: bool,
    /// If not acquired, who holds it.
    pub holder: Option<String>,
    /// Message describing the result.
    pub message: String,
}

/// Output for the `coordinate` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinateOutput {
    /// Whether the operation succeeded.
    pub success: bool,
    /// Result data (for get operations).
    pub data: Option<serde_json::Value>,
    /// Counter value (for increment/decrement).
    pub counter_value: Option<i64>,
    /// Message describing the result.
    pub message: String,
}

/// Output for the `save_progress` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveProgressOutput {
    /// Whether progress was saved.
    pub success: bool,
    /// Message describing the result.
    pub message: String,
}

// ============================================================================
// Communication Tools Implementation
// ============================================================================

/// Communication tools for agent interaction with session infrastructure.
///
/// These tools enable stateless agents to interact with session components via
/// query-based patterns. Agents query for notifications and state rather than
/// subscribing to real-time streams, since they don't persist between invocations.
pub struct CommunicationTools {
    notification_log: Arc<RwLock<NotificationLog>>,
    shared_state: Arc<SharedState>,
    participants: Arc<RwLock<ParticipantRegistry>>,
}

impl CommunicationTools {
    /// Create new communication tools with session components.
    pub fn new(
        notification_log: Arc<RwLock<NotificationLog>>,
        shared_state: Arc<SharedState>,
        participants: Arc<RwLock<ParticipantRegistry>>,
    ) -> Self {
        Self {
            notification_log,
            shared_state,
            participants,
        }
    }

    /// Execute the `notify` tool.
    ///
    /// Notifications are stored in NotificationLog for query-based retrieval.
    /// Stateless agents query their notifications via the `query` tool; they don't
    /// subscribe to real-time message streams between invocations.
    pub fn notify(&self, agent_id: &AgentId, input: NotifyInput) -> ToolResult<NotifyOutput> {
        let notification_type = input
            .notification_type
            .to_notification_type(agent_id.as_str());
        let priority = input
            .priority
            .map(Priority::from)
            .unwrap_or_else(|| input.notification_type.default_priority());

        // Add to notification log for query-based retrieval
        let notification_id = {
            let mut log = self.notification_log.write();
            log.add_with_priority(
                agent_id.as_str(),
                &input.target,
                notification_type,
                priority.value(),
            )
        };

        Ok(NotifyOutput {
            success: true,
            notification_id,
        })
    }

    /// Execute the `query` tool.
    pub fn query(&self, agent_id: &AgentId, input: QueryInput) -> ToolResult<QueryOutput> {
        match input.query_type {
            QueryType::Notifications => {
                self.query_notifications(agent_id, &input.filters, input.limit)
            }
            QueryType::Participants => self.query_participants(&input.filters, input.limit),
            QueryType::Locks => self.query_locks(&input.filters, input.limit),
            QueryType::Progress => self.query_progress(&input.filters, input.limit),
            QueryType::Coordination => self.query_coordination(&input.filters, input.limit),
        }
    }

    fn query_notifications(
        &self,
        agent_id: &AgentId,
        filters: &QueryFilters,
        limit: usize,
    ) -> ToolResult<QueryOutput> {
        let log = self.notification_log.read();

        let mut filter = NotificationFilter::new().for_agent(agent_id.as_str());

        if filters.task_related_only {
            filter = filter.task_related_only();
        }

        if filters.conflict_related_only {
            filter = filter.conflict_related_only();
        }

        if let Some(ref tasks) = filters.task_id {
            filter = filter.related_to_tasks(vec![tasks.clone()]);
        }

        if filters.exclude_acknowledged {
            filter = filter.exclude_acknowledged();
        }

        filter = filter.limit(limit);

        let notifications = log.query(&filter);
        let total_count = notifications.len();
        let truncated = total_count > limit;

        let summaries: Vec<QueryNotification> = notifications
            .into_iter()
            .take(limit)
            .map(QueryNotification::from_notification)
            .collect();

        Ok(QueryOutput {
            results: QueryResults::Notifications(summaries),
            total_count,
            truncated,
        })
    }

    fn query_participants(&self, filters: &QueryFilters, limit: usize) -> ToolResult<QueryOutput> {
        let registry = self.participants.read();

        let participants: Vec<&super::participant::Participant> =
            if let Some(ref module) = filters.module {
                registry.by_module(module)
            } else if let Some(ref role) = filters.role {
                registry.by_role(role)
            } else {
                registry.active()
            };

        let total_count = participants.len();
        let truncated = total_count > limit;

        let summaries: Vec<ParticipantSummary> = participants
            .into_iter()
            .take(limit)
            .map(ParticipantSummary::from)
            .collect();

        Ok(QueryOutput {
            results: QueryResults::Participants(summaries),
            total_count,
            truncated,
        })
    }

    fn query_locks(&self, filters: &QueryFilters, limit: usize) -> ToolResult<QueryOutput> {
        let all_locks = self.shared_state.all_locks();
        let total_count = all_locks.len();
        let truncated = total_count > limit;

        let locks: Vec<LockInfo> = all_locks
            .into_iter()
            .filter(|(_, holder)| {
                filters
                    .agent_id
                    .as_ref()
                    .map(|id| holder.as_str() == id)
                    .unwrap_or(true)
            })
            .take(limit)
            .map(|(resource, holder)| LockInfo {
                resource,
                holder: holder.as_str().to_string(),
                lock_type: "exclusive".to_string(),
            })
            .collect();

        Ok(QueryOutput {
            results: QueryResults::Locks(locks),
            total_count,
            truncated,
        })
    }

    fn query_progress(&self, filters: &QueryFilters, limit: usize) -> ToolResult<QueryOutput> {
        let progress = if let Some(ref agent_id) = filters.agent_id {
            self.shared_state.agent_progress(agent_id)
        } else if let Some(ref task_id) = filters.task_id {
            self.shared_state.task_progress(task_id)
        } else {
            let snapshot = self.shared_state.snapshot();
            snapshot.progress.into_values().collect()
        };

        let total_count = progress.len();
        let truncated = total_count > limit;

        let infos: Vec<ProgressInfo> = progress
            .into_iter()
            .take(limit)
            .map(|p| ProgressInfo {
                agent_id: p.agent_id,
                task_id: p.task_id,
                progress_percent: p.progress_percent,
                message: p.message,
                updated_at_ms: p.updated_at_ms,
            })
            .collect();

        Ok(QueryOutput {
            results: QueryResults::Progress(infos),
            total_count,
            truncated,
        })
    }

    fn query_coordination(&self, filters: &QueryFilters, limit: usize) -> ToolResult<QueryOutput> {
        let keys = self.shared_state.coordination_keys();
        let total_count = keys.len();

        let infos: Vec<CoordinationInfo> = keys
            .into_iter()
            .filter(|k| {
                filters
                    .coordination_key
                    .as_ref()
                    .map(|filter_key| k.contains(filter_key))
                    .unwrap_or(true)
            })
            .take(limit)
            .filter_map(|key| {
                self.shared_state
                    .get_coordination_entry(&key)
                    .map(|entry| CoordinationInfo {
                        key: entry.key,
                        data: entry.data,
                        owner: entry.owner,
                        updated_at_ms: entry.updated_at_ms,
                    })
            })
            .collect();

        let truncated = total_count > limit;

        Ok(QueryOutput {
            results: QueryResults::Coordination(infos),
            total_count,
            truncated,
        })
    }

    /// Execute the `acquire_lock` tool.
    pub fn acquire_lock(
        &self,
        agent_id: &AgentId,
        input: AcquireLockInput,
    ) -> ToolResult<AcquireLockOutput> {
        let lock_type: LockType = input.lock_type.into();
        let duration = input.duration_secs.map(std::time::Duration::from_secs);

        let result = self
            .shared_state
            .acquire_lock(&input.resource, agent_id, lock_type, duration);

        match result {
            LockResult::Acquired => Ok(AcquireLockOutput {
                acquired: true,
                holder: None,
                message: format!("Lock acquired on '{}'", input.resource),
            }),
            LockResult::AlreadyHeld => Ok(AcquireLockOutput {
                acquired: true,
                holder: Some(agent_id.as_str().to_string()),
                message: format!("Lock already held on '{}'", input.resource),
            }),
            LockResult::Denied { holder } => Ok(AcquireLockOutput {
                acquired: false,
                holder: Some(holder.as_str().to_string()),
                message: format!(
                    "Lock denied on '{}', held by '{}'",
                    input.resource,
                    holder.as_str()
                ),
            }),
        }
    }

    /// Release a lock (companion to acquire_lock).
    pub fn release_lock(&self, agent_id: &AgentId, resource: &str) -> bool {
        self.shared_state.release_lock(resource, agent_id)
    }

    /// Execute the `coordinate` tool.
    pub fn coordinate(
        &self,
        agent_id: &AgentId,
        input: CoordinateInput,
    ) -> ToolResult<CoordinateOutput> {
        match input.action {
            CoordinationAction::Get => {
                let data: Option<serde_json::Value> =
                    self.shared_state.get_coordination(&input.key);
                let found = data.is_some();
                Ok(CoordinateOutput {
                    success: found,
                    data,
                    counter_value: None,
                    message: if found {
                        format!("Retrieved coordination data for '{}'", input.key)
                    } else {
                        format!("No coordination data found for '{}'", input.key)
                    },
                })
            }
            CoordinationAction::Set => {
                let data = input.data.ok_or_else(|| ToolError::InvalidParams {
                    message: "data is required for set action".to_string(),
                })?;
                self.shared_state
                    .set_coordination(&input.key, &data, agent_id.as_str())
                    .map_err(|e| ToolError::SerializationError {
                        message: e.to_string(),
                    })?;
                Ok(CoordinateOutput {
                    success: true,
                    data: Some(data),
                    counter_value: None,
                    message: format!("Set coordination data for '{}'", input.key),
                })
            }
            CoordinationAction::Delete => {
                let removed = self.shared_state.remove_coordination(&input.key);
                Ok(CoordinateOutput {
                    success: removed,
                    data: None,
                    counter_value: None,
                    message: if removed {
                        format!("Removed coordination data for '{}'", input.key)
                    } else {
                        format!("No coordination data to remove for '{}'", input.key)
                    },
                })
            }
            CoordinationAction::Increment => {
                let value = self.shared_state.increment(&input.key);
                Ok(CoordinateOutput {
                    success: true,
                    data: None,
                    counter_value: Some(value),
                    message: format!("Incremented counter '{}' to {}", input.key, value),
                })
            }
            CoordinationAction::Decrement => {
                let value = self.shared_state.decrement(&input.key);
                Ok(CoordinateOutput {
                    success: true,
                    data: None,
                    counter_value: Some(value),
                    message: format!("Decremented counter '{}' to {}", input.key, value),
                })
            }
        }
    }

    /// Execute the `save_progress` tool.
    pub fn save_progress(
        &self,
        agent_id: &AgentId,
        input: SaveProgressInput,
    ) -> ToolResult<SaveProgressOutput> {
        if input.progress_percent > 100 {
            return Err(ToolError::InvalidParams {
                message: "progress_percent must be between 0 and 100".to_string(),
            });
        }

        self.shared_state.update_progress(
            agent_id.as_str(),
            &input.task_id,
            input.progress_percent,
            &input.message,
        );

        if !input.artifacts.is_empty() {
            let key = format!("artifacts:{}:{}", agent_id.as_str(), input.task_id);
            let _ = self
                .shared_state
                .set_coordination(&key, &input.artifacts, agent_id.as_str());
        }

        // Send progress notification
        let _ = self.notify(
            agent_id,
            NotifyInput {
                target: "*".to_string(),
                notification_type: NotificationTypeInput::Progress {
                    task_id: input.task_id.clone(),
                    progress_percent: input.progress_percent,
                    message: input.message.clone(),
                },
                priority: Some(PriorityInput::Low),
            },
        );

        Ok(SaveProgressOutput {
            success: true,
            message: format!(
                "Saved progress {}% for task '{}'",
                input.progress_percent, input.task_id
            ),
        })
    }

    /// Acknowledge notifications (helper for agents after processing).
    pub fn acknowledge_notifications(&self, agent_id: &str, notification_ids: &[u64]) {
        let mut log = self.notification_log.write();
        for id in notification_ids {
            log.acknowledge(*id, agent_id);
        }
    }
}

// ============================================================================
// Tool Definitions for LLM
// ============================================================================

/// Tool definition for LLM tool calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Get all communication tool definitions for LLM.
pub fn get_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "notify".to_string(),
            description: "Send a notification to other agents. Use target='*' for broadcast, \
                         or a specific agent ID for direct message."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Target agent(s): '*' for broadcast or specific agent ID"
                    },
                    "notification_type": {
                        "type": "object",
                        "description": "Notification type with data",
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "text" },
                                    "message": { "type": "string" }
                                },
                                "required": ["type", "message"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "task_completed" },
                                    "task_id": { "type": "string" },
                                    "files_modified": { "type": "array", "items": { "type": "string" } }
                                },
                                "required": ["type", "task_id", "files_modified"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "task_failed" },
                                    "task_id": { "type": "string" },
                                    "error": { "type": "string" }
                                },
                                "required": ["type", "task_id", "error"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "progress" },
                                    "task_id": { "type": "string" },
                                    "progress_percent": { "type": "integer" },
                                    "message": { "type": "string" }
                                },
                                "required": ["type", "task_id", "progress_percent", "message"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "conflict" },
                                    "conflict_id": { "type": "string" },
                                    "agents": { "type": "array", "items": { "type": "string" } },
                                    "files": { "type": "array", "items": { "type": "string" } },
                                    "description": { "type": "string" }
                                },
                                "required": ["type", "conflict_id", "agents", "files", "description"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "type": { "const": "coordination" },
                                    "request_id": { "type": "string" },
                                    "coordination_type": { "type": "string" },
                                    "data": { "type": "string" }
                                },
                                "required": ["type", "request_id", "coordination_type", "data"]
                            }
                        ]
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["critical", "high", "normal", "low", "background"],
                        "description": "Message priority (default: based on notification type)"
                    }
                },
                "required": ["target", "notification_type"]
            }),
        },
        ToolDefinition {
            name: "query".to_string(),
            description:
                "Query notifications, participants, locks, progress, or coordination data."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query_type": {
                        "type": "string",
                        "enum": ["notifications", "participants", "locks", "progress", "coordination"],
                        "description": "Type of data to query"
                    },
                    "filters": {
                        "type": "object",
                        "properties": {
                            "agent_id": { "type": "string", "description": "Filter by agent ID" },
                            "module": { "type": "string", "description": "Filter by module" },
                            "role": { "type": "string", "description": "Filter by role" },
                            "task_id": { "type": "string", "description": "Filter by task ID" },
                            "task_related_only": { "type": "boolean", "description": "Only task-related notifications" },
                            "conflict_related_only": { "type": "boolean", "description": "Only conflict-related notifications" },
                            "exclude_acknowledged": { "type": "boolean", "description": "Exclude acknowledged notifications" },
                            "coordination_key": { "type": "string", "description": "Filter coordination by key pattern" }
                        }
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results to return (default: 50)"
                    }
                },
                "required": ["query_type"]
            }),
        },
        ToolDefinition {
            name: "acquire_lock".to_string(),
            description: "Acquire a lock on a resource. Returns immediately with success/failure. \
                         No blocking - if lock is held by another agent, returns denied status."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "resource": {
                        "type": "string",
                        "description": "Resource identifier to lock (e.g., file path, section name)"
                    },
                    "lock_type": {
                        "type": "string",
                        "enum": ["exclusive", "shared"],
                        "description": "Type of lock (default: exclusive)"
                    },
                    "duration_secs": {
                        "type": "integer",
                        "description": "Lock duration in seconds (default: 300)"
                    }
                },
                "required": ["resource"]
            }),
        },
        ToolDefinition {
            name: "coordinate".to_string(),
            description: "Coordinate with other agents by getting/setting shared data or counters."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["get", "set", "delete", "increment", "decrement"],
                        "description": "Coordination action"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key for the coordination data"
                    },
                    "data": {
                        "description": "Data payload (required for 'set' action)"
                    }
                },
                "required": ["action", "key"]
            }),
        },
        ToolDefinition {
            name: "save_progress".to_string(),
            description: "Save progress on current task. Automatically notifies other agents."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task ID"
                    },
                    "progress_percent": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 100,
                        "description": "Progress percentage (0-100)"
                    },
                    "message": {
                        "type": "string",
                        "description": "Status message"
                    },
                    "artifacts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of artifact paths produced"
                    }
                },
                "required": ["task_id", "progress_percent", "message"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn create_test_tools() -> CommunicationTools {
        CommunicationTools::new(
            Arc::new(RwLock::new(NotificationLog::new(
                1000,
                Duration::from_secs(3600),
            ))),
            Arc::new(SharedState::new()),
            Arc::new(RwLock::new(ParticipantRegistry::new())),
        )
    }

    #[test]
    fn test_notify_broadcast() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        let result = tools.notify(
            &agent,
            NotifyInput {
                target: "*".to_string(),
                notification_type: NotificationTypeInput::Text {
                    message: "Hello all".to_string(),
                },
                priority: None,
            },
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success);
        assert!(output.notification_id > 0);
    }

    #[test]
    fn test_query_notifications() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Add a notification first
        tools
            .notify(
                &agent,
                NotifyInput {
                    target: "*".to_string(),
                    notification_type: NotificationTypeInput::TaskCompleted {
                        task_id: "task-1".to_string(),
                        files_modified: vec!["file.rs".to_string()],
                    },
                    priority: None,
                },
            )
            .unwrap();

        // Query notifications
        let result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Notifications,
                filters: QueryFilters {
                    task_related_only: true,
                    ..Default::default()
                },
                limit: 10,
            },
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.total_count >= 1);
    }

    #[test]
    fn test_acquire_lock() {
        let tools = create_test_tools();
        let agent1 = AgentId::new("agent-1");
        let agent2 = AgentId::new("agent-2");

        // First agent acquires
        let result = tools.acquire_lock(
            &agent1,
            AcquireLockInput {
                resource: "file.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: None,
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().acquired);

        // Second agent tries - should be denied
        let result = tools.acquire_lock(
            &agent2,
            AcquireLockInput {
                resource: "file.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: None,
            },
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.acquired);
        assert_eq!(output.holder, Some("agent-1".to_string()));

        // Release and retry
        tools.release_lock(&agent1, "file.rs");

        let result = tools.acquire_lock(
            &agent2,
            AcquireLockInput {
                resource: "file.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: None,
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().acquired);
    }

    #[test]
    fn test_coordinate_counter() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Increment
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Increment,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().counter_value, Some(1));

        // Increment again
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Increment,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert_eq!(result.unwrap().counter_value, Some(2));

        // Decrement
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Decrement,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert_eq!(result.unwrap().counter_value, Some(1));
    }

    #[test]
    fn test_coordinate_data() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Set data
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Set,
                key: "config".to_string(),
                data: Some(serde_json::json!({"max_retries": 3})),
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Get data
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Get,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success);
        assert_eq!(output.data, Some(serde_json::json!({"max_retries": 3})));

        // Delete
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Delete,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Get again - should be empty
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Get,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        assert!(!result.unwrap().success);
    }

    #[test]
    fn test_save_progress() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        let result = tools.save_progress(
            &agent,
            SaveProgressInput {
                task_id: "task-1".to_string(),
                progress_percent: 50,
                message: "Halfway done".to_string(),
                artifacts: vec!["output.json".to_string()],
            },
        );

        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Verify progress was saved
        let progress = tools.shared_state.get_progress("agent-1", "task-1");
        assert!(progress.is_some());
        assert_eq!(progress.unwrap().progress_percent, 50);
    }

    #[test]
    fn test_save_progress_invalid_percent() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        let result = tools.save_progress(
            &agent,
            SaveProgressInput {
                task_id: "task-1".to_string(),
                progress_percent: 150, // Invalid
                message: "Invalid".to_string(),
                artifacts: vec![],
            },
        );

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ToolError::InvalidParams { .. }
        ));
    }

    #[test]
    fn test_tool_definitions() {
        let definitions = get_tool_definitions();
        assert_eq!(definitions.len(), 5);

        let names: Vec<&str> = definitions.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"notify"));
        assert!(names.contains(&"query"));
        assert!(names.contains(&"acquire_lock"));
        assert!(names.contains(&"coordinate"));
        assert!(names.contains(&"save_progress"));
    }

    #[test]
    fn test_query_locks() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Acquire some locks
        tools
            .acquire_lock(
                &agent,
                AcquireLockInput {
                    resource: "file1.rs".to_string(),
                    lock_type: LockTypeInput::Exclusive,
                    duration_secs: None,
                },
            )
            .unwrap();

        tools
            .acquire_lock(
                &agent,
                AcquireLockInput {
                    resource: "file2.rs".to_string(),
                    lock_type: LockTypeInput::Exclusive,
                    duration_secs: None,
                },
            )
            .unwrap();

        // Query all locks
        let result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Locks,
                filters: QueryFilters::default(),
                limit: 50,
            },
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.total_count, 2);

        if let QueryResults::Locks(locks) = output.results {
            assert_eq!(locks.len(), 2);
        } else {
            panic!("Expected Locks results");
        }
    }

    #[test]
    fn test_acknowledge_notifications() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Add notification
        let output = tools
            .notify(
                &agent,
                NotifyInput {
                    target: "*".to_string(),
                    notification_type: NotificationTypeInput::Text {
                        message: "Test".to_string(),
                    },
                    priority: None,
                },
            )
            .unwrap();

        // Acknowledge
        tools.acknowledge_notifications(agent.as_str(), &[output.notification_id]);

        // Query - check acknowledged status
        let result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Notifications,
                filters: QueryFilters {
                    exclude_acknowledged: true,
                    ..Default::default()
                },
                limit: 50,
            },
        );

        // All should be filtered out as acknowledged
        if let QueryResults::Notifications(notifs) = result.unwrap().results {
            assert!(notifs.is_empty());
        }
    }
}
