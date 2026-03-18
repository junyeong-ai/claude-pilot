//! Coordinator agent for orchestrating multi-agent workflows.
//!
//! The Coordinator uses OrchestrationSession as the central coordination unit,
//! integrating participant management, task scheduling, health monitoring,
//! and notification-based communication.

mod accessors;
mod builder;
mod consensus_orchestration;
mod execution;
mod health;
mod context_helpers;
mod event_helpers;
mod module_helpers;
mod ownership_helpers;
mod implementation;
mod phases;
mod recovery;
mod types;
mod verification;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use super::adaptive_consensus::AdaptiveConsensusExecutor;
use super::conflict_resolver::ConflictResolver;
use super::consensus_phases::TwoPhaseOrchestrator;
use super::escalation::EscalationEngine;
use super::health::HealthMonitor;
use super::hierarchy::AgentHierarchy;
use super::messaging::AgentMessageBus;
use super::ownership::FileOwnershipManager;
use super::pool::AgentPool;
use super::session::{ContinuationManager, InvocationTracker, SessionHealthTracker};
use super::workspace_registry::WorkspaceRegistry;
use crate::agent::TaskAgent;
use crate::config::MultiAgentConfig;
use crate::context::ContextCompactor;
use crate::state::EventStore;
use crate::workspace::Workspace;

// Re-export public types

/// Coordinates multi-agent task execution.
///
/// The Coordinator integrates OrchestrationSession for centralized coordination,
/// including participant tracking, task dependencies, health monitoring, and
/// notification-based communication.
pub struct Coordinator {
    pub(super) config: MultiAgentConfig,
    pub(super) pool: Arc<AgentPool>,
    pub(super) adaptive_executor: Option<AdaptiveConsensusExecutor>,
    pub(super) task_agent: Option<Arc<TaskAgent>>,
    pub(super) workspace: Option<Arc<Workspace>>,
    pub(super) hierarchy: Option<AgentHierarchy>,
    pub(super) health_monitor: HealthMonitor,
    pub(super) event_store: Option<Arc<EventStore>>,
    pub(super) message_bus: Option<Arc<AgentMessageBus>>,
    /// Escalation engine for handling consensus failures
    pub(super) escalation_engine: EscalationEngine,
    /// File ownership manager for preventing parallel execution conflicts
    pub(super) ownership_manager: Arc<FileOwnershipManager>,
    /// P2P conflict resolver for negotiated ownership transfers
    pub(super) conflict_resolver: Option<Arc<ConflictResolver>>,
    /// Two-phase orchestrator for direction setting + synthesis
    pub(super) two_phase_orchestrator: Option<TwoPhaseOrchestrator>,
    /// Registry for cross-workspace consensus
    pub(super) workspace_registry: Arc<WorkspaceRegistry>,
    /// Additional workspaces for cross-workspace operations
    pub(super) additional_workspaces: HashMap<String, Arc<Workspace>>,

    // === Session-based coordination ===
    /// Session health tracker for participant health monitoring
    pub(super) session_health: Arc<RwLock<SessionHealthTracker>>,
    /// Invocation tracker for stateless agent pattern
    pub(super) invocation_tracker: Arc<RwLock<InvocationTracker>>,
    /// Continuation manager for async coordination
    pub(super) continuation_manager: Arc<RwLock<ContinuationManager>>,
    /// Context compactor for long-running mission context management
    pub(super) context_compactor: Option<Arc<RwLock<ContextCompactor>>>,
}

impl Coordinator {
    pub fn new(config: MultiAgentConfig, pool: Arc<AgentPool>) -> Self {
        let health_monitor = HealthMonitor::new(Arc::clone(&pool));
        Self {
            config,
            pool,
            adaptive_executor: None,
            task_agent: None,
            workspace: None,
            hierarchy: None,
            health_monitor,
            event_store: None,
            message_bus: None,
            escalation_engine: EscalationEngine::default(),
            ownership_manager: Arc::new(FileOwnershipManager::new()),
            conflict_resolver: None,
            two_phase_orchestrator: None,
            workspace_registry: Arc::new(WorkspaceRegistry::new()),
            additional_workspaces: HashMap::new(),
            // Session-based coordination
            session_health: Arc::new(RwLock::new(SessionHealthTracker::with_default_config())),
            invocation_tracker: Arc::new(RwLock::new(InvocationTracker::default())),
            continuation_manager: Arc::new(RwLock::new(ContinuationManager::default())),
            // Context compaction disabled by default - enabled via with_context_compactor()
            context_compactor: None,
        }
    }
}
