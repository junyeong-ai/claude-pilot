//! Lease acquisition, release, and fair queue management.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::debug;

use super::{
    AccessType, AcquisitionResult, FileOwnershipManager, FileLease, FileOwnership, LeaseGuard,
    WaitingEntry,
};

impl FileOwnershipManager {
    pub(crate) fn acquire(
        self: &Arc<Self>,
        file: &Path,
        requester: &str,
        requester_module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> AcquisitionResult {
        let file = file.to_path_buf();

        // Check for cross-module conflicts requiring negotiation
        if let Some(conflicting_modules) =
            self.detect_cross_module_conflict(&file, requester_module)
        {
            return AcquisitionResult::NeedsNegotiation {
                conflicting_modules,
            };
        }

        if let Some(owner_module) = self.get_default_owner(&file)
            && let Some(req_module) = requester_module
            && !self.is_same_or_allowed_module(req_module, &owner_module)
        {
            return AcquisitionResult::Denied {
                reason: format!(
                    "Module boundary violation: {} cannot modify files owned by {}",
                    req_module, owner_module
                ),
                owner_module: Some(owner_module),
            };
        }

        if let Some(current) = self.ownership.get(&file)
            && !current.is_expired()
        {
            match (current.access, access) {
                (AccessType::SharedRead, AccessType::SharedRead) => {
                    // Allow multiple shared readers
                }
                (AccessType::Collaborative, AccessType::Collaborative) => {
                    if let (Some(req_module), Some(curr_module)) =
                        (requester_module, &current.module)
                    {
                        if req_module == curr_module {
                            // Same module collaborative access allowed
                        } else {
                            return AcquisitionResult::Queued {
                                position: 1,
                                estimated_wait: current.remaining_time(),
                            };
                        }
                    }
                }
                _ => {
                    return AcquisitionResult::Queued {
                        position: 1,
                        estimated_wait: current.remaining_time(),
                    };
                }
            }
        }

        self.grant_ownership(file, requester, requester_module, access, reason)
    }

    fn grant_ownership(
        self: &Arc<Self>,
        file: PathBuf,
        owner: &str,
        module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> AcquisitionResult {
        let version = self.version_counter.fetch_add(1, Ordering::SeqCst);
        let ownership = FileOwnership {
            owner: owner.to_string(),
            access,
            expires_at: std::time::Instant::now() + self.default_lease_duration,
            reason: reason.to_string(),
            version,
            module: module.map(String::from),
        };

        self.ownership.insert(file.clone(), ownership);

        let lease = Arc::new(FileLease {
            file,
            owner: owner.to_string(),
            version,
            manager: Arc::clone(self),
            released: AtomicBool::new(false),
        });

        debug!(
            owner = %owner,
            version = version,
            access = ?access,
            "Granted file ownership"
        );

        AcquisitionResult::Granted { lease }
    }

    pub(super) fn release_internal(&self, file: &Path, owner: &str, version: u64) {
        if let Some((_, released)) = self.ownership.remove_if(file, |_, current| {
            current.owner == owner && current.version == version
        }) {
            debug!(owner = %owner, version = version, reason = %released.reason, "Released file ownership");
        }
    }

    pub fn release(&self, file: &Path, owner: &str, version: u64) -> bool {
        self.ownership
            .remove_if(file, |_, current| {
                current.owner == owner && current.version == version
            })
            .is_some()
    }

    /// Release file ownership and return any deferred tasks waiting for this file.
    pub(crate) fn release_and_get_deferred(
        &self,
        file: &Path,
        owner: &str,
        version: u64,
    ) -> Vec<super::DeferredTaskRef> {
        let removed = self.ownership.remove_if(file, |_, current| {
            current.owner == owner && current.version == version
        });

        if removed.is_some() {
            debug!(owner = %owner, version = version, "Released file ownership");
            self.deferred_tasks
                .remove(file)
                .map(|(_, v)| v)
                .unwrap_or_default()
        } else {
            vec![]
        }
    }

    /// Acquire multiple files in sorted order to prevent deadlocks.
    /// If any file cannot be acquired, all previously acquired leases are released.
    pub(crate) fn acquire_ordered(
        self: &Arc<Self>,
        files: &[PathBuf],
        requester: &str,
        requester_module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> std::result::Result<LeaseGuard, AcquisitionResult> {
        if files.is_empty() {
            return Ok(LeaseGuard::new());
        }

        let mut sorted_files: Vec<_> = files.to_vec();
        sorted_files.sort();

        let mut guard = LeaseGuard::new();

        for file in &sorted_files {
            match self.acquire_with_fair_queue(file, requester, requester_module, access, reason) {
                AcquisitionResult::Granted { lease } => {
                    guard.add(lease);
                }
                result @ (AcquisitionResult::Queued { .. }
                | AcquisitionResult::Denied { .. }
                | AcquisitionResult::NeedsNegotiation { .. }) => {
                    return Err(result);
                }
            }
        }

        Ok(guard)
    }

    /// Acquire with fair queue consideration.
    pub(crate) fn acquire_with_fair_queue(
        self: &Arc<Self>,
        file: &Path,
        requester: &str,
        requester_module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> AcquisitionResult {
        let file_buf = file.to_path_buf();

        // Check if someone else is waiting first (fairness)
        if let Some(queue) = self.wait_queues.get(&file_buf)
            && let Some(first) = queue.front()
            && first.requester != requester
        {
            let position = queue
                .iter()
                .position(|e| e.requester == requester)
                .unwrap_or(queue.len());
            return AcquisitionResult::Queued {
                position,
                estimated_wait: Duration::from_secs(30 * position as u64),
            };
        }

        self.acquire(file, requester, requester_module, access, reason)
    }

    /// Add requester to wait queue for fair scheduling.
    pub fn add_to_wait_queue(
        &self,
        file: &Path,
        requester: &str,
        module: Option<&str>,
        priority: u8,
    ) {
        let file_buf = file.to_path_buf();
        let mut queue = self.wait_queues.entry(file_buf).or_default();

        // Don't add duplicates
        if queue.iter().any(|e| e.requester == requester) {
            return;
        }

        let entry = WaitingEntry {
            requester: requester.to_string(),
            module: module.map(|m| m.to_string()),
            requested_at: std::time::Instant::now(),
            priority,
        };

        // Insert by priority (higher priority = earlier in queue)
        let insert_pos = queue
            .iter()
            .position(|e| e.priority < priority)
            .unwrap_or(queue.len());

        queue.insert(insert_pos, entry);
    }

    /// Remove requester from wait queue after acquisition or cancellation.
    pub fn remove_from_wait_queue(&self, file: &Path, requester: &str) {
        if let Some(mut queue) = self.wait_queues.get_mut(file) {
            queue.retain(|e| e.requester != requester);
        }
    }

    /// Get position in wait queue for a file.
    pub fn queue_position(&self, file: &Path, requester: &str) -> Option<usize> {
        self.wait_queues
            .get(file)
            .and_then(|queue| queue.iter().position(|e| e.requester == requester))
    }

    /// Check if requester is next in queue (for fairness).
    pub fn is_next_in_queue(&self, file: &Path, requester: &str) -> bool {
        self.wait_queues.get(file).is_some_and(|queue| {
            queue
                .front()
                .is_some_and(|first| first.requester == requester)
        })
    }
}
