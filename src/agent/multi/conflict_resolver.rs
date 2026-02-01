//! Polling-based conflict resolution for file ownership conflicts.
//!
//! Uses MessageStore for asynchronous conflict negotiation between agents,
//! supporting stateless agent invocation patterns.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::hierarchy::AgentId;
use super::message_store::{MessageStore, StoredMessage};
use super::ownership::{AccessType, AcquisitionResult, FileLease, FileOwnershipManager};
use crate::error::{PilotError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictAction {
    RequestRelease,
    GrantRelease,
    DenyRelease,
    Escalate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRequest {
    pub file: PathBuf,
    pub requester: AgentId,
    pub access_type: AccessType,
    pub reason: String,
    pub priority: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResponse {
    pub file: PathBuf,
    pub granted: bool,
    pub release_in: Option<Duration>,
    pub alternative_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WaitingRequest {
    pub requester: AgentId,
    pub requested_at: Instant,
    pub priority: u8,
    pub message_id: String,
}

pub struct ConflictResolver {
    ownership_manager: Arc<FileOwnershipManager>,
    message_store: Arc<MessageStore>,
    wait_queues: DashMap<PathBuf, VecDeque<WaitingRequest>>,
    poll_interval: Duration,
    max_wait_time: Duration,
}

impl ConflictResolver {
    pub fn new(
        ownership_manager: Arc<FileOwnershipManager>,
        message_store: Arc<MessageStore>,
    ) -> Self {
        Self {
            ownership_manager,
            message_store,
            wait_queues: DashMap::new(),
            poll_interval: Duration::from_secs(5),
            max_wait_time: Duration::from_secs(300),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait_time = max_wait;
        self
    }

    /// Acquire files in sorted order to prevent deadlocks.
    pub fn acquire_ordered(
        &self,
        files: &[PathBuf],
        requester: &AgentId,
        requester_module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> Result<Vec<AcquisitionResult>> {
        let mut sorted_files: Vec<_> = files.to_vec();
        sorted_files.sort();

        let mut results = Vec::with_capacity(sorted_files.len());
        let mut acquired_leases = Vec::new();

        for file in &sorted_files {
            match self.ownership_manager.acquire(
                file,
                requester.as_str(),
                requester_module,
                access,
                reason,
            ) {
                AcquisitionResult::Granted { lease } => {
                    acquired_leases.push(Arc::clone(&lease));
                    results.push(AcquisitionResult::Granted { lease });
                }
                other => {
                    // Rollback all acquired leases
                    for lease in acquired_leases {
                        drop(lease);
                    }

                    // Return the blocking result for the first blocking file
                    return Ok(vec![other]);
                }
            }
        }

        Ok(results)
    }

    /// Request file release via message store (async negotiation).
    pub fn request_release(
        &self,
        file: &Path,
        requester: &AgentId,
        current_owner: &AgentId,
        priority: u8,
        reason: &str,
    ) -> Result<String> {
        let request = ConflictRequest {
            file: file.to_path_buf(),
            requester: requester.clone(),
            access_type: AccessType::Exclusive,
            reason: reason.to_string(),
            priority,
        };

        let payload = serde_json::to_string(&request)
            .map_err(|e| PilotError::Ownership(format!("Failed to serialize request: {}", e)))?;

        let message_id =
            self.message_store
                .enqueue(current_owner, requester, "request_release", &payload)?;

        // Add to fair queue
        self.add_to_wait_queue(file, requester, priority, &message_id);

        debug!(
            file = %file.display(),
            requester = %requester,
            owner = %current_owner,
            message_id = %message_id,
            "File release requested"
        );

        Ok(message_id)
    }

    /// Process pending release requests for an agent.
    pub fn process_pending_requests(
        &self,
        agent_id: &AgentId,
    ) -> Result<Vec<(StoredMessage, ConflictRequest)>> {
        let pending = self.message_store.pending_for(agent_id)?;

        let mut requests = Vec::new();
        for msg in pending {
            if msg.action == "request_release"
                && let Ok(request) = serde_json::from_str::<ConflictRequest>(&msg.payload)
            {
                requests.push((msg, request));
            }
        }

        Ok(requests)
    }

    /// Respond to a release request.
    pub fn respond_to_request(
        &self,
        message_id: &str,
        granted: bool,
        release_in: Option<Duration>,
    ) -> Result<()> {
        let response = ConflictResponse {
            file: PathBuf::new(), // Filled from request context
            granted,
            release_in,
            alternative_files: Vec::new(),
        };

        let response_json = serde_json::to_string(&response)
            .map_err(|e| PilotError::Ownership(format!("Failed to serialize response: {}", e)))?;

        self.message_store.respond(message_id, &response_json)?;

        debug!(message_id = %message_id, granted, "Responded to release request");
        Ok(())
    }

    /// Poll for response to a release request.
    pub async fn poll_for_response(&self, message_id: &str) -> Result<Option<ConflictResponse>> {
        let start = Instant::now();

        while start.elapsed() < self.max_wait_time {
            if let Some(response_json) = self.message_store.get_response(message_id)? {
                let response: ConflictResponse =
                    serde_json::from_str(&response_json).map_err(|e| {
                        PilotError::Ownership(format!("Failed to parse response: {}", e))
                    })?;
                return Ok(Some(response));
            }

            tokio::time::sleep(self.poll_interval).await;
        }

        warn!(message_id = %message_id, "Release request timed out");
        Ok(None)
    }

    /// Get queue position for a file request.
    pub fn queue_position(&self, file: &Path, requester: &AgentId) -> Option<usize> {
        self.wait_queues
            .get(file)
            .and_then(|queue| queue.iter().position(|r| &r.requester == requester))
    }

    /// Check if requester is next in queue (fair scheduling).
    pub fn is_next_in_queue(&self, file: &Path, requester: &AgentId) -> bool {
        self.wait_queues.get(file).is_some_and(|queue| {
            queue
                .front()
                .is_some_and(|first| &first.requester == requester)
        })
    }

    /// Try to acquire with fair queue consideration.
    pub fn acquire_with_queue(
        &self,
        file: &Path,
        requester: &AgentId,
        module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> AcquisitionResult {
        // Check if someone else is waiting first (fairness)
        if let Some(queue) = self.wait_queues.get(file)
            && let Some(first) = queue.front()
            && &first.requester != requester
        {
            let position = queue
                .iter()
                .position(|r| &r.requester == requester)
                .unwrap_or(queue.len());
            return AcquisitionResult::Queued {
                position,
                estimated_wait: Duration::from_secs(30 * position as u64),
            };
        }

        // No queue or we're first - try normal acquisition
        self.ownership_manager
            .acquire(file, requester.as_str(), module, access, reason)
    }

    /// Cleanup expired wait requests and messages.
    pub fn cleanup(&self) -> Result<usize> {
        let now = Instant::now();
        let mut removed = 0;

        // Cleanup expired waiting requests
        self.wait_queues.retain(|_, queue| {
            let before = queue.len();
            queue.retain(|req| now.duration_since(req.requested_at) < self.max_wait_time);
            removed += before - queue.len();
            !queue.is_empty()
        });

        // Cleanup expired messages
        let expired_messages = self.message_store.cleanup_expired()?;
        removed += expired_messages;

        if removed > 0 {
            debug!(removed, "Cleaned up expired conflict requests");
        }

        Ok(removed)
    }

    fn add_to_wait_queue(&self, file: &Path, requester: &AgentId, priority: u8, message_id: &str) {
        let mut queue = self.wait_queues.entry(file.to_path_buf()).or_default();

        // Don't add duplicates
        if queue.iter().any(|r| &r.requester == requester) {
            return;
        }

        let request = WaitingRequest {
            requester: requester.clone(),
            requested_at: Instant::now(),
            priority,
            message_id: message_id.to_string(),
        };

        // Insert by priority (higher priority = earlier in queue)
        let insert_pos = queue
            .iter()
            .position(|r| r.priority < priority)
            .unwrap_or(queue.len());

        queue.insert(insert_pos, request);
    }

    /// Remove from wait queue when request is satisfied.
    pub fn remove_from_queue(&self, file: &Path, requester: &AgentId) {
        if let Some(mut queue) = self.wait_queues.get_mut(file) {
            queue.retain(|r| &r.requester != requester);
        }
    }

    /// Grant release and transfer ownership.
    #[allow(clippy::too_many_arguments)]
    pub fn grant_and_transfer(
        &self,
        file: &Path,
        current_owner: &str,
        current_version: u64,
        new_owner: &AgentId,
        module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> Result<Arc<FileLease>> {
        // Release current ownership
        self.ownership_manager
            .release(file, current_owner, current_version);

        // Remove from queue
        self.remove_from_queue(file, new_owner);

        // Grant to new owner
        match self
            .ownership_manager
            .acquire(file, new_owner.as_str(), module, access, reason)
        {
            AcquisitionResult::Granted { lease } => Ok(lease),
            other => Err(PilotError::Ownership(format!(
                "Failed to transfer ownership: {:?}",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ConflictResolver) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("conflict_messages.db");

        let ownership = Arc::new(FileOwnershipManager::new());
        let messages = Arc::new(MessageStore::new(&db_path).unwrap());
        let resolver = ConflictResolver::new(ownership, messages)
            .with_poll_interval(Duration::from_millis(100))
            .with_max_wait(Duration::from_secs(5));

        (dir, resolver)
    }

    #[test]
    fn test_acquire_ordered_prevents_deadlock() {
        let (_dir, resolver) = setup();

        let files = vec![
            PathBuf::from("b.rs"),
            PathBuf::from("a.rs"),
            PathBuf::from("c.rs"),
        ];

        let agent = AgentId::new("coder-0");
        let results = resolver
            .acquire_ordered(&files, &agent, None, AccessType::Exclusive, "test")
            .unwrap();

        // Should acquire all 3 files
        assert_eq!(results.len(), 3);
        for result in &results {
            assert!(matches!(result, AcquisitionResult::Granted { .. }));
        }
    }

    #[test]
    fn test_fair_queue() {
        let (_dir, resolver) = setup();
        let file = PathBuf::from("contested.rs");

        // First agent acquires
        let agent1 = AgentId::new("coder-1");
        let result =
            resolver.acquire_with_queue(&file, &agent1, None, AccessType::Exclusive, "first");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));

        // Second agent gets queued
        let agent2 = AgentId::new("coder-2");
        let owner = AgentId::new("coder-1");
        let _msg_id = resolver
            .request_release(&file, &agent2, &owner, 0, "need file")
            .unwrap();

        assert!(resolver.queue_position(&file, &agent2).is_some());
        // Agent2 is first in wait queue (owner is not in queue, they hold the file)
        assert!(resolver.is_next_in_queue(&file, &agent2));
    }

    #[test]
    fn test_priority_ordering() {
        let (_dir, resolver) = setup();
        let file = PathBuf::from("priority.rs");
        let owner = AgentId::new("owner");

        // Add requests with different priorities
        let low = AgentId::new("low");
        let high = AgentId::new("high");

        resolver
            .request_release(&file, &low, &owner, 1, "low priority")
            .unwrap();
        resolver
            .request_release(&file, &high, &owner, 10, "high priority")
            .unwrap();

        // High priority should be first
        assert!(resolver.is_next_in_queue(&file, &high));
        assert_eq!(resolver.queue_position(&file, &low), Some(1));
    }

    #[test]
    fn test_cleanup() {
        let (_dir, resolver) = setup();

        // Create some requests
        let file = PathBuf::from("temp.rs");
        let agent = AgentId::new("agent");
        let owner = AgentId::new("owner");

        resolver
            .request_release(&file, &agent, &owner, 0, "temp")
            .unwrap();

        // Should have 1 in queue
        assert!(resolver.wait_queues.get(&file).is_some());

        // Cleanup (nothing should be expired yet)
        let removed = resolver.cleanup().unwrap();
        assert_eq!(removed, 0);
    }
}
