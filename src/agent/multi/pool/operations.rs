//! Runtime operations: execution, health checks, shutdown.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures::future::join_all;
use tracing::{info, warn};

use super::{AgentPool, ExecutionTimer, PoolStatistics, RoleStats};
use super::super::messaging::AgentMessageBus;
use super::super::traits::{
    AgentExecutionMetrics, AgentRole, AgentTask, AgentTaskResult,
};
use crate::error::{PilotError, Result};

impl AgentPool {
    pub async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        self.execute_internal(task, working_dir, None).await
    }

    /// Execute task with P2P conflict resolution via MessageBus.
    pub async fn execute_with_messaging(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        bus: &AgentMessageBus,
    ) -> Result<AgentTaskResult> {
        self.execute_internal(task, working_dir, Some(bus)).await
    }

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

        // Record metrics immediately after getting the result
        let success = result.as_ref().map(|r| r.is_success()).unwrap_or(false);
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
        let semaphore = Arc::clone(&self.task_semaphore);
        let futures: Vec<_> = tasks
            .iter()
            .map(|task| {
                let sem = Arc::clone(&semaphore);
                async move {
                    let _permit = match sem.acquire().await {
                        Ok(permit) => permit,
                        Err(_) => {
                            return Err(PilotError::AgentExecution(
                                "Agent pool semaphore closed".into(),
                            ))
                        }
                    };
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

    pub fn metrics(&self) -> AgentExecutionMetrics {
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

    pub(super) fn determine_role(&self, task: &AgentTask) -> AgentRole {
        task.role.clone().unwrap_or_else(AgentRole::core_coder)
    }
}
