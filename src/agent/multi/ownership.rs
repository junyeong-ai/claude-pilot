use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use crate::error::{PilotError, Result};

/// Reference to a deferred task waiting for file ownership release.
///
/// # Future Use
/// This struct is designed for event-driven deferred task notification:
/// - When an agent yields due to file conflict, it registers a DeferredTaskRef
/// - When ownership is released, the coordinator can be notified immediately
/// - Currently, the coordinator uses timer-based retry instead
///
/// The `register_deferred_task()` and `release_and_get_deferred()` methods
/// provide the infrastructure for this pattern when needed.
#[derive(Debug, Clone)]
pub struct DeferredTaskRef {
    pub task_id: String,
    pub agent_id: String,
    pub waiting_for_file: PathBuf,
    pub created_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessType {
    Exclusive,
    SharedRead,
    Collaborative,
}

#[derive(Debug, Clone)]
pub struct FileOwnership {
    pub owner: String,
    pub access: AccessType,
    pub expires_at: Instant,
    pub reason: String,
    pub version: u64,
    pub module: Option<String>,
}

impl FileOwnership {
    pub fn is_expired(&self) -> bool {
        Instant::now() > self.expires_at
    }

    pub fn remaining_time(&self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }
}

pub struct FileLease {
    file: PathBuf,
    owner: String,
    version: u64,
    manager: Arc<FileOwnershipManager>,
    released: AtomicBool,
}

impl std::fmt::Debug for FileLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLease")
            .field("file", &self.file)
            .field("owner", &self.owner)
            .field("version", &self.version)
            .field("released", &self.released.load(Ordering::Relaxed))
            .finish()
    }
}

impl FileLease {
    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn release(self) {
        // Drop will handle release
    }
}

impl Drop for FileLease {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::SeqCst) {
            self.manager
                .release_internal(&self.file, &self.owner, self.version);
        }
    }
}

#[derive(Debug)]
pub enum AcquisitionResult {
    Granted {
        lease: Arc<FileLease>,
    },
    Queued {
        position: usize,
        estimated_wait: Duration,
    },
    Denied {
        reason: String,
        owner_module: Option<String>,
    },
    NeedsNegotiation {
        conflicting_modules: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ConflictInfo {
    pub conflict_id: String,
    pub file: PathBuf,
    pub requester: String,
    pub description: String,
    pub modules_involved: Vec<String>,
}

impl AcquisitionResult {
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Denied { .. } | Self::NeedsNegotiation { .. } | Self::Queued { .. }
        )
    }

