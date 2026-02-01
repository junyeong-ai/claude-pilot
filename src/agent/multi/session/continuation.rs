//! Task Continuation Pattern for async agent coordination.
//!
//! Handles the scenario where an agent can't complete a task in a single
//! invocation and needs to return a "Pending" status with continuation
//! information for the orchestrator to schedule follow-up work.
//!
//! # Pattern Overview
//!
//! 1. Agent invocation returns `Pending` with continuation info
//! 2. Orchestrator stores continuation state
//! 3. On trigger (time, event, dependency), orchestrator schedules follow-up
//! 4. New invocation includes continuation context
//! 5. Agent resumes from checkpoint
//!
//! # Use Cases
//!
//! - Long-running tasks that exceed context budget
//! - Tasks waiting for external dependencies
//! - Tasks blocked on resource locks
//! - Multi-phase tasks (planning -> review -> implementation)

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::invocation::{FollowUp, FollowUpType, InvocationOutcome, InvocationResult};
use crate::agent::multi::AgentId;

/// State of a continuation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationState {
    /// Unique continuation ID.
    pub id: String,
    /// Agent that owns this continuation.
    pub agent_id: AgentId,
    /// Task this continuation is for.
    pub task_id: String,
    /// Original invocation that created this continuation.
    pub origin_invocation_id: String,
    /// Current phase of the continuation.
    pub phase: u32,
    /// Total estimated phases.
    pub total_phases: Option<u32>,
    /// Progress within current phase (0-100).
    pub phase_progress: u32,
    /// What's needed to resume.
    pub resume_requirements: ResumeRequirements,
    /// Checkpoint data for resuming.
    pub checkpoint: ContinuationCheckpoint,
    /// When this continuation was created.
    #[serde(skip)]
    pub created_at: Option<Instant>,
    /// When this was last updated.
    #[serde(skip)]
    pub updated_at: Option<Instant>,
    /// Number of continuation attempts.
    pub attempt_count: u32,
    /// Maximum attempts before abandoning.
    pub max_attempts: u32,
    /// Current status.
    pub status: ContinuationStatus,
}

impl ContinuationState {
    pub fn new(id: String, agent_id: AgentId, task_id: String, invocation_id: String) -> Self {
        Self {
            id,
            agent_id,
            task_id,
            origin_invocation_id: invocation_id,
            phase: 1,
            total_phases: None,
            phase_progress: 0,
            resume_requirements: ResumeRequirements::default(),
            checkpoint: ContinuationCheckpoint::default(),
            created_at: Some(Instant::now()),
            updated_at: Some(Instant::now()),
            attempt_count: 0,
            max_attempts: 10,
            status: ContinuationStatus::Pending,
        }
    }

    pub fn with_phases(mut self, total: u32) -> Self {
        self.total_phases = Some(total);
        self
    }

    pub fn with_checkpoint(mut self, checkpoint: ContinuationCheckpoint) -> Self {
        self.checkpoint = checkpoint;
        self
    }

    pub fn with_requirements(mut self, requirements: ResumeRequirements) -> Self {
        self.resume_requirements = requirements;
        self
    }

    pub fn advance_phase(&mut self) {
        self.phase += 1;
        self.phase_progress = 0;
        self.updated_at = Some(Instant::now());
    }

    pub fn update_progress(&mut self, progress: u32) {
        self.phase_progress = progress.min(100);
        self.updated_at = Some(Instant::now());
    }

    pub fn can_resume(&self) -> bool {
        self.status == ContinuationStatus::Ready && self.attempt_count < self.max_attempts
    }

    pub fn mark_ready(&mut self) {
        self.status = ContinuationStatus::Ready;
        self.updated_at = Some(Instant::now());
    }

    pub fn mark_blocked(&mut self, reason: &str) {
        self.status = ContinuationStatus::Blocked {
            reason: reason.to_string(),
        };
        self.updated_at = Some(Instant::now());
    }

    pub fn mark_completed(&mut self) {
        self.status = ContinuationStatus::Completed;
        self.updated_at = Some(Instant::now());
    }

