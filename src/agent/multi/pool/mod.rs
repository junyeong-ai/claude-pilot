//! Agent pool for managing specialized agent instances.

mod builder;
mod operations;
mod selection;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;

use parking_lot::RwLock;
use tokio::sync::Semaphore;
use tracing::debug;

use super::ownership::FileOwnershipManager;
use super::traits::{
    AgentExecutionMetrics, AgentMetrics, AgentRole, BoxedAgent, LoadTracker, SpecializedAgent,
};
use crate::config::MultiAgentConfig;
use crate::error::{PilotError, Result};

pub use builder::AgentPoolBuilder;

#[derive(Debug, Default)]
pub(super) struct AgentRegistry {
    registered_ids: RwLock<HashSet<String>>,
}

impl AgentRegistry {
    fn new() -> Self {
        Self::default()
    }

    fn register(&self, id: &str) -> std::result::Result<(), String> {
        let mut ids = self.registered_ids.write();
        if ids.contains(id) {
            return Err(format!("Agent with ID '{}' already registered", id));
        }
        ids.insert(id.to_string());
        Ok(())
    }

    fn unregister(&self, id: &str) -> bool {
        self.registered_ids.write().remove(id)
    }

    fn contains(&self, id: &str) -> bool {
        self.registered_ids.read().contains(id)
    }

    fn count(&self) -> usize {
        self.registered_ids.read().len()
    }

    fn clear(&self) {
        self.registered_ids.write().clear();
    }
}

struct ExecutionTimer {
    start: Instant,
    metrics: Arc<AgentMetrics>,
    recorded: bool,
}

impl ExecutionTimer {
    fn new(metrics: Arc<AgentMetrics>) -> Self {
        Self {
            start: Instant::now(),
            metrics,
            recorded: false,
        }
    }

    fn finish(mut self, success: bool) {
        self.metrics.record_execution(success, self.start.elapsed());
        self.recorded = true;
    }
}

impl Drop for ExecutionTimer {
    fn drop(&mut self) {
        if !self.recorded {
            self.metrics.record_execution(false, self.start.elapsed());
        }
    }
}

/// Pool of specialized agents with load balancing.
///
/// Supports dual-index for efficient lookup:
/// - `by_pool_key`: Groups agents by role for load-balanced selection (e.g., "ws:auth:coder")
/// - `by_qualified_id`: Direct lookup by unique agent identifier (e.g., "ws:auth:coder-0")
pub struct AgentPool {
    pub(super) config: MultiAgentConfig,
    /// Primary index: agents grouped by pool_key (role) for load-balanced selection.
    /// Key format: "workspace:module:role" or "role" for unscoped agents.
    pub(super) agents: RwLock<HashMap<String, Vec<BoxedAgent>>>,
    /// Secondary index: direct lookup by qualified_id for specific agent instance.
    /// Key format: "workspace:module:role-instance" or "role-instance" for unscoped agents.
    pub(super) by_qualified_id: RwLock<HashMap<String, BoxedAgent>>,
    pub(super) counters: RwLock<HashMap<String, AtomicUsize>>,
    pub(super) registry: AgentRegistry,
    pub(super) metrics: Arc<AgentMetrics>,
    pub(super) shutdown: AtomicBool,
    pub(super) ownership_manager: Option<Arc<FileOwnershipManager>>,
    pub(super) load: LoadTracker,
    /// Semaphore for controlling concurrent task execution.
    pub(super) task_semaphore: Arc<Semaphore>,
}

impl AgentPool {
    pub fn new(config: MultiAgentConfig) -> Self {
        let max_concurrent = config.max_concurrent_agents.max(1);
        Self {
            task_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            config,
            agents: RwLock::new(HashMap::new()),
            by_qualified_id: RwLock::new(HashMap::new()),
            counters: RwLock::new(HashMap::new()),
            registry: AgentRegistry::new(),
            metrics: Arc::new(AgentMetrics::new()),
            shutdown: AtomicBool::new(false),
            ownership_manager: None,
            load: LoadTracker::new(),
        }
    }

    pub fn with_ownership_manager(mut self, manager: Arc<FileOwnershipManager>) -> Self {
        self.ownership_manager = Some(manager);
        self
    }

    /// Get the ownership manager if configured.
    pub fn ownership_manager(&self) -> Option<&Arc<FileOwnershipManager>> {
        self.ownership_manager.as_ref()
    }

