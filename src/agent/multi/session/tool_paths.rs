//! Synchronous vs Asynchronous tool path separation.
//!
//! Defines clear patterns for how tools behave in a stateless agent context:
//!
//! - **Synchronous**: Tools that always return immediately with a final result
//! - **Deferred**: Tools that return a pending status, requiring follow-up
//!
//! # Design Principles
//!
//! In the Stateless Agent Pattern:
//! - All tool calls return immediately (no blocking waits)
//! - Deferred results are handled through the Task Continuation Pattern
//! - The orchestrator is responsible for scheduling follow-ups

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolPathResult<T> {
    /// Tool completed immediately with a final result.
    Immediate(T),
    /// Tool returned a deferred result that requires follow-up.
    Deferred(DeferredResult<T>),
}

impl<T> ToolPathResult<T> {
    pub fn immediate(value: T) -> Self {
        Self::Immediate(value)
    }

    pub fn deferred(reason: DeferralReason, partial: Option<T>) -> Self {
        Self::Deferred(DeferredResult {
            reason,
            partial_result: partial,
            retry_after: None,
            continuation_token: None,
            context: Default::default(),
        })
    }

    pub fn is_immediate(&self) -> bool {
        matches!(self, Self::Immediate(_))
    }

    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred(_))
    }

    pub fn into_immediate(self) -> Option<T> {
        match self {
            Self::Immediate(v) => Some(v),
            Self::Deferred(_) => None,
        }
    }

    pub fn into_deferred(self) -> Option<DeferredResult<T>> {
        match self {
            Self::Immediate(_) => None,
            Self::Deferred(d) => Some(d),
        }
    }
}

/// Details about a deferred result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredResult<T> {
    /// Why the result was deferred.
    pub reason: DeferralReason,
    /// Any partial result available.
    pub partial_result: Option<T>,
    /// Suggested time to retry.
    pub retry_after: Option<Duration>,
    /// Token for continuing this operation.
    pub continuation_token: Option<String>,
    /// Additional context for the follow-up.
    pub context: DeferralContext,
}

impl<T> DeferredResult<T> {
    pub fn with_retry_after(mut self, duration: Duration) -> Self {
        self.retry_after = Some(duration);
        self
    }

    pub fn with_continuation_token(mut self, token: String) -> Self {
        self.continuation_token = Some(token);
        self
    }

    pub fn with_context(mut self, context: DeferralContext) -> Self {
        self.context = context;
        self
    }
}

/// Reason why a tool result was deferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeferralReason {
    /// Waiting for a lock to become available.
    WaitingForLock { resource: String, holder: String },
    /// Waiting for coordination response from another agent.
    WaitingForCoordination { request_id: String },
    /// Waiting for a dependency to complete.
    WaitingForDependency { task_id: String },
    /// Rate limited, need to retry later.
    RateLimited,
    /// Resource temporarily unavailable.
    ResourceUnavailable { resource: String },
    /// Needs user input or approval.
    NeedsApproval { approval_type: String },
    /// Partial result, more work needed.
    PartialCompletion { progress_percent: u32 },
    /// Custom reason.
    Custom { code: String, message: String },
}

impl DeferralReason {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::WaitingForLock { .. } | Self::RateLimited | Self::ResourceUnavailable { .. }
        )
    }

    pub fn suggested_delay(&self) -> Duration {
        match self {
            Self::WaitingForLock { .. } => Duration::from_secs(5),
            Self::RateLimited => Duration::from_secs(60),
            Self::ResourceUnavailable { .. } => Duration::from_secs(10),
            Self::WaitingForCoordination { .. } => Duration::from_secs(30),
            Self::WaitingForDependency { .. } => Duration::from_secs(0), // Event-driven
            Self::NeedsApproval { .. } => Duration::from_secs(0),        // Event-driven
            Self::PartialCompletion { .. } => Duration::from_secs(0),    // Continue immediately
            Self::Custom { .. } => Duration::from_secs(10),
        }
    }
}

/// Additional context for deferral handling.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeferralContext {
    /// Key-value pairs for custom context.
    pub values: std::collections::HashMap<String, String>,
    /// Notification IDs to watch for.
    pub watch_notifications: Vec<String>,
    /// Task IDs to watch for.
    pub watch_tasks: Vec<String>,
}

impl DeferralContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_value(mut self, key: &str, value: &str) -> Self {
        self.values.insert(key.to_string(), value.to_string());
        self
    }

    pub fn watch_notification(mut self, notification_type: &str) -> Self {
        self.watch_notifications.push(notification_type.to_string());
        self
    }

    pub fn watch_task(mut self, task_id: &str) -> Self {
        self.watch_tasks.push(task_id.to_string());
        self
    }
}