    pub fn mark_failed(&mut self, error: &str) {
        self.status = ContinuationStatus::Failed {
            error: error.to_string(),
        };
        self.updated_at = Some(Instant::now());
    }

    pub fn increment_attempt(&mut self) {
        self.attempt_count += 1;
        self.updated_at = Some(Instant::now());
    }

    pub fn overall_progress(&self) -> f32 {
        match self.total_phases {
            Some(total) if total > 0 => {
                let phase_weight = 100.0 / total as f32;
                let completed_phases = (self.phase - 1) as f32 * phase_weight;
                let current_phase = self.phase_progress as f32 * phase_weight / 100.0;
                completed_phases + current_phase
            }
            _ => self.phase_progress as f32,
        }
    }

    pub fn age(&self) -> Duration {
        self.created_at
            .map(|t| t.elapsed())
            .unwrap_or(Duration::ZERO)
    }
}

/// Status of a continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContinuationStatus {
    /// Waiting for requirements to be met.
    Pending,
    /// Requirements met, ready to resume.
    Ready,
    /// Blocked on something.
    Blocked { reason: String },
    /// Successfully completed.
    Completed,
    /// Failed to complete.
    Failed { error: String },
    /// Abandoned after max attempts.
    Abandoned,
}

/// Requirements that must be met before resuming.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResumeRequirements {
    /// Minimum delay before resuming.
    pub min_delay: Option<Duration>,
    /// Tasks that must complete first.
    pub depends_on_tasks: Vec<String>,
    /// Locks that must be acquired.
    pub requires_locks: Vec<String>,
    /// Coordination responses needed.
    pub awaiting_coordination: Vec<String>,
    /// Custom conditions.
    pub custom_conditions: Vec<String>,
}

impl ResumeRequirements {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.min_delay = Some(delay);
        self
    }

    pub fn depends_on(mut self, task_id: &str) -> Self {
        self.depends_on_tasks.push(task_id.to_string());
        self
    }

    pub fn requires_lock(mut self, resource: &str) -> Self {
        self.requires_locks.push(resource.to_string());
        self
    }

    pub fn awaits_coordination(mut self, request_id: &str) -> Self {
        self.awaiting_coordination.push(request_id.to_string());
        self
    }

    pub fn has_requirements(&self) -> bool {
        self.min_delay.is_some()
            || !self.depends_on_tasks.is_empty()
            || !self.requires_locks.is_empty()
            || !self.awaiting_coordination.is_empty()
            || !self.custom_conditions.is_empty()
    }
}

/// Checkpoint data for resuming a continuation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContinuationCheckpoint {
    /// Description of where we left off.
    pub description: String,
    /// Files that were being worked on.
    pub working_files: Vec<String>,
    /// Files that were modified before checkpoint.
    pub modified_files: Vec<String>,
    /// Work completed so far.
    pub completed_work: Vec<String>,
    /// Work remaining.
    pub remaining_work: Vec<String>,
    /// Key-value data for the agent.
    pub data: HashMap<String, String>,
    /// Structured state for the agent.
    pub state: Option<serde_json::Value>,
}

impl ContinuationCheckpoint {
    pub fn new(description: &str) -> Self {
        Self {
            description: description.to_string(),
            ..Default::default()
        }
    }

    pub fn with_files(mut self, working: Vec<String>, modified: Vec<String>) -> Self {
        self.working_files = working;
        self.modified_files = modified;
        self
    }

    pub fn with_work(mut self, completed: Vec<String>, remaining: Vec<String>) -> Self {
        self.completed_work = completed;
        self.remaining_work = remaining;
        self
    }

    pub fn with_data(mut self, key: &str, value: &str) -> Self {
        self.data.insert(key.to_string(), value.to_string());
        self
    }

    pub fn with_state<T: Serialize>(mut self, state: &T) -> Self {
        self.state = serde_json::to_value(state).ok();
        self
    }

