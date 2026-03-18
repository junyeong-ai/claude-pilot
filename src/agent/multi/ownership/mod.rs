//! File ownership management for parallel agent execution.

mod lease;
mod tracking;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};



#[derive(Debug, Clone)]
pub(crate) struct DeferredTaskRef {
    pub task_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AccessType {
    Exclusive,
    SharedRead,
    Collaborative,
}

#[derive(Debug, Clone)]
pub struct FileOwnership {
    pub(crate) owner: String,
    pub(crate) access: AccessType,
    pub(crate) expires_at: Instant,
    pub(crate) reason: String,
    pub(crate) version: u64,
    pub(crate) module: Option<String>,
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

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn version(&self) -> u64 {
        self.version
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

impl AcquisitionResult {
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Denied { .. } | Self::NeedsNegotiation { .. } | Self::Queued { .. }
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WaitingEntry {
    pub requester: String,
    pub module: Option<String>,
    pub requested_at: Instant,
    pub priority: u8,
}

pub struct FileOwnershipManager {
    pub(super) ownership: DashMap<PathBuf, FileOwnership>,
    pub(super) wait_queues: DashMap<PathBuf, VecDeque<WaitingEntry>>,
    pub(super) module_defaults: DashMap<PathBuf, String>,
    pub(super) module_dependencies: DashMap<String, Vec<String>>,
    pub(super) version_counter: AtomicU64,
    pub(super) default_lease_duration: Duration,
    pub(super) deferred_tasks: DashMap<PathBuf, Vec<DeferredTaskRef>>,
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

    /// Release all leases and return deferred tasks that were waiting for these files.
    pub(crate) fn release_and_get_deferred(self) -> Vec<DeferredTaskRef> {
        let mut deferred = Vec::new();
        for lease in &self.leases {
            if !lease.released.swap(true, Ordering::SeqCst) {
                let tasks = lease.manager.release_and_get_deferred(
                    &lease.file,
                    &lease.owner,
                    lease.version,
                );
                deferred.extend(tasks);
            }
        }
        deferred
    }
}

impl Default for LeaseGuard {
    fn default() -> Self {
        Self::new()
    }
}

