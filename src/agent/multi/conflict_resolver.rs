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
use tracing::debug;

use super::shared::AgentId;
use super::message_store::{MessageStore, StoredMessage};
use super::ownership::{AccessType, AcquisitionResult, FileOwnershipManager};
use crate::error::{PilotError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConflictRequest {
    pub(crate) file: PathBuf,
    pub(crate) requester: AgentId,
    pub(crate) access_type: AccessType,
    pub(crate) reason: String,
    pub(crate) priority: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConflictResponse {
    pub(crate) granted: bool,
    pub(crate) release_in: Option<Duration>,
}

#[derive(Debug, Clone)]
pub(crate) struct WaitingRequest {
    pub(crate) requester: AgentId,
    pub(crate) requested_at: Instant,
    pub(crate) priority: u8,
}

/// P2P conflict resolution subsystem for negotiated ownership transfers.
pub struct ConflictResolver {
    ownership_manager: Arc<FileOwnershipManager>,
    message_store: Arc<MessageStore>,
    wait_queues: DashMap<PathBuf, VecDeque<WaitingRequest>>,
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
            max_wait_time: Duration::from_secs(300),
        }
    }

    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait_time = max_wait;
        self
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

        self.add_to_wait_queue(file, requester, priority);

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
    pub(crate) fn process_pending_requests(
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
            granted,
            release_in,
        };

        let response_json = serde_json::to_string(&response)
            .map_err(|e| PilotError::Ownership(format!("Failed to serialize response: {}", e)))?;

        self.message_store.respond(message_id, &response_json)?;

        debug!(message_id = %message_id, granted, "Responded to release request");
        Ok(())
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

    /// Cleanup expired wait requests and messages.
    pub fn cleanup(&self) -> Result<usize> {
        let now = Instant::now();
        let mut removed = 0;

        self.wait_queues.retain(|_, queue| {
            let before = queue.len();
            queue.retain(|req| now.duration_since(req.requested_at) < self.max_wait_time);
            removed += before - queue.len();
            !queue.is_empty()
        });

        let expired_messages = self.message_store.cleanup_expired()?;
        removed += expired_messages;

        if removed > 0 {
            debug!(removed, "Cleaned up expired conflict requests");
        }

        Ok(removed)
    }

    fn add_to_wait_queue(&self, file: &Path, requester: &AgentId, priority: u8) {
        let mut queue = self.wait_queues.entry(file.to_path_buf()).or_default();

        if queue.iter().any(|r| &r.requester == requester) {
            return;
        }

        let request = WaitingRequest {
            requester: requester.clone(),
            requested_at: Instant::now(),
            priority,
        };

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
    pub(crate) fn grant_and_transfer(
        &self,
        file: &Path,
        current_owner: &str,
        current_version: u64,
        new_owner: &AgentId,
        module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> Result<Arc<super::ownership::FileLease>> {
        self.ownership_manager
            .release(file, current_owner, current_version);

        match self
            .ownership_manager
            .acquire(file, new_owner.as_str(), module, access, reason)
        {
            AcquisitionResult::Granted { lease } => {
                self.remove_from_queue(file, new_owner);
                Ok(lease)
            }
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

    fn setup() -> (TempDir, Arc<FileOwnershipManager>, ConflictResolver) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("conflict_messages.db");

        let ownership = Arc::new(FileOwnershipManager::new());
        let messages = Arc::new(MessageStore::new(&db_path).unwrap());
        let resolver = ConflictResolver::new(Arc::clone(&ownership), messages)
            .with_max_wait(Duration::from_secs(5));

        (dir, ownership, resolver)
    }

    #[test]
    fn test_fair_queue() {
        let (_dir, ownership, resolver) = setup();
        let file = PathBuf::from("contested.rs");

        // First agent acquires via ownership manager
        let agent1 = AgentId::new("coder-1");
        let result =
            ownership.acquire(&file, agent1.as_str(), None, AccessType::Exclusive, "first");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));

        // Second agent requests release via conflict resolver
        let agent2 = AgentId::new("coder-2");
        let _msg_id = resolver
            .request_release(&file, &agent2, &agent1, 0, "need file")
            .unwrap();

        assert!(resolver.queue_position(&file, &agent2).is_some());
        assert!(resolver.is_next_in_queue(&file, &agent2));
    }

    #[test]
    fn test_priority_ordering() {
        let (_dir, _ownership, resolver) = setup();
        let file = PathBuf::from("priority.rs");
        let owner = AgentId::new("owner");

        let low = AgentId::new("low");
        let high = AgentId::new("high");

        resolver
            .request_release(&file, &low, &owner, 1, "low priority")
            .unwrap();
        resolver
            .request_release(&file, &high, &owner, 10, "high priority")
            .unwrap();

        assert!(resolver.is_next_in_queue(&file, &high));
        assert_eq!(resolver.queue_position(&file, &low), Some(1));
    }

    #[test]
    fn test_cleanup() {
        let (_dir, _ownership, resolver) = setup();

        let file = PathBuf::from("temp.rs");
        let agent = AgentId::new("agent");
        let owner = AgentId::new("owner");

        resolver
            .request_release(&file, &agent, &owner, 0, "temp")
            .unwrap();

        assert!(resolver.wait_queues.get(&file).is_some());

        let removed = resolver.cleanup().unwrap();
        assert_eq!(removed, 0);
    }
}