    pub fn register(&self, agent: Arc<dyn SpecializedAgent>) -> Result<()> {
        if self.is_shutdown() {
            return Err(PilotError::AgentExecution("Pool is shutdown".into()));
        }

        // Extract agent info without holding locks
        let id = agent.id().to_string();
        let role_id = agent.role().id.clone();
        let identifier = agent.identifier();
        let pool_key = identifier.pool_key();
        let qualified_id = identifier.qualified_id();
        let boxed_agent = BoxedAgent::new(agent);

        // Determine if this is a namespaced agent (has workspace or module context)
        // Only add to pool_key index if:
        // 1. pool_key is different from role_id (would create duplicate otherwise)
        // 2. Agent has proper namespace context (workspace or module)
        let has_namespace = identifier.workspace.is_some() || identifier.module.is_some();
        let use_pool_key_index = has_namespace && pool_key != role_id;

        // Acquire all write locks upfront to ensure atomicity
        // This prevents partial registration if any step fails
        let mut agents = self.agents.write();
        let mut by_qualified_id = self.by_qualified_id.write();
        let mut counters = self.counters.write();

        // Register in registry (atomic operation that fails on duplicates)
        // This is done while holding locks to prevent race conditions
        self.registry.register(&id).map_err(|e| {
            // If registry fails, locks are automatically released
            PilotError::AgentExecution(e)
        })?;

        // Update primary index (by role_id for load-balanced selection)
        agents
            .entry(role_id.clone())
            .or_default()
            .push(boxed_agent.clone());

        // Also index by pool_key for namespaced agents (workspace:module:role)
        if use_pool_key_index {
            agents
                .entry(pool_key.clone())
                .or_default()
                .push(boxed_agent.clone());
        }

        // Update secondary index (by qualified_id for specific instance lookup)
        by_qualified_id.insert(qualified_id.clone(), boxed_agent);

        // Update counters
        counters
            .entry(role_id.clone())
            .or_insert_with(|| AtomicUsize::new(0));
        if use_pool_key_index {
            counters
                .entry(pool_key.clone())
                .or_insert_with(|| AtomicUsize::new(0));
        }

        debug!(
            role = %role_id,
            pool_key = %pool_key,
            qualified_id = %qualified_id,
            id = %id,
            has_namespace = %has_namespace,
            "Agent registered"
        );
        Ok(())
    }

    pub fn unregister(&self, id: &str) -> bool {
        // Acquire write locks for both indexes
        let mut agents = self.agents.write();
        let mut by_qualified_id = self.by_qualified_id.write();

        // Find and remove agent from primary index (by role_id and pool_key)
        let mut found = false;
        for pool in agents.values_mut() {
            if let Some(pos) = pool.iter().position(|a| a.id() == id) {
                pool.remove(pos);
                found = true;
                // Don't break - agent may be indexed under both role_id and pool_key
            }
        }

        // Remove from secondary index (by qualified_id)
        // Try multiple key formats for backward compatibility
        by_qualified_id.remove(id);

        // Only unregister from registry if agent was found and removed
        // This ensures consistency: if agent wasn't in pool, registry stays intact
        if found {
            // Registry unregister should succeed since we found the agent
            // If it fails, it indicates a consistency bug elsewhere
            let unregistered = self.registry.unregister(id);
            debug_assert!(unregistered, "Agent {} was in pool but not in registry", id);
            debug!(id = %id, "Agent unregistered");
            true
        } else {
            false
        }
    }

    pub fn agent_count(&self, role: &AgentRole) -> usize {
        self.agents.read().get(&role.id).map_or(0, Vec::len)
    }

    pub fn agent_count_by_id(&self, role_id: &str) -> usize {
        self.agents.read().get(role_id).map_or(0, Vec::len)
    }

    pub fn total_agents(&self) -> usize {
        self.registry.count()
    }

    pub fn has_agent(&self, id: &str) -> bool {
        self.registry.contains(id)
    }
}

#[derive(Debug, Clone)]
pub struct PoolStatistics {
    pub agent_count: usize,
    pub total_load: u32,
    pub by_role: HashMap<String, RoleStats>,
    pub metrics: AgentExecutionMetrics,
    pub is_shutdown: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RoleStats {
    pub count: usize,
    pub load: u32,
}
