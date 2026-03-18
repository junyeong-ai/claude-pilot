//! Multi-agent architecture with module-based agents and consensus-based planning.
//!
//! # Overview
//!
//! This module provides the multi-agent system for collaborative task execution:
//! - Module-based agents with scope enforcement using modmap::Module
//! - Consensus engine for multi-round planning and agreement
//! - Manifest-driven context injection from Workspace data
//!
//! # Agent Types
//!
//! - **Core agents**: Research, Planning, Coder, Verifier - standard task execution
//! - **Module agents**: ModuleAgent - module-specific expertise with scope enforcement
//! - **Infrastructure agents**: Architecture, Reviewer - cross-cutting concerns
//! - **Advisory agents**: Architect - design validation and guidance
//!
//! # Key Components
//!
//! ## Agents
//! - [`ResearchAgent`] - Evidence gathering and codebase analysis
//! - [`PlanningAgent`] - Task planning and decomposition
//! - [`CoderAgent`] - Implementation and bug fixes
//! - [`VerifierAgent`] - Output verification and validation
//! - [`ModuleAgent`] - Module-specialized with scope enforcement
//! - [`ArchitectAgent`] - Design validation advisor
//!
//! ## Infrastructure
//! - [`consensus`] - Evidence-weighted voting and multi-round consensus
//! - `Coordinator` - Multi-agent orchestration and workflow management
//! - [`discovery`] - Project analysis and module discovery
//! - [`architecture`] - Runtime boundary enforcement
//! - [`reviewer`] - Code review and quality assessment
//!
//! ## Context
//! - [`context`] - Module-based context building from modmap::Module + manifest data
//!
//! ## Supporting Systems
//! - [`health`] - Health monitoring and bottleneck detection
//! - [`messaging`] - Inter-agent communication via message bus
//! - [`ownership`] - File ownership and lease management
//! - [`hierarchy`] - Agent hierarchy and routing decisions
//! - [`escalation`] - Conflict escalation and resolution
//!
//! # Architecture
//!
//! ```text
//! Manifest = WHAT (project structure/facts) → Auto-inject from Workspace
//! Agents   = WHO  (role/composition)        → Task assignment
//! ```

// Private modules: types exported via `pub use` for controlled API surface
mod architect;
mod coder;
mod coordinator;
mod module_agent;
mod planning;
mod pool;
mod research;
mod traits;
mod verifier;

// Foundation modules: shared types and core contracts
pub mod core;
pub mod shared;

// Public modules: full API exposed for extension and customization
pub mod adaptive_consensus;
pub mod architecture;
pub mod conflict_resolver;
pub mod consensus;
pub mod consensus_phases;
pub mod context;
pub mod convergence;
pub mod discovery;
pub mod escalation;
pub mod health;
pub mod hierarchy;
pub mod identity;
pub mod message_store;
pub mod messaging;
pub mod metrics;
pub mod ownership;
pub mod reviewer;
pub mod scope;
pub mod session;
pub mod workspace_registry;

// Shared types - canonical re-exports
pub use shared::{AgentId, RoleCategory, TaskPriority};
// Core types - externally used subset
pub use core::{
    AgentRole, AgentTask, SpecializedAgent, TaskContext,
};
// Core Types - internal only (AgentExecutionMetrics used by orchestrator/engine.rs via agent::)
pub(crate) use core::AgentExecutionMetrics;

// Agents - externally used
pub use architect::ArchitectAgent;
pub use coder::CoderAgent;
pub use module_agent::ModuleAgent;
pub use planning::PlanningAgent;
pub use research::ResearchAgent;
pub use reviewer::ReviewerAgent;
pub use verifier::VerifierAgent;

// Coordination - externally used
pub use coordinator::Coordinator;
pub use pool::AgentPoolBuilder;
// Coordination - internal re-exports for sibling modules
pub(crate) use pool::AgentPool;
// Consensus - externally used
pub use consensus::{ConsensusEngine, ConsensusResult, TaskComplexity};


// Messaging - externally used
pub use messaging::{AgentMessage, AgentMessageBus, AgentMessageType, MessagePayload};

// Infrastructure - externally used
pub use hierarchy::{HierarchicalAggregator, ParticipantSet};
pub use shared::TierLevel;

// Hierarchical Consensus - externally used
pub use adaptive_consensus::AdaptiveConsensusExecutor;

// Workspace Registry - externally used
pub use workspace_registry::{WorkspaceInfo, WorkspaceRegistry};

// Scope - externally used
pub use scope::{AgentScope, ScopeTier};

// Identity - externally used (AgentIdentifier used by agent modules internally)
pub use identity::AgentIdentifier;

// Re-exports from modmap
pub use modmap::{
    DetectedLanguage, EvidenceLocation, Module, ModuleMap, WorkspaceType, is_path_in_scope,
};

use std::path::Path;
use std::sync::Arc;

use crate::agent::TaskAgent;
use crate::config::MultiAgentConfig;
use crate::error::Result;
use crate::state::EventStore;