/// Classification of tool behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolBehavior {
    /// Always returns immediately with a final result.
    Synchronous,
    /// May return a deferred result requiring follow-up.
    MayDefer,
}

/// Metadata about a tool's path behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPathInfo {
    /// Tool name.
    pub name: String,
    /// Tool behavior classification.
    pub behavior: ToolBehavior,
    /// Typical latency for synchronous execution.
    pub typical_latency_ms: u32,
    /// Can this tool be safely retried?
    pub idempotent: bool,
    /// Description of when deferral might occur.
    pub deferral_conditions: Vec<String>,
}

impl ToolPathInfo {
    pub fn synchronous(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: ToolBehavior::Synchronous,
            typical_latency_ms: 10,
            idempotent: true,
            deferral_conditions: Vec::new(),
        }
    }

    pub fn may_defer(name: &str) -> Self {
        Self {
            name: name.to_string(),
            behavior: ToolBehavior::MayDefer,
            typical_latency_ms: 100,
            idempotent: false,
            deferral_conditions: Vec::new(),
        }
    }

    pub fn with_latency(mut self, ms: u32) -> Self {
        self.typical_latency_ms = ms;
        self
    }

    pub fn with_idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }

    pub fn with_deferral_condition(mut self, condition: &str) -> Self {
        self.deferral_conditions.push(condition.to_string());
        self
    }
}

/// Get path info for all communication tools.
pub fn communication_tool_paths() -> Vec<ToolPathInfo> {
    vec![
        // Query tools - always synchronous
        ToolPathInfo::synchronous("query")
            .with_latency(5)
            .with_idempotent(true),
        // Notify - synchronous (fire and forget)
        ToolPathInfo::synchronous("notify")
            .with_latency(10)
            .with_idempotent(false),
        // Lock acquisition - synchronous (immediate result, no wait)
        ToolPathInfo::synchronous("acquire_lock")
            .with_latency(5)
            .with_idempotent(true),
        // Coordination - may defer for some operations
        ToolPathInfo::may_defer("coordinate")
            .with_latency(20)
            .with_deferral_condition("Waiting for coordination response")
            .with_deferral_condition("Distributed consensus needed"),
        // Progress saving - synchronous
        ToolPathInfo::synchronous("save_progress")
            .with_latency(10)
            .with_idempotent(true),
    ]
}

/// Handler for processing deferred results.
pub struct DeferralHandler {
    pending_deferrals: Vec<PendingDeferral>,
    max_retries: u32,
}

/// A pending deferral waiting for resolution.
#[derive(Debug, Clone)]
pub struct PendingDeferral {
    pub id: String,
    pub agent_id: String,
    pub tool_name: String,
    pub reason: DeferralReason,
    pub retry_count: u32,
    pub continuation_token: Option<String>,
    pub context: DeferralContext,
    pub created_at: std::time::Instant,
}

impl DeferralHandler {
    pub fn new(max_retries: u32) -> Self {
        Self {
            pending_deferrals: Vec::new(),
            max_retries,
        }
    }

    /// Record a new deferral.
    pub fn record_deferral<T>(
        &mut self,
        id: &str,
        agent_id: &str,
        tool_name: &str,
        deferred: &DeferredResult<T>,
    ) {
        self.pending_deferrals.push(PendingDeferral {
            id: id.to_string(),
            agent_id: agent_id.to_string(),
            tool_name: tool_name.to_string(),
            reason: deferred.reason.clone(),
            retry_count: 0,
            continuation_token: deferred.continuation_token.clone(),
            context: deferred.context.clone(),
            created_at: std::time::Instant::now(),
        });
    }

    /// Get deferrals for an agent.
    pub fn for_agent(&self, agent_id: &str) -> Vec<&PendingDeferral> {
        self.pending_deferrals
            .iter()
            .filter(|d| d.agent_id == agent_id)
            .collect()
    }

    /// Get deferrals ready for retry.
    pub fn ready_for_retry(&self) -> Vec<&PendingDeferral> {
        let now = std::time::Instant::now();
        self.pending_deferrals
            .iter()
            .filter(|d| {
                d.retry_count < self.max_retries
                    && d.reason.is_retryable()
                    && now.duration_since(d.created_at) >= d.reason.suggested_delay()
            })
            .collect()
    }