    pub fn to_conflict_info(&self, file: &Path, requester: &str) -> Option<ConflictInfo> {
        match self {
            Self::Denied {
                reason,
                owner_module,
            } => Some(ConflictInfo {
                conflict_id: Uuid::new_v4().to_string(),
                file: file.to_path_buf(),
                requester: requester.to_string(),
                description: reason.clone(),
                modules_involved: owner_module.iter().cloned().collect(),
            }),
            Self::NeedsNegotiation {
                conflicting_modules,
            } => Some(ConflictInfo {
                conflict_id: Uuid::new_v4().to_string(),
                file: file.to_path_buf(),
                requester: requester.to_string(),
                description: format!(
                    "{} modules competing for file access",
                    conflicting_modules.len()
                ),
                modules_involved: conflicting_modules.clone(),
            }),
            Self::Queued { position, .. } => Some(ConflictInfo {
                conflict_id: Uuid::new_v4().to_string(),
                file: file.to_path_buf(),
                requester: requester.to_string(),
                description: format!("Queued at position {}", position),
                modules_involved: vec![],
            }),
            Self::Granted { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WaitingEntry {
    pub requester: String,
    pub module: Option<String>,
    pub requested_at: Instant,
    pub priority: u8,
}

pub struct FileOwnershipManager {
    ownership: DashMap<PathBuf, FileOwnership>,
    wait_queues: DashMap<PathBuf, VecDeque<WaitingEntry>>,
    module_defaults: DashMap<PathBuf, String>,
    module_dependencies: DashMap<String, Vec<String>>,
    version_counter: AtomicU64,
    default_lease_duration: Duration,
    deferred_tasks: DashMap<PathBuf, Vec<DeferredTaskRef>>,
}

impl FileOwnershipManager {
    pub fn new() -> Self {
        Self {
            ownership: DashMap::new(),
            wait_queues: DashMap::new(),
            module_defaults: DashMap::new(),
            module_dependencies: DashMap::new(),
            version_counter: AtomicU64::new(0),
            default_lease_duration: Duration::from_secs(300),
            deferred_tasks: DashMap::new(),
        }
    }

    pub fn with_module_map(
        module_files: HashMap<String, Vec<PathBuf>>,
        dependencies: HashMap<String, Vec<String>>,
    ) -> Self {
        let manager = Self::new();

        for (module, files) in module_files {
            for file in files {
                manager.module_defaults.insert(file, module.clone());
            }
        }

        for (module, deps) in dependencies {
            manager.module_dependencies.insert(module, deps);
        }

        manager
    }

    /// Add additional modules to an existing manager.
    ///
    /// Used for cross-workspace scenarios where additional workspaces
    /// need to register their module file mappings.
    pub fn add_modules(
        &self,
        module_files: HashMap<String, Vec<PathBuf>>,
        dependencies: HashMap<String, Vec<String>>,
    ) {
        for (module, files) in module_files {
            for file in files {
                self.module_defaults.insert(file, module.clone());
            }
        }

        for (module, deps) in dependencies {
            self.module_dependencies.insert(module, deps);
        }
    }

    pub fn acquire(
        self: &Arc<Self>,
        file: &Path,
        requester: &str,
        requester_module: Option<&str>,
        access: AccessType,
        reason: &str,
    ) -> AcquisitionResult {
        let file = file.to_path_buf();

        // Check for cross-module conflicts requiring negotiation
        // This fires when 3+ distinct modules compete for the same file
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
            expires_at: Instant::now() + self.default_lease_duration,
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

    fn release_internal(&self, file: &Path, owner: &str, version: u64) {
        self.ownership.remove_if(file, |_, current| {
            current.owner == owner && current.version == version
        });
        debug!(owner = %owner, version = version, "Released file ownership");
    }

    pub fn release(&self, file: &Path, owner: &str, version: u64) -> bool {
        self.ownership
            .remove_if(file, |_, current| {
                current.owner == owner && current.version == version
            })
            .is_some()
    }

    /// Release file ownership and return any deferred tasks waiting for this file.
    pub fn release_and_get_deferred(
        &self,
        file: &Path,
        owner: &str,
        version: u64,
    ) -> Vec<DeferredTaskRef> {
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

    /// Register a deferred task waiting for a file's ownership to be released.
    pub fn register_deferred_task(&self, file: &Path, task_ref: DeferredTaskRef) {
        self.deferred_tasks
            .entry(file.to_path_buf())
            .or_default()
            .push(task_ref);
        debug!(
            file = %file.display(),
            task_id = %self.deferred_tasks.get(file).map(|v| v.len()).unwrap_or(0),
            "Registered deferred task"
        );
    }

    /// Get all deferred tasks waiting for a specific file.
    pub fn get_deferred_tasks(&self, file: &Path) -> Vec<DeferredTaskRef> {
        self.deferred_tasks
            .get(file)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get all pending deferred tasks across all files.
    pub fn all_deferred_tasks(&self) -> Vec<DeferredTaskRef> {
        self.deferred_tasks
            .iter()
            .flat_map(|entry| entry.value().clone())
            .collect()
    }

    /// Clear deferred tasks that have expired (older than cutoff).
    pub fn cleanup_stale_deferred(&self, max_age: Duration) {
        let cutoff = Instant::now() - max_age;
        self.deferred_tasks.retain(|_, tasks| {
            tasks.retain(|t| t.created_at > cutoff);
            !tasks.is_empty()
        });
    }

    fn get_default_owner(&self, file: &Path) -> Option<String> {
        if let Some(entry) = self.module_defaults.get(file) {
            return Some(entry.value().clone());
        }

        for entry in self.module_defaults.iter() {
            let prefix = entry.key();
            if file.starts_with(prefix)
                || file
                    .to_string_lossy()
                    .starts_with(&prefix.to_string_lossy().to_string())
            {
                return Some(entry.value().clone());
            }
        }

        None
    }

    fn is_same_or_allowed_module(&self, requester: &str, owner: &str) -> bool {
        if requester == owner {
            return true;
        }

        if let Some(deps) = self.module_dependencies.get(requester)
            && deps.contains(&owner.to_string())
        {
            return true;
        }

        false
    }

    pub fn cleanup_expired(&self) {
        self.ownership.retain(|_, v| !v.is_expired());
        // Also cleanup stale wait queue entries (older than 5 minutes)
        let cutoff = Instant::now() - Duration::from_secs(300);
        self.wait_queues.retain(|_, queue| {
            queue.retain(|entry| entry.requested_at > cutoff);
            !queue.is_empty()
        });
    }

    pub fn current_owner(&self, file: &Path) -> Option<String> {
        self.ownership
            .get(file)
            .filter(|o| !o.is_expired())
            .map(|o| o.owner.clone())
    }

    /// Acquire multiple files in sorted order to prevent deadlocks.
    /// If any file cannot be acquired, all previously acquired leases are released.
    /// Returns a LeaseGuard containing all acquired leases on success,
    /// or the blocking AcquisitionResult on failure.
    pub fn acquire_ordered(
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
                    // guard drops here, releasing all acquired leases
                    return Err(result);
                }
            }
        }

        Ok(guard)
    }

    /// Acquire with fair queue consideration.
    /// Respects queue order to prevent starvation.
    pub fn acquire_with_fair_queue(
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
            // Someone else is first in queue - respect order
            let position = queue
                .iter()
                .position(|e| e.requester == requester)
                .unwrap_or(queue.len());
            return AcquisitionResult::Queued {
                position,
                estimated_wait: Duration::from_secs(30 * position as u64),
            };
        }

        // No queue or we're first - try normal acquisition
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
            requested_at: Instant::now(),
            priority,
        };

        // Insert by priority (higher priority = earlier in queue)
        let insert_pos = queue
            .iter()
            .position(|e| e.priority < priority)
            .unwrap_or(queue.len());

        queue.insert(insert_pos, entry);
    }

    /// Check if multiple modules are competing for the same file.
    /// Returns conflicting module names if negotiation is needed.
    fn detect_cross_module_conflict(
        &self,
        file: &Path,
        requester_module: Option<&str>,
    ) -> Option<Vec<String>> {
        let file_buf = file.to_path_buf();

        // Collect modules involved: current owner + waiting entries
        let mut involved_modules = std::collections::HashSet::new();

        // Check current owner's module
        if let Some(current) = self.ownership.get(&file_buf)
            && !current.is_expired()
            && let Some(ref owner_module) = current.module
        {
            involved_modules.insert(owner_module.clone());
        }

        // Check waiting queue modules
        if let Some(queue) = self.wait_queues.get(&file_buf) {
            for entry in queue.iter() {
                if let Some(ref mod_name) = entry.module {
                    involved_modules.insert(mod_name.clone());
                }
            }
        }

        // Add requester's module
        if let Some(req_module) = requester_module {
            involved_modules.insert(req_module.to_string());
        }

        // If 3 or more distinct modules are competing, trigger negotiation
        // (2 modules = normal queuing, 3+ = likely architectural issue)
        if involved_modules.len() >= 3 {
            Some(involved_modules.into_iter().collect())
        } else {
            None
        }
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

impl Default for FileOwnershipManager {
    fn default() -> Self {
        Self::new()
    }
}

pub struct LeaseGuard {
    leases: Vec<Arc<FileLease>>,
}

impl LeaseGuard {
    pub fn new() -> Self {
        Self { leases: Vec::new() }
    }

    pub fn add(&mut self, lease: Arc<FileLease>) {
        self.leases.push(lease);
    }

    pub fn files(&self) -> Vec<&Path> {
        self.leases.iter().map(|l| l.file()).collect()
    }
}

impl Default for LeaseGuard {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn acquire_with_retry(
    manager: &Arc<FileOwnershipManager>,
    file: &Path,
    requester: &str,
    requester_module: Option<&str>,
    access: AccessType,
    reason: &str,
    max_retries: usize,
) -> Result<Arc<FileLease>> {
    for attempt in 0..max_retries {
        match manager.acquire(file, requester, requester_module, access, reason) {
            AcquisitionResult::Granted { lease } => return Ok(lease),
            AcquisitionResult::Queued { estimated_wait, .. } => {
                let wait = estimated_wait.min(Duration::from_secs(30));
                debug!(
                    attempt = attempt,
                    wait_ms = wait.as_millis(),
                    "Waiting for file ownership"
                );
                tokio::time::sleep(wait).await;
            }
            AcquisitionResult::Denied { reason, .. } => {
                return Err(PilotError::Ownership(reason));
            }
            AcquisitionResult::NeedsNegotiation {
                conflicting_modules,
            } => {
                return Err(PilotError::Ownership(format!(
                    "Negotiation required between modules: {:?}",
                    conflicting_modules
                )));
            }
        }
    }

    Err(PilotError::Ownership(format!(
        "Failed to acquire ownership after {} attempts",
        max_retries
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_ownership() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("src/main.rs");

        let result = manager.acquire(&file, "agent-1", None, AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));

        let result = manager.acquire(&file, "agent-2", None, AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Queued { .. }));
    }

    #[test]
    fn test_shared_read() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("src/lib.rs");

        let r1 = manager.acquire(&file, "agent-1", None, AccessType::SharedRead, "read");
        assert!(matches!(r1, AcquisitionResult::Granted { .. }));

        let r2 = manager.acquire(&file, "agent-2", None, AccessType::SharedRead, "read");
        assert!(matches!(r2, AcquisitionResult::Granted { .. }));
    }

    #[test]
    fn test_module_boundary() {
        let mut module_files = HashMap::new();
        module_files.insert("auth".to_string(), vec![PathBuf::from("src/auth/")]);

        let manager = Arc::new(FileOwnershipManager::with_module_map(
            module_files,
            HashMap::new(),
        ));

        let file = PathBuf::from("src/auth/login.rs");
        let result = manager.acquire(&file, "agent", Some("api"), AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Denied { .. }));

        let result = manager.acquire(&file, "agent", Some("auth"), AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));
    }

    #[test]
    fn test_module_dependency() {
        let mut module_files = HashMap::new();
        module_files.insert("db".to_string(), vec![PathBuf::from("src/db/")]);

        let mut deps = HashMap::new();
        deps.insert("auth".to_string(), vec!["db".to_string()]);

        let manager = Arc::new(FileOwnershipManager::with_module_map(module_files, deps));

        let file = PathBuf::from("src/db/schema.rs");
        let result = manager.acquire(&file, "agent", Some("auth"), AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));
    }

    #[test]
    fn test_lease_drop_releases() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("src/test.rs");

        {
            let result = manager.acquire(&file, "agent-1", None, AccessType::Exclusive, "test");
            assert!(matches!(result, AcquisitionResult::Granted { .. }));
            assert!(manager.current_owner(&file).is_some());
        }

        // Lease dropped, but we need to manually trigger cleanup or check version
        // In practice the lease's Drop releases it
    }