    pub fn get_state<T: for<'de> Deserialize<'de>>(&self) -> Option<T> {
        self.state
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

/// Manager for pending continuations.
#[derive(Debug)]
pub struct ContinuationManager {
    continuations: HashMap<String, ContinuationState>,
    by_agent: HashMap<String, Vec<String>>,
    by_task: HashMap<String, Vec<String>>,
    pending_queue: VecDeque<String>,
    max_pending: usize,
}

impl Default for ContinuationManager {
    fn default() -> Self {
        Self::new(1000)
    }
}

impl ContinuationManager {
    pub fn new(max_pending: usize) -> Self {
        Self {
            continuations: HashMap::new(),
            by_agent: HashMap::new(),
            by_task: HashMap::new(),
            pending_queue: VecDeque::new(),
            max_pending,
        }
    }

    /// Create a continuation from an invocation result.
    pub fn create_from_result(
        &mut self,
        result: &InvocationResult,
        task_id: &str,
    ) -> Option<String> {
        if result.outcome != InvocationOutcome::Pending || result.follow_ups.is_empty() {
            return None;
        }

        let id = generate_continuation_id(&result.agent_id, task_id);
        let mut state = ContinuationState::new(
            id.clone(),
            result.agent_id.clone(),
            task_id.to_string(),
            result.invocation_id.clone(),
        );

        // Extract requirements from follow-ups
        let mut requirements = ResumeRequirements::new();
        for follow_up in &result.follow_ups {
            match follow_up.follow_up_type {
                FollowUpType::Continuation => {
                    // Immediate continuation
                }
                FollowUpType::Retry => {
                    if let Some(delay) = follow_up.delay_secs {
                        requirements = requirements.with_delay(Duration::from_secs(delay as u64));
                    }
                }
                FollowUpType::WaitForLock => {
                    if let Some(resource) = follow_up.context_data.get("resource") {
                        requirements = requirements.requires_lock(resource);
                    }
                }
                FollowUpType::WaitForDependency => {
                    for dep in &follow_up.depends_on {
                        requirements = requirements.depends_on(dep);
                    }
                }
                FollowUpType::WaitForCoordination => {
                    if let Some(req_id) = follow_up.context_data.get("request_id") {
                        requirements = requirements.awaits_coordination(req_id);
                    }
                }
                _ => {}
            }
        }

        state.resume_requirements = requirements;

        // Build checkpoint from follow-up context
        let mut checkpoint = ContinuationCheckpoint::new(&result.follow_ups[0].description);
        checkpoint.modified_files = result.files_modified.clone();

        for follow_up in &result.follow_ups {
            for (k, v) in &follow_up.context_data {
                checkpoint.data.insert(k.clone(), v.clone());
            }
        }

        state.checkpoint = checkpoint;

        // Check if ready immediately
        if !state.resume_requirements.has_requirements() {
            state.mark_ready();
        }

        self.add(state);
        Some(id)
    }

    /// Add a continuation.
    pub fn add(&mut self, state: ContinuationState) {
        let id = state.id.clone();
        let agent_id = state.agent_id.as_str().to_string();
        let task_id = state.task_id.clone();

        // Enforce max pending limit
        while self.pending_queue.len() >= self.max_pending {
            if let Some(old_id) = self.pending_queue.pop_front() {
                self.remove(&old_id);
            }
        }

        self.by_agent.entry(agent_id).or_default().push(id.clone());

        self.by_task.entry(task_id).or_default().push(id.clone());

        if state.status == ContinuationStatus::Pending || state.status == ContinuationStatus::Ready
        {
            self.pending_queue.push_back(id.clone());
        }

        self.continuations.insert(id, state);
    }

    /// Get a continuation by ID.
    pub fn get(&self, id: &str) -> Option<&ContinuationState> {
        self.continuations.get(id)
    }

    /// Get a mutable continuation by ID.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ContinuationState> {
        self.continuations.get_mut(id)
    }

    /// Remove a continuation.
    pub fn remove(&mut self, id: &str) -> Option<ContinuationState> {
        if let Some(state) = self.continuations.remove(id) {
            // Clean up indices
            if let Some(ids) = self.by_agent.get_mut(state.agent_id.as_str()) {
                ids.retain(|i| i != id);
            }
            if let Some(ids) = self.by_task.get_mut(&state.task_id) {
                ids.retain(|i| i != id);
            }
            self.pending_queue.retain(|i| i != id);
            Some(state)
        } else {
            None
        }
    }

    /// Get continuations for an agent.
    pub fn for_agent(&self, agent_id: &str) -> Vec<&ContinuationState> {
        self.by_agent
            .get(agent_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.continuations.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get continuations for a task.
    pub fn for_task(&self, task_id: &str) -> Vec<&ContinuationState> {
        self.by_task
            .get(task_id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.continuations.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all ready continuations.
    pub fn ready(&self) -> Vec<&ContinuationState> {
        self.continuations
            .values()
            .filter(|c| c.status == ContinuationStatus::Ready)
            .collect()
    }

    /// Get next ready continuation for scheduling.
    pub fn next_ready(&self) -> Option<&ContinuationState> {
        self.pending_queue
            .iter()
            .filter_map(|id| self.continuations.get(id))
            .find(|c| c.can_resume())
    }

    /// Check requirements and update status for all pending.
    pub fn check_requirements(&mut self, completed_tasks: &[String], available_locks: &[String]) {
        let pending_ids: Vec<String> = self.pending_queue.iter().cloned().collect();

        for id in pending_ids {
            if let Some(state) = self.continuations.get_mut(&id) {
                if state.status != ContinuationStatus::Pending {
                    continue;
                }

                let reqs = &state.resume_requirements;
                let mut ready = true;

                if let Some(delay) = reqs.min_delay
                    && let Some(created) = state.created_at
                    && created.elapsed() < delay
                {
                    ready = false;
                }

                // Check task dependencies
                for dep in &reqs.depends_on_tasks {
                    if !completed_tasks.contains(dep) {
                        ready = false;
                        break;
                    }
                }

                // Check locks
                for lock in &reqs.requires_locks {
                    if !available_locks.contains(lock) {
                        ready = false;
                        break;
                    }
                }

                if ready {
                    state.mark_ready();
                }
            }
        }
    }

    /// Mark a continuation as completed.
    pub fn complete(&mut self, id: &str) {
        if let Some(state) = self.continuations.get_mut(id) {
            state.mark_completed();
            self.pending_queue.retain(|i| i != id);
        }
    }

    /// Mark a continuation as failed.
    pub fn fail(&mut self, id: &str, error: &str) {
        if let Some(state) = self.continuations.get_mut(id) {
            state.mark_failed(error);
            self.pending_queue.retain(|i| i != id);
        }
    }

    /// Abandon a continuation after max attempts.
    pub fn abandon(&mut self, id: &str) {
        if let Some(state) = self.continuations.get_mut(id) {
            state.status = ContinuationStatus::Abandoned;
            self.pending_queue.retain(|i| i != id);
        }
    }

    /// Get count of pending continuations.
    pub fn pending_count(&self) -> usize {
        self.pending_queue.len()
    }

    /// Get total continuation count.
    pub fn total_count(&self) -> usize {
        self.continuations.len()
    }

    /// Get statistics.
    pub fn stats(&self) -> ContinuationStats {
        let mut by_status = HashMap::new();
        for state in self.continuations.values() {
            let status_key = match &state.status {
                ContinuationStatus::Pending => "pending",
                ContinuationStatus::Ready => "ready",
                ContinuationStatus::Blocked { .. } => "blocked",
                ContinuationStatus::Completed => "completed",
                ContinuationStatus::Failed { .. } => "failed",
                ContinuationStatus::Abandoned => "abandoned",
            };
            *by_status.entry(status_key.to_string()).or_insert(0) += 1;
        }

        ContinuationStats {
            total: self.continuations.len(),
            pending: self.pending_queue.len(),
            by_status,
        }
    }

    /// Clear all continuations.
    pub fn clear_all(&mut self) {
        self.continuations.clear();
        self.by_agent.clear();
        self.by_task.clear();
        self.pending_queue.clear();
    }
}

/// Statistics about continuations.
#[derive(Debug, Clone)]
pub struct ContinuationStats {
    pub total: usize,
    pub pending: usize,
    pub by_status: HashMap<String, usize>,
}

/// Generate a unique continuation ID.
pub fn generate_continuation_id(agent_id: &AgentId, task_id: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    format!("cont-{}-{}-{}", agent_id.as_str(), task_id, timestamp)
}

/// Convert a FollowUp to continuation requirements.
pub fn follow_up_to_requirements(follow_up: &FollowUp) -> ResumeRequirements {
    let mut reqs = ResumeRequirements::new();

    if let Some(delay) = follow_up.delay_secs {
        reqs = reqs.with_delay(Duration::from_secs(delay as u64));
    }

    for dep in &follow_up.depends_on {
        reqs = reqs.depends_on(dep);
    }

    if let Some(resource) = follow_up.context_data.get("resource") {
        reqs = reqs.requires_lock(resource);
    }

    if let Some(req_id) = follow_up.context_data.get("request_id") {
        reqs = reqs.awaits_coordination(req_id);
    }

    reqs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_pending_result(agent_id: &str, follow_ups: Vec<FollowUp>) -> InvocationResult {
        InvocationResult::pending(
            "inv-1".to_string(),
            AgentId::new(agent_id),
            follow_ups,
            Duration::from_secs(10),
        )
    }

    #[test]
    fn test_continuation_state_lifecycle() {
        let mut state = ContinuationState::new(
            "cont-1".to_string(),
            AgentId::new("agent-1"),
            "task-1".to_string(),
            "inv-1".to_string(),
        );

        assert_eq!(state.status, ContinuationStatus::Pending);
        assert_eq!(state.phase, 1);
        assert_eq!(state.attempt_count, 0);

        state.mark_ready();
        assert!(state.can_resume());

        state.increment_attempt();
        assert_eq!(state.attempt_count, 1);

        state.advance_phase();
        assert_eq!(state.phase, 2);

        state.mark_completed();
        assert!(!state.can_resume());
    }

    #[test]
    fn test_continuation_progress() {
        let mut state = ContinuationState::new(
            "cont-1".to_string(),
            AgentId::new("agent-1"),
            "task-1".to_string(),
            "inv-1".to_string(),
        )
        .with_phases(4);

        // Phase 1, 50% progress = 12.5% overall
        state.update_progress(50);
        assert!((state.overall_progress() - 12.5).abs() < 0.1);

        // Phase 2, 0% progress = 25% overall
        state.advance_phase();
        assert!((state.overall_progress() - 25.0).abs() < 0.1);

        // Phase 2, 100% progress = 50% overall
        state.update_progress(100);
        assert!((state.overall_progress() - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_resume_requirements() {
        let reqs = ResumeRequirements::new()
            .with_delay(Duration::from_secs(60))
            .depends_on("task-1")
            .requires_lock("config.rs");

        assert!(reqs.has_requirements());
        assert_eq!(reqs.min_delay, Some(Duration::from_secs(60)));
        assert!(reqs.depends_on_tasks.contains(&"task-1".to_string()));
        assert!(reqs.requires_locks.contains(&"config.rs".to_string()));
    }

    #[test]
    fn test_continuation_checkpoint() {
        let checkpoint = ContinuationCheckpoint::new("Implementing auth module")
            .with_files(vec!["auth.rs".to_string()], vec!["config.rs".to_string()])
            .with_work(
                vec!["Defined types".to_string()],
                vec!["Implement handlers".to_string()],
            )
            .with_data("last_function", "login");

        assert_eq!(checkpoint.working_files, vec!["auth.rs"]);
        assert_eq!(
            checkpoint.data.get("last_function"),
            Some(&"login".to_string())
        );
    }

    #[test]
    fn test_continuation_manager_create_from_result() {
        let mut manager = ContinuationManager::new(100);

        let follow_ups = vec![FollowUp::continuation("Continue implementation")];
        let result = create_pending_result("agent-1", follow_ups);

        let cont_id = manager.create_from_result(&result, "task-1");
        assert!(cont_id.is_some());

        let state = manager.get(cont_id.as_ref().unwrap()).unwrap();
        assert_eq!(state.status, ContinuationStatus::Ready); // No requirements, immediately ready
    }

    #[test]
    fn test_continuation_manager_with_requirements() {
        let mut manager = ContinuationManager::new(100);

        let follow_ups = vec![FollowUp::wait_for_dependency("task-2")];
        let result = create_pending_result("agent-1", follow_ups);

        let cont_id = manager.create_from_result(&result, "task-1").unwrap();
        let state = manager.get(&cont_id).unwrap();

        // Should be pending because dependency not met
        assert_eq!(state.status, ContinuationStatus::Pending);
        assert!(
            state
                .resume_requirements
                .depends_on_tasks
                .contains(&"task-2".to_string())
        );
    }

    #[test]
    fn test_check_requirements() {
        let mut manager = ContinuationManager::new(100);

        let follow_ups = vec![FollowUp::wait_for_dependency("task-2")];
        let result = create_pending_result("agent-1", follow_ups);
        let cont_id = manager.create_from_result(&result, "task-1").unwrap();

        // Requirements not met
        manager.check_requirements(&[], &[]);
        assert_eq!(
            manager.get(&cont_id).unwrap().status,
            ContinuationStatus::Pending
        );

        // Requirements met
        manager.check_requirements(&["task-2".to_string()], &[]);
        assert_eq!(
            manager.get(&cont_id).unwrap().status,
            ContinuationStatus::Ready
        );
    }

    #[test]
    fn test_continuation_manager_indexing() {
        let mut manager = ContinuationManager::new(100);

        // Add several continuations
        for i in 0..5 {
            let state = ContinuationState::new(
                format!("cont-{}", i),
                AgentId::new(if i < 3 { "agent-1" } else { "agent-2" }),
                format!("task-{}", i % 2),
                format!("inv-{}", i),
            );
            manager.add(state);
        }

        assert_eq!(manager.for_agent("agent-1").len(), 3);
        assert_eq!(manager.for_agent("agent-2").len(), 2);
        assert_eq!(manager.for_task("task-0").len(), 3); // 0, 2, 4
        assert_eq!(manager.for_task("task-1").len(), 2); // 1, 3
    }

    #[test]
    fn test_continuation_complete_and_fail() {
        let mut manager = ContinuationManager::new(100);

        let state = ContinuationState::new(
            "cont-1".to_string(),
            AgentId::new("agent-1"),
            "task-1".to_string(),
            "inv-1".to_string(),
        );
        manager.add(state);

        manager.complete("cont-1");
        assert_eq!(
            manager.get("cont-1").unwrap().status,
            ContinuationStatus::Completed
        );

        let state2 = ContinuationState::new(
            "cont-2".to_string(),
            AgentId::new("agent-1"),
            "task-2".to_string(),
            "inv-2".to_string(),
        );
        manager.add(state2);

        manager.fail("cont-2", "Out of memory");
        match &manager.get("cont-2").unwrap().status {
            ContinuationStatus::Failed { error } => assert!(error.contains("memory")),
            _ => panic!("Expected Failed status"),
        }
    }

    #[test]
    fn test_continuation_stats() {
        let mut manager = ContinuationManager::new(100);

        for i in 0..10 {
            let mut state = ContinuationState::new(
                format!("cont-{}", i),
                AgentId::new("agent-1"),
                format!("task-{}", i),
                format!("inv-{}", i),
            );
            if i < 5 {
                state.mark_ready();
            }
            manager.add(state);
        }

        let stats = manager.stats();
        assert_eq!(stats.total, 10);
        assert_eq!(stats.by_status.get("ready"), Some(&5));
        assert_eq!(stats.by_status.get("pending"), Some(&5));
    }

    #[test]
    fn test_max_attempts() {
        let mut state = ContinuationState::new(
            "cont-1".to_string(),
            AgentId::new("agent-1"),
            "task-1".to_string(),
            "inv-1".to_string(),
        );
        state.max_attempts = 3;
        state.mark_ready();

        assert!(state.can_resume());

        for _ in 0..3 {
            state.increment_attempt();
        }

        assert!(!state.can_resume());
    }
}