// Local imports for functions in this module
use architecture::BoundaryEnforcementAgent;
use conflict_resolver::ConflictResolver;
use message_store::MessageStore;

/// Create a core agent pool with the standard agents.
///
/// Creates a pool containing only the core agents (Research, Planning, Coder, Verifier)
/// based on the instance counts specified in the configuration. Use this for basic
/// multi-agent workflows without dynamic module agents.
///
/// # Arguments
/// * `config` - Multi-agent configuration specifying instance counts per role
/// * `task_agent` - Shared TaskAgent for LLM interactions
///
/// # Returns
/// * `Result<AgentPool>` - Configured agent pool ready for task execution
///
/// # Example
/// ```ignore
/// let config = MultiAgentConfig::default();
/// let task_agent = Arc::new(TaskAgent::new(AgentConfig::default()));
/// let pool = create_agent_pool(config, task_agent)?;
/// ```
pub fn create_agent_pool(
    config: MultiAgentConfig,
    task_agent: Arc<TaskAgent>,
) -> Result<AgentPool> {
    let mut builder = AgentPoolBuilder::new(config.clone());

    for i in 0..config.instance_count("research") {
        builder = builder.with_agent(ResearchAgent::with_id(
            &format!("research-{i}"),
            Arc::clone(&task_agent),
        ));
    }

    for i in 0..config.instance_count("planning") {
        builder = builder.with_agent(PlanningAgent::with_id(
            &format!("planning-{i}"),
            Arc::clone(&task_agent),
        ));
    }

    for i in 0..config.instance_count("coder") {
        builder = builder.with_agent(CoderAgent::with_id(
            &format!("coder-{i}"),
            Arc::clone(&task_agent),
        ));
    }

    for i in 0..config.instance_count("verifier") {
        builder = builder.with_agent(VerifierAgent::with_id(
            &format!("verifier-{i}"),
            Arc::clone(&task_agent),
        ));
    }

    builder.build()
}

pub async fn create_dynamic_pool(
    config: MultiAgentConfig,
    task_agent: Arc<TaskAgent>,
    _working_dir: &Path,
    manifest_path: Option<&Path>,
) -> Result<(AgentPool, Option<Arc<crate::workspace::Workspace>>)> {
    use crate::workspace::Workspace;

    let pool = create_agent_pool(config.clone(), Arc::clone(&task_agent))?;

    let Some(path) = manifest_path else {
        return Ok((pool, None));
    };

    let workspace = match Workspace::from_manifest(path).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load workspace from manifest");
            return Ok((pool, None));
        }
    };

    let mut registration_errors = Vec::new();

    let arch_agent = Arc::new(BoundaryEnforcementAgent::from_workspace(
        &workspace,
        Arc::clone(&task_agent),
    ));
    if let Err(e) = pool.register(arch_agent) {
        tracing::error!(error = %e, "Failed to register architecture agent");
        registration_errors.push(format!("Architecture agent: {}", e));
    }

    for module in workspace.modules() {
        let module_arc = Arc::new(module.clone());
        let module_name = module.name.clone();
        let manifest_context = workspace.module_context(&module.id).cloned();

        let agent: Arc<ModuleAgent> = if let Some(ctx) = manifest_context {
            Arc::new(ModuleAgent::with_manifest_context(
                module_arc,
                ctx,
                Arc::clone(&task_agent),
            ))
        } else {
            Arc::new(ModuleAgent::new(module_arc, Arc::clone(&task_agent)))
        };

        if let Err(e) = pool.register(agent) {
            tracing::error!(module = %module_name, error = %e, "Failed to register module agent");
            registration_errors.push(format!("Module agent '{}': {}", module_name, e));
        }
    }

    let reviewer = ReviewerAgent::new("reviewer-0", Arc::clone(&task_agent));
    if let Err(e) = pool.register(Arc::new(reviewer)) {
        tracing::error!(error = %e, "Failed to register reviewer agent");
        registration_errors.push(format!("Reviewer agent: {}", e));
    }

    let architect = ArchitectAgent::with_id("architect-0", Arc::clone(&task_agent));
    if let Err(e) = pool.register(architect) {
        tracing::error!(error = %e, "Failed to register architect agent");
        registration_errors.push(format!("Architect agent: {}", e));
    }

    if registration_errors.is_empty() {
        tracing::info!(
            module_agents = workspace.modules().len(),
            "Successfully registered all dynamic agents"
        );
    } else {
        tracing::error!(
            count = registration_errors.len(),
            "Dynamic agent registration encountered {} error(s)",
            registration_errors.len()
        );
    }

    let workspace = Arc::new(workspace);
    Ok((pool, Some(workspace)))
}

