//! Stateless Agent Invocation Pattern.
//!
//! Provides the infrastructure for invoking agents in a completely stateless manner
//! where each invocation is fresh with no memory of previous calls. All necessary
//! context is injected via AgentContext, and results are captured for orchestration.
//!
//! # Pattern Overview
//!
//! 1. **Prepare**: Build AgentContext with all necessary information
//! 2. **Invoke**: Call agent with context, agent processes and returns result
//! 3. **Capture**: Orchestrator captures result, updates state, schedules follow-ups
//!
//! # Key Principles
//!
//! - Agents have NO persistent state between invocations
//! - All context is explicitly injected via AgentContext
//! - Results explicitly declare actions taken and pending work
//! - Orchestrator is the single source of truth for state

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::agent_context::AgentContext;
use crate::agent::multi::AgentId;

/// Input for agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationInput {
    /// Unique invocation ID.
    pub invocation_id: String,
    /// Agent being invoked.
    pub agent_id: AgentId,
    /// The prepared context for this invocation.
    pub context: AgentContext,
    /// Maximum tokens the agent can use for response.
    pub max_response_tokens: u32,
    /// Timeout for this invocation.
    pub timeout_secs: u32,
    /// Whether this is a retry of a previous invocation.
    pub is_retry: bool,
    /// Previous invocation ID if this is a retry.
    pub previous_invocation_id: Option<String>,
}

impl InvocationInput {
    pub fn new(invocation_id: String, agent_id: AgentId, context: AgentContext) -> Self {
        Self {
            invocation_id,
            agent_id,
            context,
            max_response_tokens: 4096,
            timeout_secs: 300, // 5 minutes
            is_retry: false,
            previous_invocation_id: None,
        }
    }

    pub fn with_timeout(mut self, secs: u32) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_max_tokens(mut self, tokens: u32) -> Self {
        self.max_response_tokens = tokens;
        self
    }

    pub fn as_retry(mut self, previous_id: &str) -> Self {
        self.is_retry = true;
        self.previous_invocation_id = Some(previous_id.to_string());
        self
    }
}

/// Result of an agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationResult {
    /// The invocation ID this result corresponds to.
    pub invocation_id: String,
    /// Agent that was invoked.
    pub agent_id: AgentId,
    /// Overall outcome.
    pub outcome: InvocationOutcome,
    /// Actions the agent performed.
    pub actions: Vec<AgentAction>,
    /// Files modified by the agent.
    pub files_modified: Vec<String>,
    /// Files created by the agent.
    pub files_created: Vec<String>,
    /// Any follow-up work needed.
    pub follow_ups: Vec<FollowUp>,
    /// Messages/notifications to send.
    pub notifications: Vec<NotificationRequest>,
    /// Coordination requests.
    pub coordination_requests: Vec<CoordinationRequest>,
    /// Token usage for this invocation.
    pub token_usage: TokenUsage,
    /// Duration of the invocation.
    pub duration: Duration,
    /// Raw output from the agent (for debugging/logging).
    pub raw_output: Option<String>,
    /// Error details if outcome is Error.
    pub error: Option<InvocationError>,
}

impl InvocationResult {
    fn base(
        invocation_id: String,
        agent_id: AgentId,
        outcome: InvocationOutcome,
        duration: Duration,
    ) -> Self {
        Self {
            invocation_id,
            agent_id,
            outcome,
            duration,
            actions: Vec::new(),
            files_modified: Vec::new(),
            files_created: Vec::new(),
            follow_ups: Vec::new(),
            notifications: Vec::new(),
            coordination_requests: Vec::new(),
            token_usage: TokenUsage::default(),
            raw_output: None,
            error: None,
        }
    }

    pub fn success(invocation_id: String, agent_id: AgentId, duration: Duration) -> Self {
        Self::base(
            invocation_id,
            agent_id,
            InvocationOutcome::Completed,
            duration,
        )
    }

    pub fn error(
        invocation_id: String,
        agent_id: AgentId,
        error: InvocationError,
        duration: Duration,
    ) -> Self {
        let mut result = Self::base(invocation_id, agent_id, InvocationOutcome::Error, duration);
        result.error = Some(error);
        result
    }