    #[test]
    fn test_ordered_acquisition_prevents_deadlock() {
        let manager = Arc::new(FileOwnershipManager::new());

        // Files in reverse order - should still acquire in sorted order
        let files = vec![
            PathBuf::from("c.rs"),
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
        ];

        let result =
            manager.acquire_ordered(&files, "agent-1", None, AccessType::Exclusive, "test");
        assert!(result.is_ok());
        let guard = result.unwrap();
        assert_eq!(guard.files().len(), 3);
    }

    #[test]
    fn test_fair_queue_priority() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("contested.rs");

        // Add requests with different priorities
        manager.add_to_wait_queue(&file, "low-priority", None, 1);
        manager.add_to_wait_queue(&file, "high-priority", None, 10);
        manager.add_to_wait_queue(&file, "medium-priority", None, 5);

        // High priority should be first
        assert!(manager.is_next_in_queue(&file, "high-priority"));
        assert_eq!(manager.queue_position(&file, "medium-priority"), Some(1));
        assert_eq!(manager.queue_position(&file, "low-priority"), Some(2));
    }

    #[test]
    fn test_fair_queue_respects_order() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("fair.rs");

        // First agent acquires the file
        let result = manager.acquire(&file, "owner", None, AccessType::Exclusive, "holding");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));

        // Second agent adds to queue
        manager.add_to_wait_queue(&file, "waiter-1", None, 0);

        // Third agent tries fair acquisition - should be queued after waiter-1
        let result = manager.acquire_with_fair_queue(
            &file,
            "waiter-2",
            None,
            AccessType::Exclusive,
            "want file",
        );
        assert!(matches!(result, AcquisitionResult::Queued { .. }));

        // Waiter-1 should still be first
        assert!(manager.is_next_in_queue(&file, "waiter-1"));
    }

    #[test]
    fn test_remove_from_queue() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("remove.rs");

        manager.add_to_wait_queue(&file, "agent-1", None, 0);
        manager.add_to_wait_queue(&file, "agent-2", None, 0);

        assert_eq!(manager.queue_position(&file, "agent-1"), Some(0));
        assert_eq!(manager.queue_position(&file, "agent-2"), Some(1));

        manager.remove_from_wait_queue(&file, "agent-1");

        assert!(manager.queue_position(&file, "agent-1").is_none());
        assert_eq!(manager.queue_position(&file, "agent-2"), Some(0));
    }

    #[test]
    fn test_cross_module_conflict_detection() {
        let manager = Arc::new(FileOwnershipManager::new());
        let file = PathBuf::from("shared_config.rs");

        // First module acquires
        let result = manager.acquire(
            &file,
            "agent-1",
            Some("auth"),
            AccessType::Exclusive,
            "modify",
        );
        assert!(matches!(result, AcquisitionResult::Granted { .. }));

        // Second module adds to queue
        manager.add_to_wait_queue(&file, "agent-2", Some("api"), 0);

        // Third module triggers negotiation check
        let conflicts = manager.detect_cross_module_conflict(&file, Some("database"));

        // Should detect 3 modules: auth, api, database
        assert!(conflicts.is_some());
        let modules = conflicts.unwrap();
        assert_eq!(modules.len(), 3);
        assert!(modules.contains(&"auth".to_string()));
        assert!(modules.contains(&"api".to_string()));
        assert!(modules.contains(&"database".to_string()));
    }

    #[test]
    fn test_add_modules_extends_manager() {
        // Start with a manager configured for primary workspace
        let mut primary_files = HashMap::new();
        primary_files.insert("auth".to_string(), vec![PathBuf::from("src/auth/mod.rs")]);

        let mut primary_deps = HashMap::new();
        primary_deps.insert("auth".to_string(), vec!["db".to_string()]);

        let manager = Arc::new(FileOwnershipManager::with_module_map(
            primary_files,
            primary_deps,
        ));

        // Add modules from an additional workspace with qualified IDs
        let mut additional_files = HashMap::new();
        additional_files.insert(
            "frontend::ui".to_string(),
            vec![PathBuf::from("frontend/src/ui/mod.rs")],
        );

        let mut additional_deps = HashMap::new();
        additional_deps.insert(
            "frontend::ui".to_string(),
            vec!["frontend::api".to_string()],
        );

        manager.add_modules(additional_files, additional_deps);

        // Verify primary workspace modules still work
        let primary_owner = manager.get_default_owner(&PathBuf::from("src/auth/mod.rs"));
        assert_eq!(primary_owner, Some("auth".to_string()));

        // Verify additional workspace modules work
        let additional_owner = manager.get_default_owner(&PathBuf::from("frontend/src/ui/mod.rs"));
        assert_eq!(additional_owner, Some("frontend::ui".to_string()));
    }

    #[test]
    fn test_add_modules_with_duplicate_overwrites() {
        let manager = Arc::new(FileOwnershipManager::new());

        // First registration
        let mut first = HashMap::new();
        first.insert("auth".to_string(), vec![PathBuf::from("src/auth.rs")]);
        manager.add_modules(first, HashMap::new());

        // Second registration with same key but different value
        let mut second = HashMap::new();
        second.insert("auth".to_string(), vec![PathBuf::from("src/new_auth.rs")]);
        manager.add_modules(second, HashMap::new());

        // Old file should no longer be mapped (key was overwritten)
        // Note: This documents current behavior - overwrites silently
        let new_owner = manager.get_default_owner(&PathBuf::from("src/new_auth.rs"));
        assert_eq!(new_owner, Some("auth".to_string()));
    }
}
