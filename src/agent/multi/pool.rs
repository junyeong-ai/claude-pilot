//! Agent pool for managing specialized agent instances.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use futures::future::join_all;
use parking_lot::RwLock;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::identity::AgentIdentifier;
use super::messaging::AgentMessageBus;
use super::ownership::FileOwnershipManager;
use super::shared::AgentId;
use super::traits::{
    AgentMetrics, AgentRole, AgentTask, AgentTaskResult, BoxedAgent, LoadTracker, MetricsSnapshot,
    SpecializedAgent,
};
use crate::config::{MultiAgentConfig, SelectionPolicy};
use crate::error::{PilotError, Result};

#[derive(Debug, Default)]
struct AgentRegistry {
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

/// Stride value for pseudo-random agent selection.
///
/// This prime number (7) is used to create pseudo-random distribution when selecting
/// agents from a pool without the overhead of a proper RNG. By incrementing a counter
/// by this stride and taking modulo of the pool size, we get good distribution across
/// agents while avoiding the patterns that would emerge from sequential (stride=1)
/// selection.
///
/// A prime number works well because it's coprime to most pool sizes, ensuring we cycle
/// through all agents before repeating. For example:
/// - Pool of 3 agents: visits indices [0,1,2,0,1,2,...] (7 mod 3 = 1, so cycles through all)
/// - Pool of 5 agents: visits indices [0,2,4,1,3,0,...] (7 mod 5 = 2, good distribution)
/// - Pool of 10 agents: visits indices [0,7,4,1,8,5,2,9,6,3,0,...] (visits all before repeating)
const RANDOM_SELECTION_STRIDE: usize = 7;

/// Pool of specialized agents with load balancing.
///
/// Supports dual-index for efficient lookup:
/// - `by_pool_key`: Groups agents by role for load-balanced selection (e.g., "ws:auth:coder")
/// - `by_qualified_id`: Direct lookup by unique agent identifier (e.g., "ws:auth:coder-0")
pub struct AgentPool {
    config: MultiAgentConfig,
    /// Primary index: agents grouped by pool_key (role) for load-balanced selection.
    /// Key format: "workspace:module:role" or "role" for unscoped agents.
    agents: RwLock<HashMap<String, Vec<BoxedAgent>>>,
    /// Secondary index: direct lookup by qualified_id for specific agent instance.
    /// Key format: "workspace:module:role-instance" or "role-instance" for unscoped agents.
    by_qualified_id: RwLock<HashMap<String, BoxedAgent>>,
    counters: RwLock<HashMap<String, AtomicUsize>>,
    registry: AgentRegistry,
    metrics: Arc<AgentMetrics>,
    shutdown: AtomicBool,
    ownership_manager: Option<Arc<FileOwnershipManager>>,
    load: LoadTracker,
    /// Semaphore for controlling concurrent task execution.
    task_semaphore: Arc<Semaphore>,
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

    pub fn select(&self, role: &AgentRole) -> Option<BoxedAgent> {
        self.select_by_id(&role.id)
    }

    pub fn select_by_id(&self, role_id: impl AsRef<str>) -> Option<BoxedAgent> {
        let role_id = role_id.as_ref();
        let agents = self.agents.read();
        let pool = agents.get(role_id)?;

        let pool_len = pool.len();
        if pool_len == 0 {
            return None;
        }

        match self.config.selection_policy {
            SelectionPolicy::RoundRobin => {
                let counters = self.counters.read();
                let idx = counters
                    .get(role_id)
                    .map_or(0, |c| c.fetch_add(1, Ordering::Relaxed));
                pool.get(idx % pool_len).cloned()
            }
            SelectionPolicy::LeastLoaded => pool.iter().min_by_key(|a| a.current_load()).cloned(),
            SelectionPolicy::Random => {
                let counters = self.counters.read();
                let idx = counters.get(role_id).map_or(0, |c| {
                    c.fetch_add(RANDOM_SELECTION_STRIDE, Ordering::Relaxed)
                });
                pool.get(idx % pool_len).cloned()
            }
            SelectionPolicy::Scored => {
                let metrics = self.metrics.snapshot();
                let max_load = self.config.max_agent_load;
                pool.iter()
                    .map(|agent| {
                        let score = AgentScore::from_agent(agent, &metrics, max_load);
                        (agent, score.total())
                    })
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(agent, _)| agent.clone())
            }
        }
    }