    pub fn pending(
        invocation_id: String,
        agent_id: AgentId,
        follow_ups: Vec<FollowUp>,
        duration: Duration,
    ) -> Self {
        let mut result = Self::base(
            invocation_id,
            agent_id,
            InvocationOutcome::Pending,
            duration,
        );
        result.follow_ups = follow_ups;
        result
    }

    pub fn with_actions(mut self, actions: Vec<AgentAction>) -> Self {
        self.actions = actions;
        self
    }

    pub fn with_files(mut self, modified: Vec<String>, created: Vec<String>) -> Self {
        self.files_modified = modified;
        self.files_created = created;
        self
    }

    pub fn with_token_usage(mut self, usage: TokenUsage) -> Self {
        self.token_usage = usage;
        self
    }

    pub fn with_raw_output(mut self, output: String) -> Self {
        self.raw_output = Some(output);
        self
    }

    pub fn add_notification(&mut self, notification: NotificationRequest) {
        self.notifications.push(notification);
    }

    pub fn add_follow_up(&mut self, follow_up: FollowUp) {
        self.follow_ups.push(follow_up);
    }

    pub fn is_success(&self) -> bool {
        matches!(self.outcome, InvocationOutcome::Completed)
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.outcome, InvocationOutcome::Pending)
    }

    pub fn is_error(&self) -> bool {
        matches!(self.outcome, InvocationOutcome::Error)
    }

    pub fn needs_follow_up(&self) -> bool {
        !self.follow_ups.is_empty()
    }
}

/// Outcome of an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvocationOutcome {
    /// Agent completed the task successfully.
    Completed,
    /// Agent made progress but work is pending (needs follow-up).
    Pending,
    /// Agent encountered an error.
    Error,
    /// Agent was blocked (waiting for locks, coordination, etc.).
    Blocked,
    /// Invocation timed out.
    Timeout,
    /// Invocation was cancelled.
    Cancelled,
}

/// An action performed by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    /// Action type.
    pub action_type: ActionType,
    /// Target of the action (file path, resource ID, etc.).
    pub target: String,
    /// Description of what was done.
    pub description: String,
    /// Whether the action succeeded.
    pub success: bool,
}

/// Types of actions an agent can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    /// Read a file.
    FileRead,
    /// Write/modify a file.
    FileWrite,
    /// Create a new file.
    FileCreate,
    /// Delete a file.
    FileDelete,
    /// Run a command.
    CommandRun,
    /// Acquire a lock.
    LockAcquire,
    /// Release a lock.
    LockRelease,
    /// Send a notification.
    Notify,
    /// Request coordination.
    Coordinate,
    /// Other action.
    Other,
}

/// Follow-up work needed after this invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FollowUp {
    /// Type of follow-up.
    pub follow_up_type: FollowUpType,
    /// Description of what needs to be done.
    pub description: String,
    /// Priority (higher = more urgent).
    pub priority: u32,
    /// Suggested delay before follow-up (e.g., wait for lock).
    pub delay_secs: Option<u32>,
    /// Context to pass to the follow-up invocation.
    pub context_data: HashMap<String, String>,
    /// Dependencies that must complete first.
    pub depends_on: Vec<String>,
}

impl FollowUp {
    pub fn continuation(description: &str) -> Self {
        Self {
            follow_up_type: FollowUpType::Continuation,
            description: description.to_string(),
            priority: 50,
            delay_secs: None,
            context_data: HashMap::new(),
            depends_on: Vec::new(),
        }
    }

    pub fn retry(description: &str, delay_secs: u32) -> Self {
        Self {
            follow_up_type: FollowUpType::Retry,
            description: description.to_string(),
            priority: 75,
            delay_secs: Some(delay_secs),
            context_data: HashMap::new(),
            depends_on: Vec::new(),
        }
    }

