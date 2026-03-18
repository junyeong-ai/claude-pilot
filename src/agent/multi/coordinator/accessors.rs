//! Getter and accessor methods for Coordinator.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use super::super::health::{HealthReport, HealthStatus};
use super::super::hierarchy::AgentHierarchy;
use super::super::messaging::AgentMessageBus;
use super::super::pool::PoolStatistics;
use super::super::session::{InvocationTracker, SessionHealthTracker};
use super::super::traits::AgentExecutionMetrics;
use super::super::workspace_registry::WorkspaceRegistry;
use super::Coordinator;

impl Coordinator {
    /// Get the workspace registry.
    pub fn workspace_registry(&self) -> &Arc<WorkspaceRegistry> {
        &self.workspace_registry
    }

    /// Check if a set of files requires cross-workspace consensus.
    pub fn requires_cross_workspace(&self, files: &[PathBuf]) -> bool {
        self.workspace_registry.requires_cross_workspace(files)
    }

    /// Get session health tracker.
    pub fn session_health(&self) -> &Arc<RwLock<SessionHealthTracker>> {
        &self.session_health
    }

    /// Get invocation tracker.
    pub fn invocation_tracker(&self) -> &Arc<RwLock<InvocationTracker>> {
        &self.invocation_tracker
    }

    pub fn message_bus(&self) -> Option<&Arc<AgentMessageBus>> {
        self.message_bus.as_ref()
    }

    /// Get the agent hierarchy if configured.
    pub fn hierarchy(&self) -> Option<&AgentHierarchy> {
        self.hierarchy.as_ref()
    }

    /// Get the health monitor.
    pub fn health_monitor(&self) -> &super::super::health::HealthMonitor {
        &self.health_monitor
    }

    pub fn pool_stats(&self) -> PoolStatistics {
        self.pool.statistics()
    }

    pub fn metrics(&self) -> AgentExecutionMetrics {
        self.pool.metrics()
    }

    pub fn shutdown(&self) {
        self.pool.shutdown()
    }

    pub fn is_shutdown(&self) -> bool {
        self.pool.is_shutdown()
    }

    pub fn has_agent(&self, id: &str) -> bool {
        self.pool.has_agent(id)
    }

    pub fn check_health(&self) -> HealthReport {
        self.health_monitor.check_health()
    }

    pub fn health_status(&self) -> HealthStatus {
        self.health_monitor.check_health().status
    }
}
