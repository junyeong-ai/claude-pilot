//! Shared state management for real-time coordination.
//!
//! Provides thread-safe shared state that agents can access through the
//! orchestrator for locks, progress tracking, and coordination data.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use serde::{Deserialize, Serialize};

use crate::agent::multi::AgentId;

/// Result of a lock acquisition attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    Acquired,
    AlreadyHeld,
    Denied { holder: AgentId },
}

/// A resource lock.
#[derive(Debug, Clone)]
pub struct ResourceLock {
    pub resource: String,
    pub holder: AgentId,
    pub acquired_at: Instant,
    pub expires_at: Option<Instant>,
    pub lock_type: LockType,
}

impl ResourceLock {
    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|e| Instant::now() > e).unwrap_or(false)
    }

    pub fn remaining_duration(&self) -> Option<Duration> {
        self.expires_at
            .map(|e| e.saturating_duration_since(Instant::now()))
    }
}

/// Type of lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockType {
    Exclusive,
    Shared,
}

/// Progress update from an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEntry {
    pub agent_id: String,
    pub task_id: String,
    pub progress_percent: u32,
    pub message: String,
    pub updated_at_ms: u64,
}

/// Coordination data entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationEntry {
    pub key: String,
    pub data: serde_json::Value,
    pub owner: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

/// Inner state protected by RwLock.
#[derive(Debug, Default)]
struct SharedStateInner {
    locks: HashMap<String, ResourceLock>,
    progress: HashMap<String, ProgressEntry>,
    coordination: HashMap<String, CoordinationEntry>,
    counters: HashMap<String, i64>,
}