    /// Mark a deferral as resolved.
    pub fn resolve(&mut self, id: &str) {
        self.pending_deferrals.retain(|d| d.id != id);
    }

    /// Increment retry count for a deferral.
    pub fn increment_retry(&mut self, id: &str) {
        if let Some(d) = self.pending_deferrals.iter_mut().find(|d| d.id == id) {
            d.retry_count += 1;
        }
    }

    /// Get count of pending deferrals.
    pub fn pending_count(&self) -> usize {
        self.pending_deferrals.len()
    }

    /// Clear all deferrals for an agent.
    pub fn clear_for_agent(&mut self, agent_id: &str) {
        self.pending_deferrals.retain(|d| d.agent_id != agent_id);
    }

    /// Clear all deferrals.
    pub fn clear_all(&mut self) {
        self.pending_deferrals.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_path_result_immediate() {
        let result: ToolPathResult<i32> = ToolPathResult::immediate(42);

        assert!(result.is_immediate());
        assert!(!result.is_deferred());
        assert_eq!(result.into_immediate(), Some(42));
    }

    #[test]
    fn test_tool_path_result_deferred() {
        let result: ToolPathResult<String> = ToolPathResult::deferred(
            DeferralReason::WaitingForLock {
                resource: "file.rs".to_string(),
                holder: "agent-2".to_string(),
            },
            None,
        );

        assert!(!result.is_immediate());
        assert!(result.is_deferred());

        let deferred = result.into_deferred().unwrap();
        assert!(deferred.reason.is_retryable());
    }

    #[test]
    fn test_deferral_reason_suggested_delay() {
        assert_eq!(
            DeferralReason::RateLimited.suggested_delay(),
            Duration::from_secs(60)
        );

        assert_eq!(
            DeferralReason::WaitingForLock {
                resource: "x".to_string(),
                holder: "y".to_string(),
            }
            .suggested_delay(),
            Duration::from_secs(5)
        );

        assert_eq!(
            DeferralReason::WaitingForDependency {
                task_id: "task-1".to_string(),
            }
            .suggested_delay(),
            Duration::from_secs(0)
        );
    }

    #[test]
    fn test_tool_path_info() {
        let paths = communication_tool_paths();

        assert_eq!(paths.len(), 5);

        let query = paths.iter().find(|p| p.name == "query").unwrap();
        assert_eq!(query.behavior, ToolBehavior::Synchronous);
        assert!(query.idempotent);

        let coordinate = paths.iter().find(|p| p.name == "coordinate").unwrap();
        assert_eq!(coordinate.behavior, ToolBehavior::MayDefer);
        assert!(!coordinate.deferral_conditions.is_empty());
    }

    #[test]
    fn test_deferral_handler() {
        let mut handler = DeferralHandler::new(3);

        let deferred = DeferredResult::<String> {
            reason: DeferralReason::WaitingForLock {
                resource: "config.rs".to_string(),
                holder: "agent-2".to_string(),
            },
            partial_result: None,
            retry_after: Some(Duration::from_secs(5)),
            continuation_token: Some("token-123".to_string()),
            context: DeferralContext::new(),
        };

        handler.record_deferral("def-1", "agent-1", "acquire_lock", &deferred);

        assert_eq!(handler.pending_count(), 1);

        let agent_deferrals = handler.for_agent("agent-1");
        assert_eq!(agent_deferrals.len(), 1);
        assert_eq!(agent_deferrals[0].tool_name, "acquire_lock");

        handler.resolve("def-1");
        assert_eq!(handler.pending_count(), 0);
    }

    #[test]
    fn test_deferral_context() {
        let context = DeferralContext::new()
            .with_value("key1", "value1")
            .watch_notification("lock_released")
            .watch_task("task-1");

        assert_eq!(context.values.get("key1"), Some(&"value1".to_string()));
        assert!(
            context
                .watch_notifications
                .contains(&"lock_released".to_string())
        );
        assert!(context.watch_tasks.contains(&"task-1".to_string()));
    }

    #[test]
    fn test_retry_tracking() {
        let mut handler = DeferralHandler::new(3);

        let deferred = DeferredResult::<()> {
            reason: DeferralReason::RateLimited,
            partial_result: None,
            retry_after: None,
            continuation_token: None,
            context: DeferralContext::new(),
        };

        handler.record_deferral("def-1", "agent-1", "notify", &deferred);
        handler.increment_retry("def-1");
        handler.increment_retry("def-1");

        let pending = handler.for_agent("agent-1");
        assert_eq!(pending[0].retry_count, 2);
    }
}