    pub fn wait_for_lock(resource: &str) -> Self {
        Self {
            follow_up_type: FollowUpType::WaitForLock,
            description: format!("Waiting for lock on '{}'", resource),
            priority: 60,
            delay_secs: Some(5),
            context_data: HashMap::from([("resource".to_string(), resource.to_string())]),
            depends_on: Vec::new(),
        }
    }

    pub fn wait_for_dependency(task_id: &str) -> Self {
        Self {
            follow_up_type: FollowUpType::WaitForDependency,
            description: format!("Waiting for task '{}'", task_id),
            priority: 40,
            delay_secs: None,
            context_data: HashMap::new(),
            depends_on: vec![task_id.to_string()],
        }
    }

    pub fn with_context(mut self, key: &str, value: &str) -> Self {
        self.context_data.insert(key.to_string(), value.to_string());
        self
    }
}

/// Type of follow-up work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowUpType {
    /// Continue work from where we left off.
    Continuation,
    /// Retry the same operation.
    Retry,
    /// Wait for a lock to become available.
    WaitForLock,
    /// Wait for a dependency to complete.
    WaitForDependency,
    /// Wait for coordination response.
    WaitForCoordination,
    /// Verification pass needed.
    Verification,
    /// Cleanup needed.
    Cleanup,
}

/// Request to send a notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationRequest {
    /// Target agent(s).
    pub target: String,
    /// Notification content.
    pub content: String,
    /// Priority.
    pub priority: u32,
}

impl NotificationRequest {
    pub fn broadcast(content: &str) -> Self {
        Self {
            target: "*".to_string(),
            content: content.to_string(),
            priority: 50,
        }
    }

    pub fn direct(target: &str, content: &str) -> Self {
        Self {
            target: target.to_string(),
            content: content.to_string(),
            priority: 50,
        }
    }

    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }
}

/// Request for coordination with other agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationRequest {
    /// Type of coordination.
    pub coordination_type: String,
    /// Agents to coordinate with.
    pub with_agents: Vec<String>,
    /// Data for coordination.
    pub data: String,
}

/// Error during invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationError {
    /// Error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Whether the error is retryable.
    pub retryable: bool,
    /// Suggested retry delay in seconds.
    pub retry_delay_secs: Option<u32>,
}

impl InvocationError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            retryable: false,
            retry_delay_secs: None,
        }
    }

    pub fn retryable(code: &str, message: &str, delay_secs: u32) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
            retryable: true,
            retry_delay_secs: Some(delay_secs),
        }
    }

    pub fn timeout() -> Self {
        Self::retryable("TIMEOUT", "Invocation timed out", 60)
    }

    pub fn rate_limited(delay_secs: u32) -> Self {
        Self::retryable("RATE_LIMITED", "API rate limit exceeded", delay_secs)
    }

    pub fn context_overflow() -> Self {
        Self::new("CONTEXT_OVERFLOW", "Context exceeds token budget")
    }
}

/// Token usage for an invocation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens (context).
    pub input_tokens: u32,
    /// Output tokens (response).
    pub output_tokens: u32,
    /// Total tokens.
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn new(input: u32, output: u32) -> Self {
        Self {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
        }
    }
}

/// Tracker for invocation history.
#[derive(Debug, Default)]
pub struct InvocationTracker {
    history: Vec<InvocationRecord>,
    by_agent: HashMap<String, Vec<usize>>,
    by_task: HashMap<String, Vec<usize>>,
    total_tokens: u64,
    total_duration: Duration,
}

/// Record of a single invocation.
#[derive(Debug, Clone)]
pub struct InvocationRecord {
    pub invocation_id: String,
    pub agent_id: AgentId,
    pub task_id: Option<String>,
    pub outcome: InvocationOutcome,
    pub token_usage: TokenUsage,
    pub duration: Duration,
    pub timestamp: Instant,
}