/// Thread-safe shared state for coordination.
#[derive(Debug, Clone)]
pub struct SharedState {
    inner: Arc<RwLock<SharedStateInner>>,
    default_lock_duration: Duration,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SharedStateInner::default())),
            default_lock_duration: Duration::from_secs(300), // 5 minutes
        }
    }

    pub fn with_lock_duration(mut self, duration: Duration) -> Self {
        self.default_lock_duration = duration;
        self
    }

    // === Lock Management ===

    /// Acquire a lock on a resource.
    pub fn acquire_lock(
        &self,
        resource: &str,
        holder: &AgentId,
        lock_type: LockType,
        duration: Option<Duration>,
    ) -> LockResult {
        let mut state = self.inner.write();

        // Check existing lock
        if let Some(existing) = state.locks.get(resource) {
            if existing.is_expired() {
                // Expired lock, can acquire
            } else if existing.holder == *holder {
                return LockResult::AlreadyHeld;
            } else if lock_type == LockType::Shared && existing.lock_type == LockType::Shared {
                // Shared locks can coexist - but we'll simplify to single holder
                return LockResult::Denied {
                    holder: existing.holder.clone(),
                };
            } else {
                return LockResult::Denied {
                    holder: existing.holder.clone(),
                };
            }
        }

        let duration = duration.unwrap_or(self.default_lock_duration);
        let lock = ResourceLock {
            resource: resource.to_string(),
            holder: holder.clone(),
            acquired_at: Instant::now(),
            expires_at: Some(Instant::now() + duration),
            lock_type,
        };

        state.locks.insert(resource.to_string(), lock);
        LockResult::Acquired
    }

    /// Release a lock.
    pub fn release_lock(&self, resource: &str, holder: &AgentId) -> bool {
        let mut state = self.inner.write();

        if let Some(lock) = state.locks.get(resource)
            && lock.holder == *holder
        {
            state.locks.remove(resource);
            return true;
        }
        false
    }

    /// Check if a resource is locked.
    pub fn is_locked(&self, resource: &str) -> bool {
        let state = self.inner.read();
        state
            .locks
            .get(resource)
            .map(|l| !l.is_expired())
            .unwrap_or(false)
    }

    /// Get lock holder for a resource.
    pub fn lock_holder(&self, resource: &str) -> Option<AgentId> {
        let state = self.inner.read();
        state.locks.get(resource).and_then(|l| {
            if l.is_expired() {
                None
            } else {
                Some(l.holder.clone())
            }
        })
    }

    /// Get all locks held by an agent.
    pub fn locks_held_by(&self, holder: &AgentId) -> Vec<String> {
        let state = self.inner.read();
        state
            .locks
            .iter()
            .filter(|(_, l)| l.holder == *holder && !l.is_expired())
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// Get all active locks.
    pub fn all_locks(&self) -> HashMap<String, AgentId> {
        let state = self.inner.read();
        state
            .locks
            .iter()
            .filter(|(_, l)| !l.is_expired())
            .map(|(r, l)| (r.clone(), l.holder.clone()))
            .collect()
    }

    /// Clean up expired locks.
    pub fn cleanup_expired_locks(&self) -> usize {
        let mut state = self.inner.write();
        let before = state.locks.len();
        state.locks.retain(|_, l| !l.is_expired());
        before - state.locks.len()
    }

    // === Progress Tracking ===

    /// Update progress for a task.
    pub fn update_progress(
        &self,
        agent_id: &str,
        task_id: &str,
        progress_percent: u32,
        message: &str,
    ) {
        let mut state = self.inner.write();
        let key = format!("{}:{}", agent_id, task_id);

        state.progress.insert(
            key,
            ProgressEntry {
                agent_id: agent_id.to_string(),
                task_id: task_id.to_string(),
                progress_percent: progress_percent.min(100),
                message: message.to_string(),
                updated_at_ms: current_time_ms(),
            },
        );
    }

    /// Get progress for a task.
    pub fn get_progress(&self, agent_id: &str, task_id: &str) -> Option<ProgressEntry> {
        let state = self.inner.read();
        let key = format!("{}:{}", agent_id, task_id);
        state.progress.get(&key).cloned()
    }

    /// Get all progress entries for an agent.
    pub fn agent_progress(&self, agent_id: &str) -> Vec<ProgressEntry> {
        let state = self.inner.read();
        state
            .progress
            .values()
            .filter(|p| p.agent_id == agent_id)
            .cloned()
            .collect()
    }

    /// Get all progress entries for a task.
    pub fn task_progress(&self, task_id: &str) -> Vec<ProgressEntry> {
        let state = self.inner.read();
        state
            .progress
            .values()
            .filter(|p| p.task_id == task_id)
            .cloned()
            .collect()
    }

    /// Clear progress for a task.
    pub fn clear_progress(&self, agent_id: &str, task_id: &str) {
        let mut state = self.inner.write();
        let key = format!("{}:{}", agent_id, task_id);
        state.progress.remove(&key);
    }

    // === Coordination Data ===

    /// Set coordination data.
    pub fn set_coordination<T: Serialize>(
        &self,
        key: &str,
        data: &T,
        owner: &str,
    ) -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(data)?;
        let mut state = self.inner.write();
        let now = current_time_ms();

        let entry = state
            .coordination
            .entry(key.to_string())
            .or_insert_with(|| CoordinationEntry {
                key: key.to_string(),
                data: serde_json::Value::Null,
                owner: owner.to_string(),
                created_at_ms: now,
                updated_at_ms: now,
            });

        entry.data = value;
        entry.updated_at_ms = now;
        Ok(())
    }

    /// Get coordination data.
    pub fn get_coordination<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let state = self.inner.read();
        state
            .coordination
            .get(key)
            .and_then(|e| serde_json::from_value(e.data.clone()).ok())
    }

    /// Get coordination entry metadata.
    pub fn get_coordination_entry(&self, key: &str) -> Option<CoordinationEntry> {
        let state = self.inner.read();
        state.coordination.get(key).cloned()
    }

    /// Remove coordination data.
    pub fn remove_coordination(&self, key: &str) -> bool {
        let mut state = self.inner.write();
        state.coordination.remove(key).is_some()
    }

    /// List all coordination keys.
    pub fn coordination_keys(&self) -> Vec<String> {
        let state = self.inner.read();
        state.coordination.keys().cloned().collect()
    }

    // === Counters ===

    /// Increment a counter.
    pub fn increment(&self, key: &str) -> i64 {
        let mut state = self.inner.write();
        let counter = state.counters.entry(key.to_string()).or_insert(0);
        *counter += 1;
        *counter
    }

    /// Decrement a counter.
    pub fn decrement(&self, key: &str) -> i64 {
        let mut state = self.inner.write();
        let counter = state.counters.entry(key.to_string()).or_insert(0);
        *counter -= 1;
        *counter
    }

    /// Get counter value.
    pub fn get_counter(&self, key: &str) -> i64 {
        let state = self.inner.read();
        state.counters.get(key).copied().unwrap_or(0)
    }

    /// Set counter value.
    pub fn set_counter(&self, key: &str, value: i64) {
        let mut state = self.inner.write();
        state.counters.insert(key.to_string(), value);
    }

    // === Bulk Operations ===

    /// Get a read guard for bulk reads.
    pub fn read(&self) -> SharedStateReadGuard<'_> {
        SharedStateReadGuard {
            guard: self.inner.read(),
        }
    }

    /// Get a write guard for bulk writes.
    pub fn write(&self) -> SharedStateWriteGuard<'_> {
        SharedStateWriteGuard {
            guard: self.inner.write(),
        }
    }

    /// Snapshot current state for serialization.
    pub fn snapshot(&self) -> SharedStateSnapshot {
        let state = self.inner.read();
        SharedStateSnapshot {
            locks: state
                .locks
                .iter()
                .filter(|(_, l)| !l.is_expired())
                .map(|(k, l)| (k.clone(), l.holder.as_str().to_string()))
                .collect(),
            progress: state.progress.clone(),
            coordination: state.coordination.clone(),
            counters: state.counters.clone(),
        }
    }
}

