//! Builder methods for Coordinator configuration.

use std::sync::Arc;

use super::super::adaptive_consensus::AdaptiveConsensusExecutor;
use super::super::conflict_resolver::ConflictResolver;
use super::super::consensus_phases::TwoPhaseOrchestrator;
use super::super::escalation::HumanEscalationConfig;
use super::super::escalation::EscalationEngine;
use super::super::hierarchy::{AgentHierarchy, HierarchyConfig};
use super::super::ownership::FileOwnershipManager;
use super::Coordinator;
use crate::config::CompactionConfig;
use crate::context::ContextCompactor;
use crate::workspace::Workspace;

use parking_lot::RwLock;

impl Coordinator {
    /// Enable context compaction for long-running missions.
    pub fn with_context_compactor(mut self, config: CompactionConfig) -> Self {
        self.context_compactor = Some(Arc::new(RwLock::new(ContextCompactor::new(config))));
        self
    }

    /// Add an additional workspace for cross-workspace consensus.
    ///
    /// Configures workspace registry and file ownership manager for the
    /// additional workspace, enabling proper cross-workspace detection
    /// and parallel execution control.
    pub fn with_additional_workspace(
        mut self,
        id: impl Into<String>,
        workspace: Arc<Workspace>,
    ) -> Self {
        let id = id.into();

        // Build module map with qualified IDs for cross-workspace uniqueness
        let (module_files, dependencies) = Self::build_module_map(&workspace, Some(&id));

        // Extend file ownership manager with additional workspace's modules
        self.ownership_manager
            .add_modules(module_files.clone(), dependencies);

        // Register in workspace registry with actual file paths
        let ws_info = super::super::workspace_registry::WorkspaceInfo::new(&id, &workspace.root)
            .with_modules(workspace.modules().iter().map(|m| m.id.clone()).collect());

        self.workspace_registry
            .register_with_paths(ws_info, module_files);

        // Store for later use
        self.additional_workspaces.insert(id, workspace);
        self
    }

    /// Configure custom escalation settings.
    pub fn with_escalation_config(mut self, config: HumanEscalationConfig) -> Self {
        self.escalation_engine = EscalationEngine::new(config);
        self
    }

    pub fn with_adaptive_executor(mut self, executor: AdaptiveConsensusExecutor) -> Self {
        self.adaptive_executor = Some(executor);
        self
    }

    pub fn with_workspace(mut self, workspace: Arc<Workspace>) -> Self {
        let hierarchy = AgentHierarchy::from_workspace(&workspace, HierarchyConfig::default());
        self.hierarchy = Some(hierarchy);

        // Build module map without qualification (primary workspace)
        let (module_files, dependencies) = Self::build_module_map(&workspace, None);

        self.ownership_manager = Arc::new(FileOwnershipManager::with_module_map(
            module_files.clone(),
            dependencies,
        ));

        // Register workspace with actual file paths for cross-workspace detection
        let ws_info =
            super::super::workspace_registry::WorkspaceInfo::new(&workspace.name, &workspace.root)
                .with_modules(workspace.modules().iter().map(|m| m.id.clone()).collect());

        self.workspace_registry
            .register_with_paths(ws_info, module_files);

        self.workspace = Some(workspace);
        self
    }

    pub fn with_task_agent(mut self, task_agent: Arc<crate::agent::TaskAgent>) -> Self {
        self.task_agent = Some(task_agent);
        self
    }

    pub fn with_event_store(mut self, store: Arc<crate::state::EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_message_bus(
        mut self,
        bus: Arc<super::super::messaging::AgentMessageBus>,
    ) -> Self {
        self.message_bus = Some(bus);
        self
    }

    pub(crate) fn with_two_phase_orchestrator(mut self, orchestrator: TwoPhaseOrchestrator) -> Self {
        self.two_phase_orchestrator = Some(orchestrator);
        self
    }

    pub(crate) fn with_conflict_resolver(mut self, resolver: Arc<ConflictResolver>) -> Self {
        self.conflict_resolver = Some(resolver);
        self
    }
}