impl InvocationTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an invocation result.
    pub fn record(&mut self, result: &InvocationResult, task_id: Option<&str>) {
        let idx = self.history.len();
        let record = InvocationRecord {
            invocation_id: result.invocation_id.clone(),
            agent_id: result.agent_id.clone(),
            task_id: task_id.map(String::from),
            outcome: result.outcome,
            token_usage: result.token_usage.clone(),
            duration: result.duration,
            timestamp: Instant::now(),
        };

        // Index by agent
        self.by_agent
            .entry(result.agent_id.as_str().to_string())
            .or_default()
            .push(idx);

        // Index by task
        if let Some(tid) = task_id {
            self.by_task.entry(tid.to_string()).or_default().push(idx);
        }

        // Update totals
        self.total_tokens += result.token_usage.total_tokens as u64;
        self.total_duration += result.duration;

        self.history.push(record);
    }

    /// Get invocations for an agent.
    pub fn for_agent(&self, agent_id: &str) -> Vec<&InvocationRecord> {
        self.by_agent
            .get(agent_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&i| self.history.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get invocations for a task.
    pub fn for_task(&self, task_id: &str) -> Vec<&InvocationRecord> {
        self.by_task
            .get(task_id)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|&i| self.history.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get total invocation count.
    pub fn count(&self) -> usize {
        self.history.len()
    }

    /// Get total tokens used.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Get total duration.
    pub fn total_duration(&self) -> Duration {
        self.total_duration
    }

    /// Get success rate.
    pub fn success_rate(&self) -> f32 {
        if self.history.is_empty() {
            return 0.0;
        }
        let successes = self
            .history
            .iter()
            .filter(|r| r.outcome == InvocationOutcome::Completed)
            .count();
        successes as f32 / self.history.len() as f32
    }

    /// Clear all tracked invocations.
    pub fn clear(&mut self) {
        self.history.clear();
        self.by_agent.clear();
        self.by_task.clear();
        self.total_tokens = 0;
        self.total_duration = Duration::ZERO;
    }

    /// Get statistics.
    pub fn stats(&self) -> InvocationStats {
        let mut by_outcome = HashMap::new();
        for record in &self.history {
            *by_outcome.entry(record.outcome).or_insert(0) += 1;
        }

        InvocationStats {
            total_invocations: self.history.len(),
            total_tokens: self.total_tokens,
            total_duration: self.total_duration,
            by_outcome,
            avg_tokens_per_invocation: if self.history.is_empty() {
                0
            } else {
                (self.total_tokens / self.history.len() as u64) as u32
            },
            avg_duration: if self.history.is_empty() {
                Duration::ZERO
            } else {
                self.total_duration / self.history.len() as u32
            },
        }
    }
}

/// Statistics about invocations.
#[derive(Debug, Clone)]
pub struct InvocationStats {
    pub total_invocations: usize,
    pub total_tokens: u64,
    pub total_duration: Duration,
    pub by_outcome: HashMap<InvocationOutcome, usize>,
    pub avg_tokens_per_invocation: u32,
    pub avg_duration: Duration,
}

/// Generate a unique invocation ID.
pub fn generate_invocation_id(agent_id: &AgentId, task_id: Option<&str>) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    match task_id {
        Some(tid) => format!("inv-{}-{}-{}", agent_id.as_str(), tid, timestamp),
        None => format!("inv-{}-{}", agent_id.as_str(), timestamp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invocation_input() {
        let agent_id = AgentId::new("test-agent");
        let context = AgentContext::new(agent_id.as_str(), "test-session");

        let input = InvocationInput::new("inv-1".to_string(), agent_id.clone(), context)
            .with_timeout(600)
            .with_max_tokens(8192);

        assert_eq!(input.timeout_secs, 600);
        assert_eq!(input.max_response_tokens, 8192);
        assert!(!input.is_retry);
    }

    #[test]
    fn test_invocation_result_success() {
        let agent_id = AgentId::new("test-agent");
        let result =
            InvocationResult::success("inv-1".to_string(), agent_id, Duration::from_secs(10))
                .with_files(vec!["auth.rs".to_string()], vec!["new.rs".to_string()])
                .with_token_usage(TokenUsage::new(5000, 1000));

        assert!(result.is_success());
        assert!(!result.needs_follow_up());
        assert_eq!(result.files_modified.len(), 1);
        assert_eq!(result.files_created.len(), 1);
        assert_eq!(result.token_usage.total_tokens, 6000);
    }

    #[test]
    fn test_invocation_result_pending() {
        let agent_id = AgentId::new("test-agent");
        let follow_ups = vec![
            FollowUp::continuation("Continue implementation"),
            FollowUp::wait_for_lock("config.rs"),
        ];

        let result = InvocationResult::pending(
            "inv-1".to_string(),
            agent_id,
            follow_ups,
            Duration::from_secs(5),
        );

        assert!(result.is_pending());
        assert!(result.needs_follow_up());
        assert_eq!(result.follow_ups.len(), 2);
    }

    #[test]
    fn test_invocation_result_error() {
        let agent_id = AgentId::new("test-agent");
        let error = InvocationError::timeout();

        let result = InvocationResult::error(
            "inv-1".to_string(),
            agent_id,
            error,
            Duration::from_secs(300),
        );

        assert!(result.is_error());
        assert!(result.error.as_ref().unwrap().retryable);
    }

    #[test]
    fn test_follow_up_types() {
        let continuation = FollowUp::continuation("Continue work");
        assert_eq!(continuation.follow_up_type, FollowUpType::Continuation);

        let retry = FollowUp::retry("Retry after rate limit", 60);
        assert_eq!(retry.delay_secs, Some(60));

        let lock_wait = FollowUp::wait_for_lock("file.rs");
        assert!(lock_wait.context_data.contains_key("resource"));

        let dep_wait = FollowUp::wait_for_dependency("task-1");
        assert_eq!(dep_wait.depends_on, vec!["task-1".to_string()]);
    }

    #[test]
    fn test_invocation_tracker() {
        let mut tracker = InvocationTracker::new();
        let agent_id = AgentId::new("test-agent");

        // Record some invocations
        for i in 0..5 {
            let outcome = if i < 4 {
                InvocationOutcome::Completed
            } else {
                InvocationOutcome::Error
            };

            let result = InvocationResult {
                invocation_id: format!("inv-{}", i),
                agent_id: agent_id.clone(),
                outcome,
                actions: Vec::new(),
                files_modified: Vec::new(),
                files_created: Vec::new(),
                follow_ups: Vec::new(),
                notifications: Vec::new(),
                coordination_requests: Vec::new(),
                token_usage: TokenUsage::new(1000, 200),
                duration: Duration::from_secs(10),
                raw_output: None,
                error: None,
            };

            tracker.record(&result, Some(&format!("task-{}", i % 2)));
        }

        assert_eq!(tracker.count(), 5);
        assert_eq!(tracker.total_tokens(), 6000); // 5 * 1200
        assert!((tracker.success_rate() - 0.8).abs() < 0.01);

        let agent_records = tracker.for_agent("test-agent");
        assert_eq!(agent_records.len(), 5);

        let task_records = tracker.for_task("task-0");
        assert_eq!(task_records.len(), 3); // tasks 0, 2, 4
    }

    #[test]
    fn test_generate_invocation_id() {
        let agent_id = AgentId::new("agent-1");

        let id1 = generate_invocation_id(&agent_id, Some("task-1"));
        let id2 = generate_invocation_id(&agent_id, None);

        assert!(id1.starts_with("inv-agent-1-task-1-"));
        assert!(id1 != id2);
    }

    #[test]
    fn test_invocation_stats() {
        let mut tracker = InvocationTracker::new();
        let agent_id = AgentId::new("test-agent");

        for _ in 0..10 {
            let result = InvocationResult::success(
                "inv".to_string(),
                agent_id.clone(),
                Duration::from_secs(5),
            )
            .with_token_usage(TokenUsage::new(1000, 500));

            tracker.record(&result, None);
        }

        let stats = tracker.stats();
        assert_eq!(stats.total_invocations, 10);
        assert_eq!(stats.total_tokens, 15000);
        assert_eq!(stats.avg_tokens_per_invocation, 1500);
        assert_eq!(
            stats.by_outcome.get(&InvocationOutcome::Completed),
            Some(&10)
        );
    }
}