    /// Select a specific agent instance by its qualified ID with fallback.
    ///
    /// Qualified ID format: "workspace:module:role-instance" or "role-instance"
    /// This is used for consensus protocols that need specific agent instances.
    ///
    /// The lookup follows this order:
    /// 1. Exact match on the qualified ID
    /// 2. Role-only match (strips workspace/module prefix for cross-workspace scenarios)
    ///
    /// # Arguments
    /// * `qualified_id` - The full qualified identifier (e.g., "ws-a:auth:coder-0")
    ///
    /// # Returns
    /// * `Some(BoxedAgent)` if the agent exists (exact or fallback match)
    /// * `None` if no matching agent is registered
    pub fn select_by_qualified_id(&self, qualified_id: &str) -> Option<BoxedAgent> {
        if qualified_id.is_empty() {
            return None;
        }

        let by_qid = self.by_qualified_id.read();

        // 1. Exact match
        if let Some(agent) = by_qid.get(qualified_id) {
            return Some(agent.clone());
        }

        // 2. Fallback: try role-only match (last segment after ':')
        // e.g., "ws-a:auth:coder-0" -> "coder-0"
        let role_part = qualified_id.rsplit(':').next()?;
        if role_part.is_empty() {
            return None;
        }

        // Collect and sort candidates for deterministic selection
        // Priority: exact role match > shorter ID > alphabetically first
        let mut candidates: Vec<_> = by_qid
            .iter()
            .filter(|(id, _)| *id == role_part || id.ends_with(&format!(":{}", role_part)))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by: exact match first, then by ID length, then alphabetically
        candidates.sort_by(|(id_a, _), (id_b, _)| {
            let exact_a = *id_a == role_part;
            let exact_b = *id_b == role_part;
            match (exact_a, exact_b) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => id_a.len().cmp(&id_b.len()).then_with(|| id_a.cmp(id_b)),
            }
        });