/// Create a coordinator with consensus engine for dynamic mode.
///
/// Creates a fully configured coordinator for multi-agent task execution with:
/// - Agent pool (dynamic or core-only based on config)
/// - Consensus engine (if dynamic_mode enabled)
/// - Message bus with event store integration (if provided)
/// - Project map for dynamic agent registration
///
/// The coordinator stores the ProjectMap for dynamic agent registration with full scope data.
///
/// # Arguments
/// * `config` - Multi-agent configuration (controls dynamic mode)
/// * `task_agent` - Shared TaskAgent for LLM interactions
/// * `working_dir` - Project root directory for analysis
/// * `event_store` - Optional event store for audit trail and message logging
///
/// # Returns
/// * `Result<Coordinator>` - Configured coordinator ready for task execution
///
/// # Configuration Modes
///
/// ## Dynamic Mode (config.dynamic_mode = true)
/// - Creates dynamic pool with module agents
/// - Enables consensus engine for multi-round planning
/// - Stores ProjectMap for on-demand agent registration
/// - Uses evidence-weighted voting for decisions
///
/// ## Sequential Mode (config.dynamic_mode = false)
/// - Creates core agent pool only
/// - Uses legacy sequential pipeline
/// - No consensus or dynamic registration
///
pub async fn create_coordinator(
    config: MultiAgentConfig,
    task_agent: Arc<TaskAgent>,
    working_dir: &Path,
    event_store: Option<Arc<EventStore>>,
    manifest_path: Option<&Path>,
) -> Result<Coordinator> {
    let (pool, workspace) = if config.dynamic_mode {
        let (pool, ws) = create_dynamic_pool(
            config.clone(),
            Arc::clone(&task_agent),
            working_dir,
            manifest_path,
        )
        .await?;
        (Arc::new(pool), ws)
    } else {
        (
            Arc::new(create_agent_pool(config.clone(), Arc::clone(&task_agent))?),
            None,
        )
    };

    let mut coordinator = Coordinator::new(config.clone(), pool)
        .with_task_agent(Arc::clone(&task_agent))
        .with_context_compactor(config.context_compaction.clone());

    if let Some(ref store) = event_store {
        let bus = AgentMessageBus::default().with_event_store(Arc::clone(store));
        coordinator = coordinator.with_message_bus(Arc::new(bus));
    }

    if config.dynamic_mode {
        let consensus_engine =
            ConsensusEngine::new(Arc::clone(&task_agent), config.consensus.clone());
        let mut adaptive_executor =
            AdaptiveConsensusExecutor::new(consensus_engine, config.consensus.clone());

        if let Some(ref store) = event_store {
            adaptive_executor = adaptive_executor.with_event_store(Arc::clone(store));
        }

        coordinator = coordinator.with_adaptive_executor(adaptive_executor);

        if let Some(ws) = workspace {
            coordinator = coordinator.with_workspace(ws);
        }

        let mut two_phase = consensus_phases::TwoPhaseOrchestrator::new(Arc::clone(&task_agent));
        two_phase = two_phase.with_workspace_registry(Arc::clone(&coordinator.workspace_registry));
        coordinator = coordinator.with_two_phase_orchestrator(two_phase);

        // Wire ConflictResolver for P2P ownership negotiation
        let messages_db = working_dir.join(".pilot").join("messages.db");
        match MessageStore::new(&messages_db) {
            Ok(msg_store) => {
                let resolver = ConflictResolver::new(
                    Arc::clone(&coordinator.ownership_manager),
                    Arc::new(msg_store),
                );
                coordinator = coordinator.with_conflict_resolver(Arc::new(resolver));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "P2P conflict resolution disabled: MessageStore initialization failed"
                );
            }
        }
    }

    if let Some(store) = event_store {
        coordinator = coordinator.with_event_store(store);
    }

    Ok(coordinator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    #[test]
    fn test_create_agent_pool() {
        let config = MultiAgentConfig::default();
        let task_agent = Arc::new(TaskAgent::new(AgentConfig::default()));

        let pool = create_agent_pool(config.clone(), task_agent).unwrap();

        assert_eq!(
            pool.agent_count(&AgentRole::core_research()),
            config.instance_count("research")
        );
        assert_eq!(
            pool.agent_count(&AgentRole::core_planning()),
            config.instance_count("planning")
        );
        assert_eq!(
            pool.agent_count(&AgentRole::core_coder()),
            config.instance_count("coder")
        );
        assert_eq!(
            pool.agent_count(&AgentRole::core_verifier()),
            config.instance_count("verifier")
        );
    }

    #[test]
    fn test_total_agents() {
        let config = MultiAgentConfig::default();
        let task_agent = Arc::new(TaskAgent::new(AgentConfig::default()));

        let pool = create_agent_pool(config.clone(), task_agent).unwrap();

        let expected = config.instance_count("research")
            + config.instance_count("planning")
            + config.instance_count("coder")
            + config.instance_count("verifier");

        assert_eq!(pool.total_agents(), expected);
    }

    #[tokio::test]
    async fn test_create_coordinator() {
        let config = MultiAgentConfig::default();
        let task_agent = Arc::new(TaskAgent::new(AgentConfig::default()));
        let temp_dir = std::env::temp_dir();

        let coordinator = create_coordinator(config, task_agent, &temp_dir, None, None)
            .await
            .unwrap();
        assert!(!coordinator.is_shutdown());
    }
}