/// Read guard for shared state.
pub struct SharedStateReadGuard<'a> {
    guard: RwLockReadGuard<'a, SharedStateInner>,
}

impl SharedStateReadGuard<'_> {
    pub fn lock_count(&self) -> usize {
        self.guard.locks.len()
    }

    pub fn progress_count(&self) -> usize {
        self.guard.progress.len()
    }
}

/// Write guard for shared state.
pub struct SharedStateWriteGuard<'a> {
    guard: RwLockWriteGuard<'a, SharedStateInner>,
}

impl SharedStateWriteGuard<'_> {
    pub fn clear_all(&mut self) {
        self.guard.locks.clear();
        self.guard.progress.clear();
        self.guard.coordination.clear();
        self.guard.counters.clear();
    }
}

/// Serializable snapshot of shared state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedStateSnapshot {
    pub locks: HashMap<String, String>,
    pub progress: HashMap<String, ProgressEntry>,
    pub coordination: HashMap<String, CoordinationEntry>,
    pub counters: HashMap<String, i64>,
}

fn current_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_acquire_release() {
        let state = SharedState::new();
        let agent1 = AgentId::new("agent-1");
        let agent2 = AgentId::new("agent-2");

        // Acquire lock
        let result = state.acquire_lock("file.rs", &agent1, LockType::Exclusive, None);
        assert_eq!(result, LockResult::Acquired);

        // Same agent re-acquiring
        let result = state.acquire_lock("file.rs", &agent1, LockType::Exclusive, None);
        assert_eq!(result, LockResult::AlreadyHeld);

        // Different agent trying to acquire
        let result = state.acquire_lock("file.rs", &agent2, LockType::Exclusive, None);
        assert!(matches!(result, LockResult::Denied { .. }));

        // Release
        assert!(state.release_lock("file.rs", &agent1));

        // Now agent2 can acquire
        let result = state.acquire_lock("file.rs", &agent2, LockType::Exclusive, None);
        assert_eq!(result, LockResult::Acquired);
    }

    #[test]
    fn test_lock_expiration() {
        let state = SharedState::new();
        let agent1 = AgentId::new("agent-1");
        let agent2 = AgentId::new("agent-2");

        // Acquire with very short duration
        state.acquire_lock(
            "file.rs",
            &agent1,
            LockType::Exclusive,
            Some(Duration::from_millis(1)),
        );

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(5));

        // Agent2 should be able to acquire now
        let result = state.acquire_lock("file.rs", &agent2, LockType::Exclusive, None);
        assert_eq!(result, LockResult::Acquired);
    }

    #[test]
    fn test_progress_tracking() {
        let state = SharedState::new();

        state.update_progress("agent-1", "task-1", 50, "Halfway done");
        state.update_progress("agent-1", "task-2", 25, "Started");

        let progress = state.get_progress("agent-1", "task-1").unwrap();
        assert_eq!(progress.progress_percent, 50);

        let agent_progress = state.agent_progress("agent-1");
        assert_eq!(agent_progress.len(), 2);
    }

    #[test]
    fn test_coordination_data() {
        let state = SharedState::new();

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Config {
            max_retries: u32,
            timeout_ms: u64,
        }

        let config = Config {
            max_retries: 3,
            timeout_ms: 5000,
        };

        state
            .set_coordination("config", &config, "coordinator")
            .unwrap();

        let retrieved: Config = state.get_coordination("config").unwrap();
        assert_eq!(retrieved, config);
    }

    #[test]
    fn test_counters() {
        let state = SharedState::new();

        assert_eq!(state.get_counter("tasks_completed"), 0);

        state.increment("tasks_completed");
        state.increment("tasks_completed");
        assert_eq!(state.get_counter("tasks_completed"), 2);

        state.decrement("tasks_completed");
        assert_eq!(state.get_counter("tasks_completed"), 1);
    }

    #[test]
    fn test_snapshot() {
        let state = SharedState::new();
        let agent = AgentId::new("agent-1");

        state.acquire_lock("file.rs", &agent, LockType::Exclusive, None);
        state.update_progress("agent-1", "task-1", 50, "Working");
        state.set_counter("count", 42);

        let snapshot = state.snapshot();
        assert!(snapshot.locks.contains_key("file.rs"));
        assert!(!snapshot.progress.is_empty());
        assert_eq!(snapshot.counters.get("count"), Some(&42));
    }
}