        let (matched_id, agent) = candidates[0];
        debug!(
            qualified_id = %qualified_id,
            matched_id = %matched_id,
            candidates_count = candidates.len(),
            "Fallback match for cross-workspace agent selection"
        );
        Some(agent.clone())
    }

    /// Select a specific agent instance by its AgentIdentifier.
    ///
    /// This is the type-safe version of `select_by_qualified_id`.
    pub fn select_by_identifier(&self, identifier: &AgentIdentifier) -> Option<BoxedAgent> {
        self.select_by_qualified_id(&identifier.qualified_id())
    }

    /// Get all agent instances for a given pool key.
    ///
    /// Pool key format: "workspace:module:role" or "role"
    /// This is used for multi-instance consensus where all instances of a role
    /// participate in discussion.
    ///
    /// # Arguments
    /// * `pool_key` - The pool key (e.g., "ws-a:auth:planning" or "planning")
    ///
    /// # Returns
    /// * Vec of all agent instances for that pool key
    pub fn select_instances(&self, pool_key: &str) -> Vec<BoxedAgent> {
        self.agents
            .read()
            .get(pool_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all agent instances for a given AgentIdentifier's pool key.
    ///
    /// This returns all instances that share the same workspace:module:role.
    pub fn select_instances_for(&self, identifier: &AgentIdentifier) -> Vec<BoxedAgent> {
        self.select_instances(&identifier.pool_key())
    }

    /// Get all agent identifiers for a given pool key.
    ///
    /// Returns the qualified IDs of all agent instances for the pool key.
    pub fn identifiers_for_pool(&self, pool_key: &str) -> Vec<AgentIdentifier> {
        self.agents
            .read()
            .get(pool_key)
            .map(|agents| agents.iter().map(|a| a.identifier()).collect())
            .unwrap_or_default()
    }

    /// Get all registered role IDs.
    pub fn role_ids(&self) -> Vec<String> {
        self.agents.read().keys().cloned().collect()
    }

    /// Get role IDs for planning-relevant agents only (excludes QA, verifier, coder).
    pub fn planning_role_ids(&self) -> Vec<String> {
        self.agents
            .read()
            .iter()
            .filter(|(_, agents)| {
                agents
                    .first()
                    .is_some_and(|a| a.role().is_planning_relevant())
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Get all agents for a given role ID.
    pub fn select_all_by_id(&self, role_id: &str) -> Vec<BoxedAgent> {
        self.agents.read().get(role_id).cloned().unwrap_or_default()
    }

    /// Get all agents for a given role.
    pub fn select_all(&self, role: &AgentRole) -> Vec<BoxedAgent> {
        self.select_all_by_id(&role.id)
    }

    /// Get all agent IDs for a given role.
    pub fn agent_ids_for(&self, role: &AgentRole) -> Vec<AgentId> {
        self.agent_ids_for_key(&role.id)
    }

    /// Get all agent IDs for a given pool key (e.g., "module-auth", "planning").
    ///
    /// This supports multi-instance agent lookup for same-role consensus.
    pub fn agent_ids_for_key(&self, pool_key: &str) -> Vec<AgentId> {
        self.agents
            .read()
            .get(pool_key)
            .map(|agents| agents.iter().map(|a| AgentId::new(a.id())).collect())
            .unwrap_or_default()
    }

    pub async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        self.execute_internal(task, working_dir, None).await
    }

    /// Execute task with P2P conflict resolution via MessageBus.
    ///
    /// Uses the agent's execute_with_messaging method for conflict-aware execution.
    /// Falls back to direct execute if the agent doesn't support messaging.
    pub async fn execute_with_messaging(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        bus: &AgentMessageBus,
    ) -> Result<AgentTaskResult> {
        self.execute_internal(task, working_dir, Some(bus)).await
    }

    /// Internal execution with optional MessageBus for P2P conflict resolution.
    async fn execute_internal(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        bus: Option<&AgentMessageBus>,
    ) -> Result<AgentTaskResult> {
        if self.is_shutdown() {
            return Err(PilotError::AgentExecution("Pool is shutdown".into()));
        }

        let role = self.determine_role(task);

        // Try primary role, fallback to core_coder for module roles
        let (agent, actual_role) = match self.select(&role) {
            Some(a) => (a, role.clone()),
            None if role.is_module() => {
                let fallback = AgentRole::core_coder();
                match self.select(&fallback) {
                    Some(a) => {
                        warn!(
                            requested_role = %role,
                            fallback_role = %fallback,
                            task_id = %task.id,
                            "Module agent unavailable, falling back to core coder"
                        );
                        (a, fallback)
                    }
                    None => {
                        return Err(PilotError::AgentExecution(format!(
                            "No agent available for role {} or fallback",
                            role
                        )));
                    }
                }
            }
            None => {
                return Err(PilotError::AgentExecution(format!(
                    "No agent available for role: {}",
                    role
                )));
            }
        };

        if bus.is_some() {
            info!(role = %actual_role, task_id = %task.id, "Executing task with P2P messaging");
        } else {
            info!(role = %actual_role, task_id = %task.id, "Executing task");
        }

        // Track pool-level load
        self.load.increment();

        // Check shutdown status again before starting execution
        // This prevents new executions if shutdown was called during agent selection
        if self.is_shutdown() {
            self.load.decrement();
            return Err(PilotError::AgentExecution("Pool is shutdown".into()));
        }

        let timer = ExecutionTimer::new(Arc::clone(&self.metrics));

        // Execute with or without message bus
        let result = if let Some(bus) = bus {
            agent.execute_with_messaging(task, working_dir, bus).await
        } else {
            agent.execute(task, working_dir).await
        };

        // CRITICAL: Record metrics immediately after getting the result,
        // before any potential panic in result processing.
        let success = result.as_ref().map(|r| r.success).unwrap_or(false);
        timer.finish(success);

        self.load.decrement();

        result
    }

    pub async fn execute_many(
        &self,
        tasks: Vec<AgentTask>,
        working_dir: &Path,
    ) -> Vec<Result<AgentTaskResult>> {
        if !self.config.parallel_execution {
            let mut results = Vec::new();
            for task in tasks {
                results.push(self.execute(&task, working_dir).await);
            }
            return results;
        }

        // Use semaphore for efficient concurrency control.
        // Unlike chunking, this starts a new task immediately when any previous one completes.
        let semaphore = Arc::clone(&self.task_semaphore);
        let futures: Vec<_> = tasks
            .iter()
            .map(|task| {
                let sem = Arc::clone(&semaphore);
                async move {
                    let _permit = sem.acquire().await.expect("semaphore closed");
                    self.execute(task, working_dir).await
                }
            })
            .collect();

        join_all(futures).await
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        info!("Agent pool shutdown initiated");
        self.registry.clear();
    }

    pub fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Get current pool-level load (number of tasks currently executing).
    pub fn current_load(&self) -> u32 {
        self.load.current()
    }

    pub fn statistics(&self) -> PoolStatistics {
        let agents = self.agents.read();
        let mut by_role = HashMap::new();
        let mut total_load = 0u32;

        for (role_id, pool) in agents.iter() {
            let count = pool.len();
            let load: u32 = pool.iter().map(|a| a.current_load()).sum();
            by_role.insert(role_id.clone(), RoleStats { count, load });
            total_load += load;
        }

        PoolStatistics {
            agent_count: self.registry.count(),
            total_load,
            by_role,
            metrics: self.metrics.snapshot(),
            is_shutdown: self.is_shutdown(),
        }
    }

    fn determine_role(&self, task: &AgentTask) -> AgentRole {
        task.role.clone().unwrap_or_else(AgentRole::core_coder)
    }
}

#[derive(Debug, Clone)]
pub struct PoolStatistics {
    pub agent_count: usize,
    pub total_load: u32,
    pub by_role: HashMap<String, RoleStats>,
    pub metrics: MetricsSnapshot,
    pub is_shutdown: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RoleStats {
    pub count: usize,
    pub load: u32,
}

/// Weighted score for agent selection.
/// Weights: capability 0.30, load 0.20, performance 0.25, health 0.15, availability 0.10
#[derive(Debug, Clone, Copy)]
pub struct AgentScore {
    pub capability: f64,
    pub load: f64,
    pub performance: f64,
    pub health: f64,
    pub availability: f64,
}

impl AgentScore {
    pub const CAPABILITY_WEIGHT: f64 = 0.30;
    pub const LOAD_WEIGHT: f64 = 0.20;
    pub const PERFORMANCE_WEIGHT: f64 = 0.25;
    pub const HEALTH_WEIGHT: f64 = 0.15;
    pub const AVAILABILITY_WEIGHT: f64 = 0.10;

    pub fn total(&self) -> f64 {
        self.capability * Self::CAPABILITY_WEIGHT
            + self.load * Self::LOAD_WEIGHT
            + self.performance * Self::PERFORMANCE_WEIGHT
            + self.health * Self::HEALTH_WEIGHT
            + self.availability * Self::AVAILABILITY_WEIGHT
    }

    pub fn from_agent(agent: &BoxedAgent, metrics: &MetricsSnapshot, max_load: u32) -> Self {
        let current_load = agent.current_load();

        let load_score = 1.0 - (current_load as f64 / max_load as f64).min(1.0);
        let availability_score = if current_load < max_load { 1.0 } else { 0.0 };

        let performance_score = if metrics.total_executions > 0 {
            metrics.success_rate.min(1.0)
        } else {
            0.8 // Default for new agents
        };

        let health_score = if metrics.avg_duration_ms < 30000 {
            1.0
        } else if metrics.avg_duration_ms < 60000 {
            0.7
        } else {
            0.4
        };

        Self {
            capability: 1.0, // Module matching handled separately
            load: load_score,
            performance: performance_score,
            health: health_score,
            availability: availability_score,
        }
    }
}

/// Builder for AgentPool with agent registration.
pub struct AgentPoolBuilder {
    config: MultiAgentConfig,
    agents: Vec<Arc<dyn SpecializedAgent>>,
    ownership_manager: Option<Arc<FileOwnershipManager>>,
}

impl AgentPoolBuilder {
    pub fn new(config: MultiAgentConfig) -> Self {
        Self {
            config,
            agents: Vec::new(),
            ownership_manager: None,
        }
    }

    pub fn with_agent(mut self, agent: Arc<dyn SpecializedAgent>) -> Self {
        self.agents.push(agent);
        self
    }

    pub fn with_agents(mut self, agents: Vec<Arc<dyn SpecializedAgent>>) -> Self {
        self.agents.extend(agents);
        self
    }

    pub fn with_ownership_manager(mut self, manager: Arc<FileOwnershipManager>) -> Self {
        self.ownership_manager = Some(manager);
        self
    }

    pub fn build(self) -> Result<AgentPool> {
        let mut pool = AgentPool::new(self.config);
        if let Some(manager) = self.ownership_manager {
            pool = pool.with_ownership_manager(manager);
        }
        for agent in self.agents {
            pool.register(agent)?;
        }
        Ok(pool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::traits::{LoadTracker, TaskContext, TaskPriority};

    struct MockAgent {
        id: String,
        role: AgentRole,
        load: LoadTracker,
    }

    impl MockAgent {
        fn new(id: &str, role: AgentRole) -> Self {
            Self {
                id: id.to_string(),
                role,
                load: LoadTracker::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl SpecializedAgent for MockAgent {
        fn role(&self) -> &AgentRole {
            &self.role
        }

        fn id(&self) -> &str {
            &self.id
        }

        fn system_prompt(&self) -> &str {
            "Mock agent"
        }

        async fn execute(&self, task: &AgentTask, _working_dir: &Path) -> Result<AgentTaskResult> {
            self.load.increment();
            let result = AgentTaskResult::success(&task.id, format!("Executed by {}", self.id));
            self.load.decrement();
            Ok(result)
        }

        fn current_load(&self) -> u32 {
            self.load.current()
        }
    }

    #[test]
    fn test_pool_registration() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        let agent = Arc::new(MockAgent::new("test-1", AgentRole::core_coder()));
        pool.register(agent).unwrap();

        assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);
        assert_eq!(pool.total_agents(), 1);
    }

    #[test]
    fn test_duplicate_registration_fails() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new("dup-1", AgentRole::core_coder())))
            .unwrap();
        let result = pool.register(Arc::new(MockAgent::new("dup-1", AgentRole::core_coder())));
        assert!(result.is_err());
    }

    #[test]
    fn test_agent_selection() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
            .unwrap();
        pool.register(Arc::new(MockAgent::new("coder-2", AgentRole::core_coder())))
            .unwrap();

        let agent = pool.select(&AgentRole::core_coder());
        assert!(agent.is_some());

        let no_agent = pool.select(&AgentRole::core_research());
        assert!(no_agent.is_none());
    }

    #[test]
    fn test_module_agent_selection() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new(
            "module-auth-0",
            AgentRole::module("auth"),
        )))
        .unwrap();

        assert!(pool.select(&AgentRole::module("auth")).is_some());
        assert!(pool.select(&AgentRole::module("api")).is_none());
    }

    #[test]
    fn test_role_determination_uses_explicit_role() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        let task = AgentTask {
            id: "1".to_string(),
            description: "Some task".to_string(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(AgentRole::core_research()),
        };
        assert_eq!(pool.determine_role(&task), AgentRole::core_research());
    }

    #[test]
    fn test_role_determination_defaults_to_coder() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        let task = AgentTask {
            id: "1".to_string(),
            description: "Analyze codebase for patterns".to_string(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: None,
        };
        assert_eq!(pool.determine_role(&task), AgentRole::core_coder());
    }

    #[test]
    fn test_pool_builder() {
        let config = MultiAgentConfig::default();
        let pool = AgentPoolBuilder::new(config)
            .with_agent(Arc::new(MockAgent::new("r1", AgentRole::core_research())))
            .with_agent(Arc::new(MockAgent::new("c1", AgentRole::core_coder())))
            .build()
            .unwrap();

        assert_eq!(pool.total_agents(), 2);
        assert_eq!(pool.agent_count(&AgentRole::core_research()), 1);
        assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);
    }

    #[test]
    fn test_shutdown() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new("test-1", AgentRole::core_coder())))
            .unwrap();
        assert!(!pool.is_shutdown());

        pool.shutdown();
        assert!(pool.is_shutdown());

        let result = pool.register(Arc::new(MockAgent::new("test-2", AgentRole::core_coder())));
        assert!(result.is_err());
    }

    #[test]
    fn test_metrics() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        let metrics = pool.metrics();
        assert_eq!(metrics.total_executions, 0);
        assert_eq!(metrics.success_rate, 0.0);
    }

    #[test]
    fn test_unregister() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new(
            "to-remove",
            AgentRole::core_coder(),
        )))
        .unwrap();
        assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);

        assert!(pool.unregister("to-remove"));
        assert_eq!(pool.agent_count(&AgentRole::core_coder()), 0);

        assert!(!pool.unregister("nonexistent"));
    }

    #[test]
    fn test_has_agent() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new("agent-1", AgentRole::core_coder())))
            .unwrap();

        assert!(pool.has_agent("agent-1"));
        assert!(!pool.has_agent("agent-2"));
    }

    #[test]
    fn test_statistics() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new("agent-1", AgentRole::core_coder())))
            .unwrap();

        let stats = pool.statistics();
        assert_eq!(stats.agent_count, 1);
        assert!(!stats.is_shutdown);
        assert_eq!(stats.metrics.total_executions, 0);
    }

    #[tokio::test]
    async fn test_module_agent_fallback_to_coder() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        // Register only core_coder, no module agents
        pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
            .unwrap();

        // Create task with module role that doesn't exist
        let task = AgentTask {
            id: "task-1".to_string(),
            description: "Implement auth feature".to_string(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(AgentRole::module("auth")), // No auth module agent registered
        };

        // Should fallback to coder instead of failing
        let result = pool.execute(&task, std::path::Path::new(".")).await;
        assert!(
            result.is_ok(),
            "Should fallback to coder when module agent unavailable"
        );
    }

    #[test]
    fn test_select_by_qualified_id() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new(
            "research-0",
            AgentRole::core_research(),
        )))
        .unwrap();
        pool.register(Arc::new(MockAgent::new(
            "research-1",
            AgentRole::core_research(),
        )))
        .unwrap();

        // Should find by qualified ID
        let agent = pool.select_by_qualified_id("research-0");
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().id(), "research-0");

        let agent = pool.select_by_qualified_id("research-1");
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().id(), "research-1");

        // Should not find nonexistent
        assert!(pool.select_by_qualified_id("research-99").is_none());
    }

    #[test]
    fn test_select_by_qualified_id_fallback() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        // Register agents with simple IDs
        pool.register(Arc::new(MockAgent::new("coder-0", AgentRole::core_coder())))
            .unwrap();
        pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
            .unwrap();

        // Exact match should work
        let agent = pool.select_by_qualified_id("coder-0");
        assert!(agent.is_some());
        assert_eq!(agent.unwrap().id(), "coder-0");

        // Cross-workspace fallback: "ws-a:auth:coder-0" -> "coder-0"
        let agent = pool.select_by_qualified_id("ws-a:auth:coder-0");
        assert!(agent.is_some(), "Should fallback to role-only match");
        assert_eq!(agent.unwrap().id(), "coder-0");

        // Cross-workspace fallback with different format
        let agent = pool.select_by_qualified_id("project-b:payments:coder-1");
        assert!(agent.is_some(), "Should fallback to role-only match");
        assert_eq!(agent.unwrap().id(), "coder-1");

        // Nonexistent should still return None
        assert!(
            pool.select_by_qualified_id("ws-x:mod:nonexistent-99")
                .is_none()
        );
    }

    #[test]
    fn test_select_instances() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new(
            "planning-0",
            AgentRole::core_planning(),
        )))
        .unwrap();
        pool.register(Arc::new(MockAgent::new(
            "planning-1",
            AgentRole::core_planning(),
        )))
        .unwrap();
        pool.register(Arc::new(MockAgent::new("coder-0", AgentRole::core_coder())))
            .unwrap();

        // Should get all planning instances
        let planning_instances = pool.select_instances("planning");
        assert_eq!(planning_instances.len(), 2);

        // Should get coder instance
        let coder_instances = pool.select_instances("coder");
        assert_eq!(coder_instances.len(), 1);

        // Should get empty for nonexistent
        let none_instances = pool.select_instances("nonexistent");
        assert!(none_instances.is_empty());
    }

    #[test]
    fn test_identifiers_for_pool() {
        let config = MultiAgentConfig::default();
        let pool = AgentPool::new(config);

        pool.register(Arc::new(MockAgent::new(
            "verifier-0",
            AgentRole::core_verifier(),
        )))
        .unwrap();
        pool.register(Arc::new(MockAgent::new(
            "verifier-1",
            AgentRole::core_verifier(),
        )))
        .unwrap();

        let identifiers = pool.identifiers_for_pool("verifier");
        assert_eq!(identifiers.len(), 2);

        // Verify they are proper identifiers
        let ids: Vec<String> = identifiers.iter().map(|id| id.qualified_id()).collect();
        assert!(ids.contains(&"verifier-0".to_string()));
        assert!(ids.contains(&"verifier-1".to_string()));
    }
}
