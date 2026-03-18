//! Ownership tracking, module boundary enforcement, and deferred task management.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use tracing::debug;

use super::{DeferredTaskRef, FileOwnershipManager};

impl FileOwnershipManager {
    pub(super) fn get_default_owner(&self, file: &Path) -> Option<String> {
        if let Some(entry) = self.module_defaults.get(file) {
            return Some(entry.value().clone());
        }

        for entry in self.module_defaults.iter() {
            if file.starts_with(entry.key()) {
                return Some(entry.value().clone());
            }
        }

        None
    }

    pub(super) fn is_same_or_allowed_module(&self, requester: &str, owner: &str) -> bool {
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

    /// Check if multiple modules are competing for the same file.
    pub(super) fn detect_cross_module_conflict(
        &self,
        file: &Path,
        requester_module: Option<&str>,
    ) -> Option<Vec<String>> {
        let file_buf = file.to_path_buf();

        let mut involved_modules = HashSet::new();

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

        // 3+ distinct modules triggers negotiation
        if involved_modules.len() >= 3 {
            Some(involved_modules.into_iter().collect())
        } else {
            None
        }
    }

    /// Register a deferred task waiting for a file's ownership to be released.
    pub(crate) fn register_deferred_task(&self, file: &Path, task_ref: DeferredTaskRef) {
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

}
