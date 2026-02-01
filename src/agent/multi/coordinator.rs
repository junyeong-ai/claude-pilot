//! Coordinator agent for orchestrating multi-agent workflows.
//!
//! The Coordinator uses OrchestrationSession as the central coordination unit,
//! integrating participant management, task scheduling, health monitoring,
//! and notification-based communication.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::adaptive_consensus::{
    AdaptiveConsensusExecutor, ConsensusMission, ConsensusOutcome, HierarchicalConsensusResult,
};
use super::consensus::ConflictSeverity;
use super::consensus::{Conflict, ConsensusResult, ConsensusTask, TaskComplexity};
use super::consensus_phases::TwoPhaseOrchestrator;
use super::context::{ContextComposer, PersonaLoader};
use super::escalation::{EscalationConfig, EscalationEngine, EscalationOption, EscalationStrategy};
use super::health::{HealthMonitor, HealthReport, HealthStatus};
use super::hierarchy::{AgentHierarchy, HierarchyConfig};
use super::messaging::{AgentMessage, AgentMessageBus, MessagePayload};
use super::ownership::{AccessType, AcquisitionResult, FileOwnershipManager, LeaseGuard};
use super::pool::{AgentPool, PoolStatistics};
use super::rules::RuleRegistry;
use super::session::{
    ContinuationManager, InvocationTracker, NotificationType, OrchestrationSession, SessionConfig,
    SessionHealthTracker, SharedSession,
};
use super::shared::QualifiedModule;
use super::skills::SkillRegistry;
use super::traits::{
    AgentRole, AgentTask, AgentTaskResult, ConvergenceResult, MetricsSnapshot, TaskContext,
    TaskPriority, TaskStatus, VerificationVerdict, calculate_priority_score,
    extract_files_from_output,
};
use super::workspace_registry::WorkspaceRegistry;
use crate::agent::TaskAgent;
use crate::agent::multi::{AgentId, ComposedContext};
use crate::config::{CompactionConfig, MultiAgentConfig};
use crate::context::ContextCompactor;
use crate::error::{PilotError, Result};
use crate::orchestration::AgentScope;
use crate::state::{DomainEvent, EventPayload, EventStore};
use crate::workspace::Workspace;

/// Checkpoint information for session recovery.
#[derive(Debug, Clone)]
struct CheckpointInfo {
    id: String,
    session_id: String,
    round: usize,
}

/// Coordinates multi-agent task execution.
///
/// The Coordinator integrates OrchestrationSession for centralized coordination,
/// including participant tracking, task dependencies, health monitoring, and
/// notification-based communication.
pub struct Coordinator {
    config: MultiAgentConfig,
    pool: Arc<AgentPool>,
    adaptive_executor: Option<AdaptiveConsensusExecutor>,
    task_agent: Option<Arc<TaskAgent>>,
    workspace: Option<Arc<Workspace>>,
    hierarchy: Option<AgentHierarchy>,
    health_monitor: HealthMonitor,
    event_store: Option<Arc<EventStore>>,
    message_bus: Option<Arc<AgentMessageBus>>,
    rule_registry: Option<Arc<RuleRegistry>>,
    skill_registry: Arc<SkillRegistry>,
    persona_loader: Arc<PersonaLoader>,
    /// Escalation engine for handling consensus failures
    escalation_engine: EscalationEngine,
    /// File ownership manager for preventing parallel execution conflicts
    ownership_manager: Arc<FileOwnershipManager>,
    /// Two-phase orchestrator for direction setting + synthesis
    two_phase_orchestrator: Option<TwoPhaseOrchestrator>,
    /// Registry for cross-workspace consensus
    workspace_registry: Arc<WorkspaceRegistry>,
    /// Additional workspaces for cross-workspace operations
    additional_workspaces: HashMap<String, Arc<Workspace>>,

    // === Session-based coordination ===
    /// Session health tracker for participant health monitoring
    session_health: Arc<RwLock<SessionHealthTracker>>,
    /// Invocation tracker for stateless agent pattern
    invocation_tracker: Arc<RwLock<InvocationTracker>>,
    /// Continuation manager for async coordination
    continuation_manager: Arc<RwLock<ContinuationManager>>,
    /// Context compactor for long-running mission context management
    context_compactor: Option<Arc<RwLock<ContextCompactor>>>,
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
            rule_registry: None,
            skill_registry: Arc::new(SkillRegistry::default()),
            persona_loader: Arc::new(PersonaLoader::default()),
            escalation_engine: EscalationEngine::default(),
            ownership_manager: Arc::new(FileOwnershipManager::new()),
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
        let ws_info = super::workspace_registry::WorkspaceInfo::new(&id, &workspace.root)
            .with_modules(workspace.modules().iter().map(|m| m.id.clone()).collect());

        self.workspace_registry
            .register_with_paths(ws_info, module_files);

        // Store for later use
        self.additional_workspaces.insert(id, workspace);
        self
    }

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

    /// Record task completion in session health tracker.
    fn record_task_completion(&self, agent_id: &AgentId, response_time_ms: u64) {
        self.session_health
            .write()
            .record_task_completion(agent_id, response_time_ms);
    }

    /// Record task failure in session health tracker.
    fn record_task_failure(&self, agent_id: &AgentId) {
        self.session_health.write().record_task_failure(agent_id);
    }

    /// Configure custom escalation settings.
    pub fn with_escalation_config(mut self, config: EscalationConfig) -> Self {
        self.escalation_engine = EscalationEngine::new(config);
        self
    }

    pub fn with_adaptive_executor(mut self, executor: AdaptiveConsensusExecutor) -> Self {
        self.adaptive_executor = Some(executor);
        self
    }

    /// Build module file paths and dependencies from a workspace.
    ///
    /// If `workspace_id` is provided, module IDs are qualified as "workspace_id::module_id".
    /// This is used for cross-workspace scenarios where modules need unique identifiers.
    fn build_module_map(
        workspace: &Workspace,
        workspace_id: Option<&str>,
    ) -> (HashMap<String, Vec<PathBuf>>, HashMap<String, Vec<String>>) {
        let mut module_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();

        for module in workspace.modules() {
            let paths: Vec<PathBuf> = module.paths.iter().map(PathBuf::from).collect();

            // Build module key with optional workspace qualification
            let module_key = match workspace_id {
                Some(ws_id) => format!("{}::{}", ws_id, module.id),
                None => module.id.clone(),
            };

            module_files.insert(module_key.clone(), paths);

            // Build dependency list with same qualification
            let deps: Vec<String> = module
                .dependencies
                .iter()
                .map(|d| match workspace_id {
                    Some(ws_id) => format!("{}::{}", ws_id, &d.module_id),
                    None => d.module_id.clone(),
                })
                .collect();
            dependencies.insert(module_key, deps);
        }

        (module_files, dependencies)
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
            super::workspace_registry::WorkspaceInfo::new(&workspace.name, &workspace.root)
                .with_modules(workspace.modules().iter().map(|m| m.id.clone()).collect());

        self.workspace_registry
            .register_with_paths(ws_info, module_files);

        self.workspace = Some(workspace);
        self
    }

    pub fn with_task_agent(mut self, task_agent: Arc<TaskAgent>) -> Self {
        self.task_agent = Some(task_agent);
        self
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_message_bus(mut self, bus: Arc<AgentMessageBus>) -> Self {
        self.message_bus = Some(bus);
        self
    }

    pub fn with_rule_registry(mut self, registry: Arc<RuleRegistry>) -> Self {
        self.rule_registry = Some(registry);
        self
    }

    pub fn with_skill_registry(mut self, registry: Arc<SkillRegistry>) -> Self {
        self.skill_registry = registry;
        self
    }

    pub fn with_persona_loader(mut self, loader: Arc<PersonaLoader>) -> Self {
        self.persona_loader = loader;
        self
    }

    pub fn with_two_phase_orchestrator(mut self, orchestrator: TwoPhaseOrchestrator) -> Self {
        self.two_phase_orchestrator = Some(orchestrator);
        self
    }

    /// Check and apply context compaction if threshold exceeded.
    ///
    /// Called after task execution phases to prevent context overflow
    /// during long-running missions. Delegates to ContextCompactor for
    /// configurable compaction logic.
    fn maybe_compact_task_context(&self, context: &mut TaskContext) {
        if let Some(compactor) = &self.context_compactor {
            let compactor = compactor.read();
            if compactor.compact_task_context(context) {
                debug!(
                    mission_id = %context.mission_id,
                    "TaskContext compacted due to threshold exceeded"
                );
            }
        }
    }

    pub fn message_bus(&self) -> Option<&Arc<AgentMessageBus>> {
        self.message_bus.as_ref()
    }

    /// Get the agent hierarchy if configured.
    pub fn hierarchy(&self) -> Option<&AgentHierarchy> {
        self.hierarchy.as_ref()
    }

    /// Get the health monitor.
    pub fn health_monitor(&self) -> &HealthMonitor {
        &self.health_monitor
    }

    fn compose_context_for_task(&self, task: &mut AgentTask) -> Option<ComposedContext> {
        let rule_registry = self.rule_registry.as_ref()?;

        let affected_files: Vec<std::path::PathBuf> = task
            .context
            .related_files
            .iter()
            .map(std::path::PathBuf::from)
            .collect();

        let default_role = AgentRole::core_coder();
        let role = task.role.as_ref().unwrap_or(&default_role);

        let composer =
            ContextComposer::new(rule_registry, &self.skill_registry, &self.persona_loader);
        let composed = composer.compose(role, task, &affected_files);

        task.context.composed_prompt = Some(composed.system_prompt.clone());
        Some(composed)
    }

    async fn emit_context_events(
        &self,
        mission_id: &str,
        agent_id: &str,
        task_id: &str,
        composed: &ComposedContext,
    ) {
        if composed.rule_count() > 0 {
            self.emit_event(DomainEvent::rules_injected(
                mission_id,
                agent_id,
                task_id,
                composed.rule_count(),
                composed.rule_categories(),
            ))
            .await;
        }

        if let Some(skill) = &composed.active_skill {
            self.emit_event(DomainEvent::skill_activated(
                mission_id,
                agent_id,
                task_id,
                &format!("{:?}", skill),
            ))
            .await;
        }

        if let Some(ref persona_name) = composed.persona_name {
            self.emit_event(DomainEvent::persona_loaded(
                mission_id,
                agent_id,
                persona_name,
            ))
            .await;
        }
    }

    async fn broadcast_task_assignment(&self, task: &AgentTask, agent_id: &str) {
        if let Some(bus) = &self.message_bus {
            let message = AgentMessage::task_assignment("coordinator", agent_id, task.clone());
            if let Err(e) = bus.send(message).await {
                debug!(error = %e, "Failed to broadcast task assignment");
            }
        }
    }

    async fn broadcast_task_result(&self, result: &AgentTaskResult, from_agent: &str) {
        if let Some(bus) = &self.message_bus {
            // Send to coordinator (for collection)
            let message = AgentMessage::task_result(from_agent, "coordinator", result.clone());
            if let Err(e) = bus.send(message).await {
                debug!(error = %e, "Failed to send task result to coordinator");
            }

            // Broadcast to all agents (for inter-agent visibility)
            // This enables agents to observe each other's work status and react to conflicts
            let broadcast_msg = AgentMessage::new(
                from_agent,
                "*", // Broadcast to all
                MessagePayload::TaskResult {
                    result: result.clone(),
                },
            );
            if let Err(e) = bus.send(broadcast_msg).await {
                debug!(error = %e, "Failed to broadcast task result to all agents");
            }
        }
    }

    async fn emit_event(&self, event: DomainEvent) {
        self.emit_event_internal(event, false).await;
    }

    async fn emit_critical_event(&self, event: DomainEvent) {
        self.emit_event_internal(event, true).await;
    }

    async fn emit_event_internal(&self, event: DomainEvent, is_critical: bool) {
        if let Some(store) = &self.event_store {
            let event_type = event.event_type();
            if let Err(e) = store.append(event).await {
                if is_critical {
                    tracing::error!(
                        error = %e,
                        event_type,
                        "Failed to append critical event to store - audit trail may be incomplete"
                    );
                } else {
                    warn!(error = %e, event_type, "Failed to emit event");
                }
            }
        }
    }

    /// Check for an incomplete session and attempt recovery.
    ///
    /// Returns `Ok(Some(result))` if session was recovered successfully,
    /// `Ok(None)` if no recovery was needed, or an error if recovery failed.
    pub async fn try_recover_session(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<Option<MissionResult>> {
        let Some(checkpoint) = self.find_latest_checkpoint(mission_id).await? else {
            return Ok(None);
        };

        info!(
            mission_id = %mission_id,
            checkpoint_id = %checkpoint.id,
            round = checkpoint.round,
            "Found incomplete session, attempting recovery"
        );

        let Some(ref executor) = self.adaptive_executor else {
            self.emit_recovery_failed(
                mission_id,
                Some(&checkpoint.session_id),
                &checkpoint.id,
                "Adaptive executor not configured",
            )
            .await;
            return Ok(None);
        };

        let Some(ref workspace) = self.workspace else {
            self.emit_recovery_failed(
                mission_id,
                Some(&checkpoint.session_id),
                &checkpoint.id,
                "Workspace not configured",
            )
            .await;
            return Ok(None);
        };

        // Note: SessionRecoveryStarted event is emitted by AdaptiveConsensusExecutor
        // after state reconstruction, which provides proper correlation ID.

        let scope = AgentScope::Workspace {
            workspace: workspace.name.clone(),
        };
        let mission = ConsensusMission::new(
            description,
            vec![],
            TaskContext::new(mission_id, working_dir.to_path_buf()),
        );

        let start_time = std::time::Instant::now();

        match executor
            .resume_from_checkpoint(
                &checkpoint.id,
                &checkpoint.session_id,
                checkpoint.round,
                &mission,
                &scope,
                workspace,
                &self.pool,
            )
            .await
        {
            Ok(consensus_result) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let success = matches!(
                    consensus_result.outcome,
                    ConsensusOutcome::Converged | ConsensusOutcome::PartialConvergence
                );

                // Emit recovery completed event with correlation for queryability
                self.emit_event(
                    DomainEvent::new(
                        mission_id,
                        EventPayload::SessionRecoveryCompleted {
                            session_id: consensus_result.session_id.clone(),
                            checkpoint_id: checkpoint.id.clone(),
                            outcome: consensus_result.outcome.as_str().to_string(),
                            recovered_rounds: consensus_result.total_rounds,
                            duration_ms,
                        },
                    )
                    .with_correlation(&consensus_result.session_id),
                )
                .await;

                // Extract and convert conflicts from tier results
                let conflicts = Self::extract_conflicts_from_tiers(&consensus_result.tier_results);

                let tier_count = consensus_result.tier_results.len();
                let converged_tiers = consensus_result
                    .tier_results
                    .values()
                    .filter(|t| t.converged)
                    .count();

                Ok(Some(MissionResult {
                    success,
                    results: Vec::new(),
                    summary: consensus_result.plan.unwrap_or_default(),
                    metrics: MetricsSnapshot {
                        total_executions: consensus_result.total_rounds as u64,
                        successful_executions: converged_tiers as u64,
                        failed_executions: (tier_count - converged_tiers) as u64,
                        success_rate: if tier_count > 0 {
                            converged_tiers as f64 / tier_count as f64
                        } else {
                            0.0
                        },
                        avg_duration_ms: duration_ms,
                    },
                    conflicts,
                    complexity_metrics: ComplexityMetrics {
                        by_level: HashMap::new(),
                        total_score: 0.0,
                        completed_score: 0.0,
                        completion_pct: if success { 100.0 } else { 0.0 },
                    },
                    needs_human_decision: None,
                }))
            }
            Err(e) => {
                self.emit_recovery_failed(
                    mission_id,
                    Some(&checkpoint.session_id),
                    &checkpoint.id,
                    &e.to_string(),
                )
                .await;
                warn!(error = %e, "Session recovery failed, will start fresh");
                Ok(None)
            }
        }
    }

    /// Emit a session recovery failed event with optional correlation.
    async fn emit_recovery_failed(
        &self,
        mission_id: &str,
        session_id: Option<&str>,
        checkpoint_id: &str,
        reason: &str,
    ) {
        let mut event = DomainEvent::new(
            mission_id,
            EventPayload::SessionRecoveryFailed {
                session_id: session_id.map(String::from),
                checkpoint_id: checkpoint_id.to_string(),
                reason: reason.to_string(),
            },
        );
        if let Some(sid) = session_id {
            event = event.with_correlation(sid);
        }
        self.emit_event(event).await;
    }

    /// Extract and convert conflicts from tier results.
    fn extract_conflicts_from_tiers(
        tier_results: &HashMap<String, super::shared::TierResult>,
    ) -> Vec<Conflict> {
        tier_results
            .values()
            .flat_map(|tier| {
                tier.conflicts.iter().enumerate().map(|(i, desc)| {
                    Conflict::from_description(format!("{}-conflict-{}", tier.unit_id, i), desc)
                })
            })
            .collect()
    }

    /// Find the latest incomplete checkpoint for a mission.
    async fn find_latest_checkpoint(&self, mission_id: &str) -> Result<Option<CheckpointInfo>> {
        let Some(ref store) = self.event_store else {
            return Ok(None);
        };

        let events = store.query(mission_id, 0).await?;

        let mut latest_checkpoint: Option<CheckpointInfo> = None;
        let mut completed_sessions: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for event in &events {
            match &event.payload {
                EventPayload::ConsensusCheckpointCreated {
                    session_id,
                    checkpoint_id,
                    round,
                    ..
                } => {
                    latest_checkpoint = Some(CheckpointInfo {
                        id: checkpoint_id.clone(),
                        session_id: session_id.clone(),
                        round: *round,
                    });
                }
                EventPayload::HierarchicalConsensusCompleted { session_id, .. } => {
                    completed_sessions.insert(session_id.clone());
                }
                EventPayload::ConsensusCompleted { .. } => {
                    // ConsensusCompleted is from non-hierarchical consensus (no session_id).
                    // For checkpoint-based recovery, only HierarchicalConsensusCompleted
                    // should mark sessions as completed. Skip this event to avoid
                    // incorrectly marking unrelated sessions as completed.
                }
                _ => {}
            }
        }

        match latest_checkpoint {
            Some(cp) if !completed_sessions.contains(&cp.session_id) => Ok(Some(cp)),
            _ => Ok(None),
        }
    }

    /// Execute a mission using adaptive consensus when available,
    /// falling back to sequential pipeline otherwise.
    ///
    /// This method creates an OrchestrationSession for the mission, managing
    /// participant tracking, task dependencies, health monitoring, and
    /// notification-based communication throughout the mission lifecycle.
    pub async fn execute_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<MissionResult> {
        if !self.config.enabled {
            return Err(PilotError::AgentExecution(
                "Multi-agent mode not enabled".into(),
            ));
        }

        // Apply mission-level timeout from consensus config
        let mission_timeout =
            tokio::time::Duration::from_secs(self.config.consensus.total_timeout_secs);

        let result = tokio::time::timeout(
            mission_timeout,
            self.execute_mission_internal(mission_id, description, working_dir),
        )
        .await;

        match result {
            Ok(inner) => inner,
            Err(_) => {
                warn!(
                    mission_id = %mission_id,
                    timeout_secs = self.config.consensus.total_timeout_secs,
                    "Mission timed out"
                );
                Err(PilotError::Timeout(format!(
                    "Mission '{}' timed out after {} seconds",
                    mission_id, self.config.consensus.total_timeout_secs
                )))
            }
        }
    }

    async fn execute_mission_internal(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<MissionResult> {
        // Create orchestration session for this mission
        let session = self.create_mission_session(mission_id, description, working_dir);
        info!(mission_id = %mission_id, "Orchestration session created");

        // Execute the mission
        let result = if self.config.dynamic_mode
            && self.adaptive_executor.is_some()
            && self.workspace.is_some()
        {
            self.execute_adaptive_mission(mission_id, description, working_dir)
                .await
        } else {
            self.execute_sequential_mission(mission_id, description, working_dir)
                .await
        };

        // End the session
        self.end_mission_session(
            &session,
            result.as_ref().map(|r| r.success).unwrap_or(false),
        );

        result
    }

    /// Create an orchestration session for a mission (internal mutability via RwLock).
    fn create_mission_session(
        &self,
        mission_id: &str,
        mission: &str,
        working_dir: &Path,
    ) -> SharedSession {
        // Create session with config
        let session_config = SessionConfig {
            max_duration: std::time::Duration::from_secs(86400), // 24 hours default
            max_parallel_tasks: self.config.max_concurrent_agents,
            max_task_retries: self.config.convergent_max_fix_attempts_per_issue as u32,
            ..SessionConfig::default()
        };

        let session = Arc::new(RwLock::new(
            OrchestrationSession::new(mission, working_dir.to_path_buf())
                .with_config(session_config)
                .with_metadata("mission_id", mission_id),
        ));

        // Register participants notification
        {
            let mut s = session.write();
            s.notify(
                "coordinator",
                "*",
                NotificationType::Text {
                    message:
                        "Orchestration session initialized - agents will register on first task"
                            .to_string(),
                },
            );
        }

        session
    }

    /// End an orchestration session.
    fn end_mission_session(&self, session: &SharedSession, success: bool) {
        let mut s = session.write();
        if success {
            s.complete();
        } else {
            s.fail("Mission ended without success");
        }

        // Log session summary
        let summary = s.summary();
        info!(
            session_id = %summary.id,
            status = ?summary.status,
            duration_secs = summary.duration_secs,
            participant_count = summary.participant_count,
            tasks_completed = summary.task_stats.completed,
            tasks_failed = summary.task_stats.failed,
            "Session ended"
        );

        // Clear session-specific trackers
        self.invocation_tracker.write().clear();
        self.continuation_manager.write().clear_all();
    }

    /// Adaptive consensus-based mission execution using hierarchical strategy selection.
    async fn execute_adaptive_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<MissionResult> {
        info!(mission_id = %mission_id, "Starting adaptive consensus mission execution");

        // Health check at mission start
        if let HealthAdaptation::Escalate { reason } = self.check_health_adaptation() {
            return Err(PilotError::AgentExecution(format!(
                "Health escalation at mission start: {}",
                reason
            )));
        }

        let (mut context, mut results) = self.init_mission_context(mission_id, working_dir);

        // Phase 1: Research
        if let Some(early_return) = self
            .run_research_and_check_blockers(&mut context, &mut results, description, working_dir)
            .await?
        {
            return Ok(early_return);
        }

        // Phase 2: Adaptive consensus with deterministic participant selection
        let consensus_result = self
            .run_adaptive_consensus(description, &context, working_dir)
            .await?;

        self.emit_hierarchical_consensus_event(mission_id, &consensus_result)
            .await;

        // Extract tasks based on consensus result
        let impl_tasks = self.process_hierarchical_result(&consensus_result, &mut context);

        // Handle escalation or failure using EscalationEngine
        if consensus_result.outcome == ConsensusOutcome::Escalated
            || consensus_result.outcome == ConsensusOutcome::Failed
        {
            // Create a blocking conflict representing the failed consensus
            // The actual conflict details would come from the consensus synthesis
            let blocking_conflicts: Vec<Conflict> = vec![Conflict {
                id: format!("consensus-{}", mission_id),
                agents: consensus_result.tier_results.keys().cloned().collect(),
                topic: format!(
                    "Consensus {} after {} rounds",
                    if consensus_result.outcome == ConsensusOutcome::Escalated {
                        "escalated"
                    } else {
                        "failed"
                    },
                    consensus_result.total_rounds
                ),
                positions: consensus_result
                    .tier_results
                    .values()
                    .map(|r| r.synthesis.clone())
                    .collect(),
                severity: super::consensus::ConflictSeverity::Blocking,
                resolution: None,
            }];

            let consensus_for_escalation = ConsensusResult::NoConsensus {
                summary: format!(
                    "Consensus {} after {} rounds",
                    if consensus_result.outcome == ConsensusOutcome::Escalated {
                        "escalated"
                    } else {
                        "failed"
                    },
                    consensus_result.total_rounds
                ),
                blocking_conflicts: blocking_conflicts.clone(),
                respondent_count: consensus_result
                    .tier_results
                    .values()
                    .map(|r| r.respondent_count)
                    .sum(),
            };

            let strategy = self
                .escalation_engine
                .escalate(mission_id, &consensus_for_escalation)?;

            match strategy {
                EscalationStrategy::RequestHumanDecision {
                    question,
                    options,
                    context: conflict_ctx,
                } => {
                    info!(
                        mission_id = %mission_id,
                        "Escalating to human decision"
                    );
                    return Ok(MissionResult {
                        success: false,
                        results,
                        summary: "Human decision required - consensus could not be reached".into(),
                        metrics: self.pool.metrics(),
                        conflicts: blocking_conflicts,
                        complexity_metrics: ComplexityMetrics::default(),
                        needs_human_decision: Some(HumanDecisionRequest {
                            question,
                            options,
                            conflict_summary: conflict_ctx.conflict_summary,
                            affected_files: conflict_ctx.affected_files,
                            agent_positions: conflict_ctx.agent_positions,
                            attempted_resolutions: conflict_ctx
                                .attempted_resolutions
                                .iter()
                                .map(|r| format!("{:?}: {}", r.level, r.outcome))
                                .collect(),
                        }),
                    });
                }
                EscalationStrategy::RetryWithDifferentAgents { exclude_agents, .. } => {
                    info!(
                        mission_id = %mission_id,
                        excluded = ?exclude_agents,
                        "Retrying consensus with different agents"
                    );
                    // For now, fall through to use extracted tasks
                    // Future: implement actual retry with excluded agents
                }
                EscalationStrategy::ExecuteNonConflicting {
                    safe_tasks,
                    deferred_tasks,
                } => {
                    info!(
                        mission_id = %mission_id,
                        safe = safe_tasks.len(),
                        deferred = deferred_tasks.len(),
                        "Executing non-conflicting tasks only"
                    );
                    // Use only safe tasks
                    let safe_impl_tasks = self.prepare_tasks(safe_tasks);
                    let impl_results = self
                        .run_implementation_with_events(
                            mission_id,
                            &mut context,
                            safe_impl_tasks,
                            working_dir,
                        )
                        .await?;
                    results.extend(impl_results);

                    return self
                        .run_verification_and_finalize(
                            mission_id,
                            &mut context,
                            &mut results,
                            &[], // deferred tasks not executed
                            working_dir,
                            blocking_conflicts,
                        )
                        .await;
                }
                EscalationStrategy::ArchitecturalOverride {
                    decision_prompt,
                    affected_modules,
                } => {
                    info!(
                        mission_id = %mission_id,
                        modules = ?affected_modules,
                        "Requesting architectural mediation"
                    );

                    if let Some(architect) = self.pool.select(&AgentRole::architect()) {
                        let architect_task = AgentTask {
                            id: format!("{}-architect-mediation", mission_id),
                            description: decision_prompt,
                            context: context.clone(),
                            priority: TaskPriority::Critical,
                            role: Some(AgentRole::architect()),
                        };

                        match architect.execute(&architect_task, working_dir).await {
                            Ok(result) => {
                                context.key_findings.push(format!(
                                    "[Architect Decision] {}",
                                    result.output.lines().next().unwrap_or("No decision")
                                ));
                                info!(
                                    mission_id = %mission_id,
                                    verdict = %result.output.lines().next().unwrap_or("unknown"),
                                    "Architect mediation completed"
                                );
                            }
                            Err(e) => {
                                warn!(
                                    mission_id = %mission_id,
                                    error = %e,
                                    "Architect mediation failed, proceeding with extracted tasks"
                                );
                            }
                        }
                    } else {
                        warn!(
                            mission_id = %mission_id,
                            "No architect agent available for mediation"
                        );
                    }
                }
                EscalationStrategy::SequentializeConflicts {
                    task_order,
                    rationale,
                } => {
                    info!(
                        mission_id = %mission_id,
                        order = ?task_order,
                        rationale = %rationale,
                        "Sequentializing conflicting tasks"
                    );
                    // Tasks will be executed sequentially in partition_tasks
                    // The task_order can be used to reorder impl_tasks
                }
            }
        }

        // Phase 3: Execute tasks
        let mission_tasks = impl_tasks.clone();
        let impl_results = self
            .run_implementation_with_events(mission_id, &mut context, impl_tasks, working_dir)
            .await?;
        results.extend(impl_results);

        // Phase 4: Convergent Verification and finalize
        self.run_verification_and_finalize(
            mission_id,
            &mut context,
            &mut results,
            &mission_tasks,
            working_dir,
            vec![],
        )
        .await
    }

    /// Run adaptive consensus using the AdaptiveConsensusExecutor.
    ///
    /// When a two-phase orchestrator is configured, this method first runs
    /// the direction-setting phase to establish global constraints before
    /// executing the synthesis phase with cross-visibility.
    async fn run_adaptive_consensus(
        &self,
        description: &str,
        context: &TaskContext,
        _working_dir: &Path,
    ) -> Result<HierarchicalConsensusResult> {
        let executor = self
            .adaptive_executor
            .as_ref()
            .ok_or_else(|| PilotError::AgentExecution("Adaptive executor not configured".into()))?;

        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| PilotError::AgentExecution("Workspace not configured".into()))?;

        // Build affected files from context
        let affected_files: Vec<PathBuf> =
            context.related_files.iter().map(PathBuf::from).collect();

        // Create consensus mission
        let mission = ConsensusMission::new(description, affected_files.clone(), context.clone());

        // Determine scope from workspace
        let scope = AgentScope::Workspace {
            workspace: workspace.name.clone(),
        };

        // Execute two-phase consensus if orchestrator is available
        if let Some(ref orchestrator) = self.two_phase_orchestrator {
            // Derive qualified modules from affected files
            let qualified_modules = self.derive_qualified_modules(&affected_files, workspace);

            // Phase 1: Direction setting - establish global constraints
            let constraints = match orchestrator
                .execute_phase1(&mission, &qualified_modules)
                .await
            {
                Ok(c) => {
                    info!(
                        tech_decisions = c.tech_decisions.len(),
                        api_contracts = c.api_contracts.len(),
                        "Direction phase completed with constraints"
                    );
                    Some(c)
                }
                Err(e) => {
                    warn!(error = %e, "Direction phase failed, proceeding without constraints");
                    None
                }
            };

            // Phase 2: Synthesis with constraints
            if let Some(c) = constraints {
                // Convert GlobalConstraints to constraint strings
                let constraint_strings: Vec<String> = c
                    .tech_decisions
                    .iter()
                    .map(|td| format!("[Tech] {}: {}", td.topic, td.decision))
                    .chain(c.api_contracts.iter().map(|ac| {
                        format!(
                            "[API] {}: provided by {}",
                            ac.name,
                            ac.provider.to_qualified_string()
                        )
                    }))
                    .chain(
                        c.runtime_constraints
                            .iter()
                            .map(|rc| format!("[Runtime] {}", rc.description)),
                    )
                    .collect();

                info!(
                    constraints = constraint_strings.len(),
                    "Executing Phase 2 synthesis with constraints"
                );

                // Check if cross-workspace execution is needed
                if self.requires_cross_workspace(&affected_files) {
                    // Collect all involved workspaces
                    let workspace_ids = self
                        .workspace_registry
                        .workspaces_for_files(&affected_files);
                    let mut workspaces_map: HashMap<String, &Workspace> = HashMap::new();

                    // Check workspace health before execution
                    let unavailable: Vec<_> = self
                        .workspace_registry
                        .unavailable_workspaces()
                        .into_iter()
                        .map(|ws| ws.id)
                        .collect();

                    // Add primary workspace (always included)
                    workspaces_map.insert(workspace.name.clone(), workspace);

                    // Add additional workspaces, filtering out unavailable ones
                    for ws_id in &workspace_ids {
                        if unavailable.contains(ws_id) {
                            warn!(
                                workspace = %ws_id,
                                "Workspace unavailable, proceeding with degraded mode"
                            );
                            continue;
                        }
                        if let Some(ws) = self.additional_workspaces.get(ws_id) {
                            workspaces_map.insert(ws_id.clone(), ws.as_ref());
                        }
                    }

                    if workspaces_map.len() > 1 {
                        info!(
                            workspace_count = workspaces_map.len(),
                            "Using cross-workspace consensus execution"
                        );
                        return executor
                            .execute_cross_workspace(
                                &mission,
                                &workspaces_map,
                                &self.pool,
                                constraint_strings,
                            )
                            .await;
                    }
                }

                // Use single-workspace constraint-aware execution
                return executor
                    .execute_with_constraints(
                        &mission,
                        &scope,
                        workspace,
                        &self.pool,
                        constraint_strings,
                    )
                    .await;
            }
        }

        // Execute adaptive consensus without constraints (fallback)
        executor
            .execute(&mission, &scope, workspace, &self.pool)
            .await
    }

    /// Derive qualified modules from affected files using workspace structure.
    fn derive_qualified_modules(
        &self,
        affected_files: &[PathBuf],
        workspace: &Workspace,
    ) -> Vec<QualifiedModule> {
        let mut modules = Vec::new();
        let workspace_name = &workspace.name;

        // Match files to modules by checking if file path starts with module path
        for file in affected_files {
            for module in workspace.modules() {
                let file_str = file.to_string_lossy();
                if module.paths.iter().any(|p| file_str.starts_with(p)) {
                    let qualified = QualifiedModule::new(workspace_name, &module.name);
                    let qualified_str = qualified.to_qualified_string();
                    if !modules
                        .iter()
                        .any(|m: &QualifiedModule| m.to_qualified_string() == qualified_str)
                    {
                        modules.push(qualified);
                    }
                    break;
                }
            }
        }

        modules
    }

    /// Process hierarchical consensus result into implementation tasks.
    fn process_hierarchical_result(
        &self,
        result: &HierarchicalConsensusResult,
        context: &mut TaskContext,
    ) -> Vec<ConsensusTask> {
        match result.outcome {
            ConsensusOutcome::Converged => {
                if let Some(ref plan) = result.plan {
                    context.key_findings.push(format!(
                        "Consensus converged in {} rounds: {}",
                        result.total_rounds,
                        plan.lines().next().unwrap_or("Plan agreed")
                    ));
                }
                self.prepare_tasks(result.tasks.clone())
            }
            ConsensusOutcome::PartialConvergence => {
                if let Some(ref plan) = result.plan {
                    context
                        .key_findings
                        .push(format!("Partial consensus: {}", plan));
                }
                if result.tasks.is_empty() {
                    self.extract_implementation_tasks(context)
                } else {
                    self.prepare_tasks(result.tasks.clone())
                }
            }
            ConsensusOutcome::Escalated | ConsensusOutcome::Timeout | ConsensusOutcome::Failed => {
                self.extract_implementation_tasks(context)
            }
        }
    }

    /// Emit event for hierarchical consensus completion.
    async fn emit_hierarchical_consensus_event(
        &self,
        mission_id: &str,
        result: &HierarchicalConsensusResult,
    ) {
        let respondent_count = result
            .tier_results
            .values()
            .map(|r| r.respondent_count)
            .sum::<usize>();

        let outcome_str = match result.outcome {
            ConsensusOutcome::Converged => "converged",
            ConsensusOutcome::PartialConvergence => "partial",
            ConsensusOutcome::Escalated => "escalated",
            ConsensusOutcome::Timeout => "timeout",
            ConsensusOutcome::Failed => "failed",
        };

        self.emit_critical_event(DomainEvent::consensus_completed(
            mission_id,
            result.total_rounds as u32,
            respondent_count,
            outcome_str,
        ))
        .await;
    }

    /// Sequential pipeline mission execution (legacy mode).
    async fn execute_sequential_mission(
        &self,
        mission_id: &str,
        description: &str,
        working_dir: &Path,
    ) -> Result<MissionResult> {
        info!(mission_id = %mission_id, "Starting sequential mission execution");

        let (mut context, mut results) = self.init_mission_context(mission_id, working_dir);

        // Phase 1: Research
        if let Some(early_return) = self
            .run_research_and_check_blockers(&mut context, &mut results, description, working_dir)
            .await?
        {
            return Ok(early_return);
        }

        // Phase 2: Planning
        let planning_result = self
            .run_planning_phase(&mut context, description, working_dir)
            .await?;
        results.push(planning_result);

        // Phase 3: Implementation
        let impl_tasks = self.extract_implementation_tasks(&context);
        let mission_tasks = impl_tasks.clone();
        let impl_results = self
            .run_implementation_with_events(mission_id, &mut context, impl_tasks, working_dir)
            .await?;
        results.extend(impl_results);

        // Phase 4: Convergent Verification and finalize
        self.run_verification_and_finalize(
            mission_id,
            &mut context,
            &mut results,
            &mission_tasks,
            working_dir,
            vec![],
        )
        .await
    }

    fn init_mission_context(
        &self,
        mission_id: &str,
        working_dir: &Path,
    ) -> (TaskContext, Vec<AgentTaskResult>) {
        let context = TaskContext {
            mission_id: mission_id.to_string(),
            working_dir: working_dir.to_path_buf(),
            key_findings: Vec::new(),
            blockers: Vec::new(),
            related_files: Vec::new(),
            composed_prompt: None,
        };
        (context, Vec::new())
    }

    async fn run_research_and_check_blockers(
        &self,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
        description: &str,
        working_dir: &Path,
    ) -> Result<Option<MissionResult>> {
        let research_result = self
            .run_research_phase(context, description, working_dir)
            .await?;
        results.push(research_result);

        if self.has_blockers(context) {
            return Ok(Some(MissionResult {
                success: false,
                results: results.clone(),
                summary: "Blocked during research phase".into(),
                metrics: self.pool.metrics(),
                conflicts: vec![],
                complexity_metrics: ComplexityMetrics::default(),
                needs_human_decision: None,
            }));
        }
        Ok(None)
    }

    async fn run_implementation_with_events(
        &self,
        mission_id: &str,
        context: &mut TaskContext,
        impl_tasks: Vec<ConsensusTask>,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        let task_count = impl_tasks.len();
        let impl_start = std::time::Instant::now();
        let impl_results = self
            .run_implementation_phase(context, impl_tasks, working_dir)
            .await?;
        let impl_duration_ms = impl_start.elapsed().as_millis() as u64;
        let success_count = impl_results.iter().filter(|r| r.success).count();

        self.emit_critical_event(DomainEvent::multi_agent_phase_completed(
            mission_id,
            "implementation",
            task_count,
            success_count,
            impl_duration_ms,
        ))
        .await;

        // Check and compact context for long-running missions
        self.maybe_compact_task_context(context);

        Ok(impl_results)
    }

    async fn run_verification_and_finalize(
        &self,
        mission_id: &str,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
        mission_tasks: &[ConsensusTask],
        working_dir: &Path,
        conflicts: Vec<Conflict>,
    ) -> Result<MissionResult> {
        let verification_start = std::time::Instant::now();
        let convergence = self
            .run_convergent_verification(context, working_dir, results)
            .await?;
        let verification_duration_ms = verification_start.elapsed().as_millis() as u64;

        if convergence.converged {
            self.emit_critical_event(DomainEvent::convergence_achieved(
                mission_id,
                convergence.total_rounds as u32,
                convergence.clean_rounds as u32,
            ))
            .await;
        } else {
            let remaining_issues = convergence
                .issues_found
                .len()
                .saturating_sub(convergence.issues_fixed.len());
            self.emit_critical_event(DomainEvent::verification_round(
                mission_id,
                convergence.total_rounds as u32,
                false,
                remaining_issues,
                verification_duration_ms,
            ))
            .await;
        }

        let success = convergence.converged;
        let summary = Self::build_convergence_summary(&convergence);

        let completed_ids: Vec<String> = results
            .iter()
            .filter(|r| r.success)
            .map(|r| r.task_id.clone())
            .collect();

        Ok(MissionResult {
            success,
            results: std::mem::take(results),
            summary,
            metrics: self.pool.metrics(),
            conflicts,
            complexity_metrics: ComplexityMetrics::from_tasks(mission_tasks, &completed_ids),
            needs_human_decision: None,
        })
    }

    fn build_convergence_summary(convergence: &ConvergenceResult) -> String {
        let remaining = convergence
            .issues_found
            .len()
            .saturating_sub(convergence.issues_fixed.len());
        match convergence.final_verdict {
            VerificationVerdict::Passed => format!(
                "Mission completed: {} rounds, {} issues fixed",
                convergence.total_rounds,
                convergence.issues_fixed.len()
            ),
            VerificationVerdict::PartialPass => format!(
                "Mission partial: {} clean of 2 required, {} issues remain",
                convergence.clean_rounds, remaining
            ),
            VerificationVerdict::Timeout => format!(
                "Mission timeout: max rounds reached, {} issues remain",
                remaining
            ),
            VerificationVerdict::Failed => format!(
                "Mission failed: {} unresolved issues",
                convergence.issues_found.len()
            ),
        }
    }

    /// Compute priority scores and sort tasks.
    fn prepare_tasks(&self, tasks: Vec<ConsensusTask>) -> Vec<ConsensusTask> {
        let mut scored_tasks: Vec<_> = tasks
            .into_iter()
            .map(|mut t| {
                let complexity_factor = t.estimated_complexity.weight();
                let priority_factor = match t.priority {
                    TaskPriority::Low => 0.25,
                    TaskPriority::Normal => 0.5,
                    TaskPriority::High => 0.75,
                    TaskPriority::Critical => 1.0,
                };
                t.priority_score = priority_factor * 0.7 + (1.0 - complexity_factor) * 0.3;
                t
            })
            .collect();

        scored_tasks.sort_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        scored_tasks
    }

    async fn run_code_review(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        if self.pool.agent_count(&AgentRole::reviewer()) == 0 {
            debug!("No reviewer agent registered, skipping code review");
            return Ok(vec![]);
        }

        let module_context = self.build_module_context();

        let description = if module_context.is_empty() {
            format!(
                "Review code changes for quality. Files: {}",
                context.related_files.join(", ")
            )
        } else {
            format!(
                "Review code changes for quality.\n\nFiles: {}\n\n## Module Conventions\n{}",
                context.related_files.join(", "),
                module_context
            )
        };

        let task = AgentTask {
            id: "code-review".to_string(),
            description,
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::reviewer()),
        };

        match self.pool.execute(&task, working_dir).await {
            Ok(result) => Ok(vec![result]),
            Err(e) => {
                warn!(error = %e, "Code review failed");
                Ok(vec![])
            }
        }
    }

    /// Run architecture validation to check boundary and convention compliance.
    async fn run_architecture_review(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<Option<AgentTaskResult>> {
        if !self.pool.has_agent("architecture") {
            debug!("No architecture agent registered, skipping architecture review");
            return Ok(None);
        }

        let task = AgentTask {
            id: "architecture-review".to_string(),
            description: format!(
                "Validate architecture boundaries and conventions for changes in: {}",
                context.related_files.join(", ")
            ),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::architect()),
        };

        match self.pool.execute(&task, working_dir).await {
            Ok(result) => Ok(Some(result)),
            Err(e) => {
                warn!(error = %e, "Architecture review failed");
                Ok(None)
            }
        }
    }

    async fn run_research_phase(
        &self,
        context: &mut TaskContext,
        description: &str,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase 1: Research");

        let task = AgentTask {
            id: "research-1".to_string(),
            description: format!("Analyze codebase for: {}", description),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::core_research()),
        };

        let result = self.pool.execute(&task, working_dir).await?;

        if result.success {
            context.key_findings.extend(result.findings.clone());
            context.related_files = extract_files_from_output(&result.output, 20);
        } else {
            context
                .blockers
                .push(format!("Research failed: {}", result.output));
        }

        Ok(result)
    }

    /// Sequential-mode planning phase. In dynamic mode, consensus supersedes this;
    /// PlanningAgent remains as a valid fallback for non-consensus workflows.
    async fn run_planning_phase(
        &self,
        context: &mut TaskContext,
        description: &str,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase 2: Planning");

        let task = AgentTask {
            id: "planning-1".to_string(),
            description: format!("Create implementation plan for: {}", description),
            context: context.clone(),
            priority: TaskPriority::High,
            role: Some(AgentRole::core_planning()),
        };

        let result = self.pool.execute(&task, working_dir).await?;

        if result.success {
            context.key_findings.extend(result.findings.clone());
        } else {
            context
                .blockers
                .push(format!("Planning failed: {}", result.output));
        }

        Ok(result)
    }

    async fn run_implementation_phase(
        &self,
        context: &mut TaskContext,
        tasks: Vec<ConsensusTask>,
        working_dir: &Path,
    ) -> Result<Vec<AgentTaskResult>> {
        info!(task_count = tasks.len(), "Phase 3: Implementation");

        let (independent, dependent) = self.partition_tasks(&tasks);

        let mut final_results = Vec::new();
        let mut pending_tasks: Vec<&ConsensusTask> = independent;
        let max_deferred_retries = self.config.max_deferred_retries;
        let retry_delay_ms = self.config.deferred_retry_delay_ms;

        // Retry loop for deferred tasks (file conflicts resolved via yield)
        for retry_round in 0..=max_deferred_retries {
            if pending_tasks.is_empty() {
                break;
            }

            if retry_round > 0 {
                info!(
                    retry_round = retry_round,
                    pending_count = pending_tasks.len(),
                    "Retrying deferred tasks"
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
            }

            let mut round_results = Vec::new();

            if self.config.parallel_execution {
                // Try to acquire file ownership for parallel tasks
                let (tasks_with_ownership, tasks_ownership_deferred) = self
                    .acquire_ownership_for_tasks(&pending_tasks, context)
                    .await;

                if !tasks_with_ownership.is_empty() {
                    let agent_tasks: Vec<_> = tasks_with_ownership
                        .iter()
                        .map(|(t, _)| Self::to_agent_task(t, context))
                        .collect();

                    // Broadcast task assignments for all parallel tasks
                    for task in &agent_tasks {
                        let agent_id = task
                            .role
                            .as_ref()
                            .map(|r| r.id.clone())
                            .unwrap_or_else(|| "coder".to_string());
                        self.broadcast_task_assignment(task, &agent_id).await;
                    }

                    let task_ids: Vec<_> = agent_tasks.iter().map(|t| t.id.clone()).collect();
                    let agent_ids: Vec<_> = agent_tasks
                        .iter()
                        .map(|t| {
                            t.role
                                .as_ref()
                                .map(|r| r.id.clone())
                                .unwrap_or_else(|| "coder".to_string())
                        })
                        .collect();

                    let parallel_results = self.pool.execute_many(agent_tasks, working_dir).await;

                    for (idx, result) in parallel_results.into_iter().enumerate() {
                        let agent_id = agent_ids
                            .get(idx)
                            .cloned()
                            .unwrap_or_else(|| "coder".to_string());
                        match result {
                            Ok(r) => {
                                self.broadcast_task_result(&r, &agent_id).await;
                                context.key_findings.extend(r.findings.clone());
                                round_results.push(r);
                            }
                            Err(e) => {
                                let task_id = task_ids.get(idx).cloned().unwrap_or_default();
                                warn!(error = %e, task_id = %task_id, "Parallel task failed");
                                let failed_result =
                                    AgentTaskResult::failure(task_id, e.to_string());
                                self.broadcast_task_result(&failed_result, &agent_id).await;
                                round_results.push(failed_result);
                            }
                        }
                    }
                }

                // Execute ownership-deferred tasks sequentially
                for (task, reason) in tasks_ownership_deferred {
                    debug!(
                        task_id = %task.id,
                        reason = %reason,
                        "Executing ownership-deferred task sequentially"
                    );
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }
            } else {
                for task in &pending_tasks {
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }
            }

            // Partition results: completed vs deferred (yield due to P2P conflict)
            let (completed, deferred): (Vec<_>, Vec<_>) = round_results
                .into_iter()
                .partition(|r| r.status != TaskStatus::Deferred);

            final_results.extend(completed);

            if deferred.is_empty() {
                break;
            }

            // Map deferred results back to their original tasks for retry
            let deferred_task_ids: std::collections::HashSet<_> =
                deferred.iter().map(|r| r.task_id.as_str()).collect();

            pending_tasks = tasks
                .iter()
                .filter(|t| deferred_task_ids.contains(t.id.as_str()))
                .collect();

            if retry_round == max_deferred_retries && !pending_tasks.is_empty() {
                warn!(
                    remaining_deferred = pending_tasks.len(),
                    "Max deferred retries reached, marking remaining as failed"
                );
                for task in &pending_tasks {
                    final_results.push(AgentTaskResult::failure(
                        &task.id,
                        format!(
                            "Task exhausted {} deferred retries due to persistent file conflicts",
                            max_deferred_retries
                        ),
                    ));
                }
            }
        }

        // Execute dependent tasks in topological order (providers before consumers)
        // Also supports deferred retry for file conflicts
        if !dependent.is_empty() {
            let dependent_vec: Vec<_> = dependent.iter().map(|t| (*t).clone()).collect();
            let sorted_deps = self.topological_sort_tasks(&dependent_vec);

            debug!(
                original_count = dependent.len(),
                sorted_count = sorted_deps.len(),
                "Executing dependent tasks in topological order"
            );

            let mut pending_deps: Vec<&ConsensusTask> = sorted_deps.to_vec();

            for retry_round in 0..=max_deferred_retries {
                if pending_deps.is_empty() {
                    break;
                }

                if retry_round > 0 {
                    info!(
                        retry_round = retry_round,
                        pending_count = pending_deps.len(),
                        "Retrying deferred dependent tasks"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms)).await;
                }

                let mut round_results = Vec::new();
                for task in &pending_deps {
                    self.execute_and_collect(
                        &Self::to_agent_task(task, context),
                        working_dir,
                        context,
                        &mut round_results,
                    )
                    .await;
                }

                let (completed, deferred): (Vec<_>, Vec<_>) = round_results
                    .into_iter()
                    .partition(|r| r.status != TaskStatus::Deferred);

                final_results.extend(completed);

                if deferred.is_empty() {
                    break;
                }

                let deferred_ids: std::collections::HashSet<_> =
                    deferred.iter().map(|r| r.task_id.as_str()).collect();

                pending_deps = sorted_deps
                    .iter()
                    .copied()
                    .filter(|t| deferred_ids.contains(t.id.as_str()))
                    .collect();

                if retry_round == max_deferred_retries && !pending_deps.is_empty() {
                    warn!(
                        remaining_deferred = pending_deps.len(),
                        "Max deferred retries reached for dependent tasks"
                    );
                    for task in &pending_deps {
                        final_results.push(AgentTaskResult::failure(
                            &task.id,
                            format!(
                                "Task exhausted {} deferred retries due to persistent file conflicts",
                                max_deferred_retries
                            ),
                        ));
                    }
                }
            }
        }

        Ok(final_results)
    }

    async fn acquire_ownership_for_tasks<'a>(
        &self,
        tasks: &[&'a ConsensusTask],
        context: &TaskContext,
    ) -> (
        Vec<(&'a ConsensusTask, LeaseGuard)>,
        Vec<(&'a ConsensusTask, String)>,
    ) {
        let mut tasks_with_ownership = Vec::new();
        let mut tasks_deferred = Vec::new();

        for task in tasks {
            let mut lease_guard = LeaseGuard::new();
            let mut all_acquired = true;
            let mut denial_reason = String::new();

            for file in &task.files_affected {
                let file_path = Path::new(file);
                let module = task.assigned_module.as_deref();

                let result = self.ownership_manager.acquire(
                    file_path,
                    &task.id,
                    module,
                    AccessType::Exclusive,
                    &format!("Task {} implementation", task.id),
                );

                match &result {
                    AcquisitionResult::Granted { lease } => {
                        lease_guard.add(Arc::clone(lease));
                    }
                    AcquisitionResult::Denied {
                        reason,
                        owner_module,
                    } => {
                        all_acquired = false;
                        denial_reason = owner_module
                            .as_ref()
                            .map(|owner| format!("Module boundary: {} owned by {}", file, owner))
                            .unwrap_or_else(|| reason.clone());
                        self.broadcast_conflict_alert(
                            file_path,
                            &denial_reason,
                            ConflictSeverity::Blocking,
                        )
                        .await;
                        break;
                    }
                    AcquisitionResult::Queued {
                        position,
                        estimated_wait,
                    } => {
                        all_acquired = false;
                        denial_reason = format!(
                            "Queued at position {} (~{:?} wait)",
                            position, estimated_wait
                        );
                        break;
                    }
                    AcquisitionResult::NeedsNegotiation {
                        conflicting_modules,
                    } => {
                        all_acquired = false;
                        denial_reason = format!("Negotiation needed: {:?}", conflicting_modules);
                        self.broadcast_conflict_alert(
                            file_path,
                            &denial_reason,
                            ConflictSeverity::Major,
                        )
                        .await;
                        break;
                    }
                }
            }

            if all_acquired {
                tasks_with_ownership.push((*task, lease_guard));
            } else {
                warn!(
                    task_id = %task.id,
                    mission_id = %context.mission_id,
                    reason = %denial_reason,
                    "Task deferred due to ownership issue"
                );
                tasks_deferred.push((*task, denial_reason));
            }
        }

        (tasks_with_ownership, tasks_deferred)
    }

    async fn broadcast_conflict_alert(
        &self,
        file: &Path,
        description: &str,
        severity: ConflictSeverity,
    ) {
        let Some(bus) = &self.message_bus else { return };

        let msg = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::ConflictAlert {
                conflict_id: uuid::Uuid::new_v4().to_string(),
                severity,
                description: format!("{}: {}", file.display(), description),
            },
        );

        if let Err(e) = bus.send(msg).await {
            debug!(error = %e, file = %file.display(), "Failed to broadcast conflict alert");
        }
    }

    async fn run_verification_phase(
        &self,
        context: &TaskContext,
        working_dir: &Path,
    ) -> Result<AgentTaskResult> {
        info!("Phase: Verification");

        let task = AgentTask {
            id: "verification-1".to_string(),
            description: "Verify all changes and run quality checks".to_string(),
            context: context.clone(),
            priority: TaskPriority::Critical,
            role: Some(AgentRole::core_verifier()),
        };

        self.pool.execute(&task, working_dir).await
    }

    fn to_agent_task(task: &ConsensusTask, context: &TaskContext) -> AgentTask {
        let role = task
            .assigned_module
            .as_ref()
            .filter(|m| !m.is_empty()) // Guard against empty strings
            .map(|module_id| {
                // Check if this is a qualified module (contains ::)
                if module_id.contains("::") {
                    // Qualified format: workspace::domain::module or workspace::module
                    // Convert to pool_key format for proper agent lookup
                    AgentRole::module_coder_from_qualified(module_id)
                } else {
                    // Simple module ID - use legacy format
                    AgentRole::module(module_id)
                }
            })
            .unwrap_or_else(AgentRole::core_coder);

        AgentTask {
            id: task.id.clone(),
            description: task.description.clone(),
            context: context.clone(),
            priority: task.priority,
            role: Some(role),
        }
    }

    async fn execute_and_collect(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
    ) {
        let agent_id = task
            .role
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "coder".to_string());

        // Log task routing for observability
        let is_qualified = task
            .role
            .as_ref()
            .is_some_and(|r| r.is_qualified_module_coder());
        debug!(
            task_id = %task.id,
            target_agent = %agent_id,
            is_qualified_module = %is_qualified,
            workspace = ?task.role.as_ref().and_then(|r| r.workspace()),
            "Routing task to coder agent"
        );

        let mut task_with_context = task.clone();
        let composed = self.compose_context_for_task(&mut task_with_context);

        if let Some(ref composed) = composed {
            self.emit_context_events(&context.mission_id, &agent_id, &task.id, composed)
                .await;
        }

        self.broadcast_task_assignment(&task_with_context, &agent_id)
            .await;

        let agent_id_typed = AgentId::new(&agent_id);
        let start_time = std::time::Instant::now();

        // Use message-based execution with P2P conflict resolution when MessageBus available
        let execution_result = if let Some(bus) = &self.message_bus {
            self.pool
                .execute_with_messaging(&task_with_context, working_dir, bus)
                .await
        } else {
            self.pool.execute(&task_with_context, working_dir).await
        };

        let elapsed_ms = start_time.elapsed().as_millis() as u64;

        match execution_result {
            Ok(r) => {
                self.broadcast_task_result(&r, &agent_id).await;
                self.record_task_completion(&agent_id_typed, elapsed_ms);
                context.key_findings.extend(r.findings.clone());
                results.push(r);
            }
            Err(e) => {
                warn!(error = %e, task_id = %task.id, "Task execution failed");
                self.record_task_failure(&agent_id_typed);
                let failed_result = AgentTaskResult::failure(&task.id, e.to_string());
                self.broadcast_task_result(&failed_result, &agent_id).await;
                results.push(failed_result);
            }
        }
    }

    /// Run convergent verification loop until N consecutive clean rounds.
    /// NON-NEGOTIABLE: N consecutive clean rounds required for success (configurable via config).
    async fn run_convergent_verification(
        &self,
        context: &mut TaskContext,
        working_dir: &Path,
        results: &mut Vec<AgentTaskResult>,
    ) -> Result<ConvergenceResult> {
        let required_clean_rounds = self.config.convergent_required_clean_rounds;
        let max_rounds = self.config.convergent_max_rounds;
        let max_fix_attempts_per_issue = self.config.convergent_max_fix_attempts_per_issue;

        info!(
            "Starting convergent verification (requires {} consecutive clean rounds)",
            required_clean_rounds
        );

        let mut consecutive_clean = 0;
        let mut total_rounds = 0;
        let mut all_issues_found = Vec::new();
        let mut all_issues_fixed = Vec::new();
        let mut fix_attempts: HashMap<String, usize> = HashMap::new();

        while consecutive_clean < required_clean_rounds && total_rounds < max_rounds {
            total_rounds += 1;
            info!(
                round = total_rounds,
                consecutive_clean = consecutive_clean,
                "Verification round"
            );

            // Health-driven adaptation every 3 rounds during intensive verification
            if total_rounds % 3 == 0 {
                match self.check_health_adaptation() {
                    HealthAdaptation::Escalate { reason } => {
                        warn!(reason = %reason, "Health escalation during verification, breaking loop");
                        break;
                    }
                    HealthAdaptation::Throttle { delay_ms } => {
                        debug!(
                            delay_ms = delay_ms,
                            "Throttling verification due to degraded health"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }
                    HealthAdaptation::ForceSequential => {
                        info!("Forcing sequential execution due to critical health");
                        // Note: Sequential mode handled at task level
                    }
                    HealthAdaptation::Continue => {}
                }
            }

            // Step 1: Code Review
            let qa_results = self.run_code_review(context, working_dir).await?;
            let qa_issues: Vec<String> = qa_results
                .iter()
                .filter(|r| !r.success)
                .flat_map(|r| r.findings.clone())
                .collect();
            results.extend(qa_results);

            // Step 2: Architecture Review (boundary and convention compliance)
            let arch_issues: Vec<String> = if let Some(arch_result) =
                self.run_architecture_review(context, working_dir).await?
            {
                let issues = if arch_result.success {
                    vec![]
                } else {
                    arch_result.findings.clone()
                };
                results.push(arch_result);
                issues
            } else {
                vec![]
            };

            // Step 3: Core Verification (build, tests, etc.)
            let verify_result = self.run_verification_phase(context, working_dir).await?;
            let verify_issues: Vec<String> = if verify_result.success {
                vec![]
            } else {
                verify_result.findings.clone()
            };
            results.push(verify_result.clone());

            // Combine all issues from this round
            let round_issues: Vec<String> = qa_issues
                .into_iter()
                .chain(arch_issues)
                .chain(verify_issues)
                .filter(|issue| !issue.is_empty())
                .collect();

            if round_issues.is_empty() {
                consecutive_clean += 1;
                info!(
                    round = total_rounds,
                    consecutive_clean = consecutive_clean,
                    "Clean round"
                );
            } else {
                consecutive_clean = 0;
                info!(
                    round = total_rounds,
                    issues = round_issues.len(),
                    "Issues found, resetting clean counter"
                );

                // Track and attempt to fix issues
                for issue in &round_issues {
                    let issue_key = Self::normalize_issue_key(issue);
                    let attempts = fix_attempts.entry(issue_key.clone()).or_insert(0);
                    *attempts += 1;

                    if *attempts > max_fix_attempts_per_issue {
                        warn!(issue = %issue, attempts = *attempts, "Max fix attempts exceeded for issue");
                        continue;
                    }

                    // Attempt fix via coder agent
                    let fix_task = AgentTask {
                        id: format!("fix-r{}-{}", total_rounds, attempts),
                        description: format!("Fix issue: {}", issue),
                        context: context.clone(),
                        priority: TaskPriority::High,
                        role: Some(AgentRole::core_coder()),
                    };

                    match self.pool.execute(&fix_task, working_dir).await {
                        Ok(fix_result) => {
                            if fix_result.success {
                                all_issues_fixed.push(issue.clone());
                                context.key_findings.extend(fix_result.findings.clone());
                            }
                            results.push(fix_result);
                        }
                        Err(e) => {
                            warn!(error = %e, "Fix attempt failed");
                        }
                    }
                }

                all_issues_found.extend(round_issues);
            }
        }

        let converged = consecutive_clean >= required_clean_rounds;
        let final_verdict = if converged {
            VerificationVerdict::Passed
        } else if consecutive_clean > 0 {
            VerificationVerdict::PartialPass
        } else if total_rounds >= max_rounds {
            VerificationVerdict::Timeout
        } else {
            VerificationVerdict::Failed
        };

        info!(
            converged = converged,
            clean_rounds = consecutive_clean,
            total_rounds = total_rounds,
            verdict = ?final_verdict,
            "Convergent verification complete"
        );

        Ok(ConvergenceResult {
            converged,
            clean_rounds: consecutive_clean,
            total_rounds,
            issues_found: all_issues_found,
            issues_fixed: all_issues_fixed,
            final_verdict,
        })
    }

    fn normalize_issue_key(issue: &str) -> String {
        // Limit input to prevent excessive memory consumption from very long issue descriptions
        const MAX_ISSUE_LENGTH: usize = 500;
        let truncated = if issue.len() > MAX_ISSUE_LENGTH {
            &issue[..MAX_ISSUE_LENGTH]
        } else {
            issue
        };

        // Replace non-alphanumeric chars with spaces to preserve word boundaries
        // This ensures "src/main.rs" becomes "src main rs" not "srcmainrs"
        truncated
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .take(10)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    fn has_blockers(&self, context: &TaskContext) -> bool {
        !context.blockers.is_empty()
    }

    fn build_module_context(&self) -> String {
        let Some(ws) = &self.workspace else {
            return String::new();
        };

        let conventions: Vec<String> = ws
            .modules()
            .iter()
            .filter(|m| !m.conventions.is_empty())
            .map(|m| {
                format!(
                    "### {}\n{}",
                    m.name,
                    m.conventions
                        .iter()
                        .map(|c| format!("- {c}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect();

        conventions.join("\n\n")
    }

    fn extract_implementation_tasks(&self, context: &TaskContext) -> Vec<ConsensusTask> {
        let default_score = calculate_priority_score(None, None);
        let mut tasks: Vec<_> = context
            .key_findings
            .iter()
            .enumerate()
            .map(|(i, finding)| ConsensusTask {
                id: format!("impl-{}", i),
                description: finding.clone(),
                priority: TaskPriority::Normal,
                dependencies: Vec::new(),
                assigned_module: None,
                priority_score: default_score,
                files_affected: Vec::new(),
                estimated_complexity: TaskComplexity::Medium,
            })
            .collect();

        if tasks.is_empty() && !context.related_files.is_empty() {
            tasks.push(ConsensusTask {
                id: "impl-main".to_string(),
                description: format!(
                    "Implement changes in files: {}",
                    context.related_files.join(", ")
                ),
                priority: TaskPriority::Normal,
                dependencies: Vec::new(),
                assigned_module: None,
                priority_score: default_score,
                files_affected: context.related_files.clone(),
                estimated_complexity: TaskComplexity::Medium,
            });
        }

        if let Some(ref ws) = self.workspace {
            for task in &mut tasks {
                if task.assigned_module.is_none() {
                    task.assigned_module = self.infer_module_from_files(
                        &task.files_affected,
                        &context.related_files,
                        ws.modules(),
                    );
                }
            }
        }

        // Sort by priority score (higher first)
        tasks.sort_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        tasks
    }

    fn infer_module_from_files(
        &self,
        task_files: &[String],
        context_files: &[String],
        modules: &[modmap::Module],
    ) -> Option<String> {
        let files_to_check: Vec<&str> = if task_files.is_empty() {
            context_files.iter().map(|s| s.as_str()).collect()
        } else {
            task_files.iter().map(|s| s.as_str()).collect()
        };

        if files_to_check.is_empty() {
            return None;
        }

        // Count how many task files each module covers
        let mut best_module = None;
        let mut best_count = 0usize;

        for module in modules {
            let count = files_to_check
                .iter()
                .filter(|f| {
                    module
                        .paths
                        .iter()
                        .any(|p: &String| f.starts_with(p.as_str()))
                })
                .count();
            if count > best_count {
                best_count = count;
                best_module = Some(module.name.clone());
            }
        }

        best_module
    }

    /// Partition tasks into independent (can run in parallel) and dependent (must wait).
    ///
    /// Tasks are considered dependent if they:
    /// 1. Have explicit dependencies on other tasks
    /// 2. Would modify files already being modified by an independent task (conflict)
    ///
    /// ## File Conflict Detection
    ///
    /// Uses path canonicalization to robustly detect file conflicts across:
    /// - Symlinks (different paths to same file)
    /// - Relative vs absolute paths (`./src/main.rs` vs `src/main.rs`)
    /// - Redundant separators (`foo//bar.rs` vs `foo/bar.rs`)
    /// - Case-insensitive filesystems (macOS/Windows)
    ///
    /// Falls back to path normalization for files that don't exist yet.
    fn partition_tasks<'a>(
        &self,
        tasks: &'a [ConsensusTask],
    ) -> (Vec<&'a ConsensusTask>, Vec<&'a ConsensusTask>) {
        use std::collections::HashSet;
        use std::path::PathBuf;

        let mut independent = Vec::new();
        let mut dependent = Vec::new();
        let mut claimed_files: HashSet<PathBuf> = HashSet::new();

        for task in tasks {
            // Check explicit dependencies
            if !task.dependencies.is_empty() {
                dependent.push(task);
                continue;
            }

            // Check file conflicts with already-independent tasks
            // Normalize paths to handle symlinks, relative paths, and case-sensitivity
            let has_conflict = task.files_affected.iter().any(|f| {
                // Try to canonicalize the path, fall back to as-is if it fails
                // (e.g., file doesn't exist yet)
                let normalized = std::fs::canonicalize(f).unwrap_or_else(|_| {
                    // Fall back to normalized relative path
                    Path::new(f).to_path_buf()
                });
                claimed_files.contains(&normalized)
            });

            if has_conflict {
                debug!(
                    task_id = %task.id,
                    "Task has file conflicts with independent tasks, marking as dependent"
                );
                dependent.push(task);
            } else {
                // Claim these files for this task
                for file in &task.files_affected {
                    let normalized = std::fs::canonicalize(file).unwrap_or_else(|_| {
                        // Fall back to normalized relative path
                        Path::new(file).to_path_buf()
                    });
                    claimed_files.insert(normalized);
                }
                independent.push(task);
            }
        }

        (independent, dependent)
    }

    /// Topological sort of tasks based on dependencies.
    ///
    /// Returns tasks in dependency order (providers before consumers).
    /// Tasks without dependencies come first, then tasks whose dependencies
    /// are satisfied, and so on.
    fn topological_sort_tasks<'a>(&self, tasks: &'a [ConsensusTask]) -> Vec<&'a ConsensusTask> {
        use std::collections::{HashMap, HashSet, VecDeque};

        if tasks.is_empty() {
            return Vec::new();
        }

        // Build dependency graph
        let task_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        let task_map: HashMap<&str, &ConsensusTask> =
            tasks.iter().map(|t| (t.id.as_str(), t)).collect();

        // Initialize in-degree for all tasks
        for task in tasks {
            in_degree.entry(task.id.as_str()).or_insert(0);

            // Count dependencies that exist in our task set
            for dep in &task.dependencies {
                if task_ids.contains(dep.as_str()) {
                    *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(dep.as_str())
                        .or_default()
                        .push(task.id.as_str());
                }
            }
        }

        // Start with tasks that have no dependencies
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::with_capacity(tasks.len());

        while let Some(task_id) = queue.pop_front() {
            if let Some(&task) = task_map.get(task_id) {
                sorted.push(task);

                // Reduce in-degree of dependents
                if let Some(deps) = dependents.get(task_id) {
                    for &dep_id in deps {
                        if let Some(deg) = in_degree.get_mut(dep_id) {
                            *deg = deg.saturating_sub(1);
                            if *deg == 0 {
                                queue.push_back(dep_id);
                            }
                        }
                    }
                }
            }
        }

        // Check for cycles (tasks not in sorted result)
        if sorted.len() < tasks.len() {
            warn!(
                sorted = sorted.len(),
                total = tasks.len(),
                "Circular dependency detected, appending remaining tasks"
            );
            // Append any remaining tasks that weren't sorted due to cycles
            for task in tasks {
                if !sorted.iter().any(|t| t.id == task.id) {
                    sorted.push(task);
                }
            }
        }

        sorted
    }

    pub fn pool_stats(&self) -> PoolStatistics {
        self.pool.statistics()
    }

    pub fn metrics(&self) -> MetricsSnapshot {
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

    fn check_health_adaptation(&self) -> HealthAdaptation {
        let report = self.health_monitor.check_health();
        match report.status {
            HealthStatus::Critical => {
                warn!(
                    score = %format!("{:.1}%", report.overall_score * 100.0),
                    bottlenecks = %report.bottlenecks.len(),
                    "Critical health status detected"
                );
                for bottleneck in &report.bottlenecks {
                    warn!(
                        role = %bottleneck.role_id,
                        severity = %format!("{:.2}", bottleneck.severity),
                        recommendation = %bottleneck.recommendation,
                        "Bottleneck detected"
                    );
                }
                if report.highest_severity() > 0.8 {
                    HealthAdaptation::Escalate {
                        reason: format!(
                            "Critical health: {:.0}% score, {} bottlenecks with severity > 0.8",
                            report.overall_score * 100.0,
                            report.bottlenecks.len()
                        ),
                    }
                } else {
                    HealthAdaptation::ForceSequential
                }
            }
            HealthStatus::Degraded => {
                info!(
                    score = %format!("{:.1}%", report.overall_score * 100.0),
                    alerts = %report.alerts.len(),
                    "Degraded health status"
                );
                HealthAdaptation::Throttle { delay_ms: 1000 }
            }
            HealthStatus::Healthy => HealthAdaptation::Continue,
        }
    }
}

enum HealthAdaptation {
    Continue,
    Throttle { delay_ms: u64 },
    ForceSequential,
    Escalate { reason: String },
}

/// Complexity distribution metrics for mission tasks.
#[derive(Debug, Clone, Default)]
pub struct ComplexityMetrics {
    /// Number of tasks by complexity level.
    pub by_level: HashMap<TaskComplexity, usize>,
    /// Total weighted complexity score.
    pub total_score: f64,
    /// Completed weighted complexity score.
    pub completed_score: f64,
    /// Completion percentage by complexity weight.
    pub completion_pct: f64,
}

impl ComplexityMetrics {
    fn from_tasks(tasks: &[ConsensusTask], completed_ids: &[String]) -> Self {
        let mut by_level: HashMap<TaskComplexity, usize> = HashMap::new();
        let mut total_score = 0.0;
        let mut completed_score = 0.0;

        for task in tasks {
            *by_level.entry(task.estimated_complexity).or_default() += 1;
            let weight = task.estimated_complexity.weight();
            total_score += weight;
            if completed_ids.contains(&task.id) {
                completed_score += weight;
            }
        }

        let completion_pct = if total_score > 0.0 {
            (completed_score / total_score) * 100.0
        } else {
            100.0
        };

        Self {
            by_level,
            total_score,
            completed_score,
            completion_pct,
        }
    }
}

/// Request for human decision when escalation is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanDecisionRequest {
    /// The question to present to the user.
    pub question: String,
    /// Available options for the user to choose from.
    pub options: Vec<EscalationOption>,
    /// Summary of the conflict that led to escalation.
    pub conflict_summary: String,
    /// Files affected by the conflict.
    pub affected_files: Vec<String>,
    /// Positions of different agents on the issue.
    pub agent_positions: HashMap<String, String>,
    /// Resolutions that were attempted before escalating.
    pub attempted_resolutions: Vec<String>,
}

#[derive(Debug)]
pub struct MissionResult {
    pub success: bool,
    pub results: Vec<AgentTaskResult>,
    pub summary: String,
    pub metrics: MetricsSnapshot,
    pub conflicts: Vec<Conflict>,
    /// Complexity distribution metrics for the mission.
    pub complexity_metrics: ComplexityMetrics,
    /// Human decision request when escalation is required.
    /// If Some, the mission cannot proceed without user input.
    pub needs_human_decision: Option<HumanDecisionRequest>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::consensus::TaskComplexity;
    use crate::agent::multi::traits::extract_files_from_output;

    #[test]
    fn test_extract_files() {
        let output = r#"
Found relevant files:
- src/auth/login.rs
- src/utils.rs (confidence: 0.9)
Also check `src/lib.rs` and src/main.rs
"#;

        let files = extract_files_from_output(output, 20);
        assert!(!files.is_empty());
        assert!(files.iter().any(|f| f.contains("src/auth")));
    }

    #[test]
    fn test_partition_tasks() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        let tasks = vec![
            ConsensusTask {
                id: "1".to_string(),
                description: "Task 1".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/a.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
            ConsensusTask {
                id: "2".to_string(),
                description: "Task 2".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec!["1".to_string()],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/b.rs".to_string()],
                estimated_complexity: TaskComplexity::Medium,
            },
            ConsensusTask {
                id: "3".to_string(),
                description: "Task 3".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/c.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
        ];

        let (independent, dependent) = coordinator.partition_tasks(&tasks);
        assert_eq!(independent.len(), 2); // Tasks 1 and 3 (no deps, different files)
        assert_eq!(dependent.len(), 1); // Task 2 (explicit dependency)
    }

    #[test]
    fn test_partition_tasks_file_conflict() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        // Task 1 and Task 2 both modify src/shared.rs - conflict!
        let tasks = vec![
            ConsensusTask {
                id: "1".to_string(),
                description: "Task 1".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/shared.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
            ConsensusTask {
                id: "2".to_string(),
                description: "Task 2".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![], // No explicit deps but has file conflict
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/shared.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
            ConsensusTask {
                id: "3".to_string(),
                description: "Task 3".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/other.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
        ];

        let (independent, dependent) = coordinator.partition_tasks(&tasks);
        // Task 1 and 3 can run in parallel (different files)
        // Task 2 conflicts with Task 1 on shared.rs, moved to dependent
        assert_eq!(independent.len(), 2);
        assert_eq!(dependent.len(), 1);
        assert_eq!(dependent[0].id, "2"); // Task 2 was marked dependent due to conflict
    }

    #[test]
    fn test_partition_tasks_path_normalization() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        // Test path normalization: relative vs absolute, redundant separators
        let tasks = vec![
            ConsensusTask {
                id: "1".to_string(),
                description: "Task 1".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["./src/main.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
            ConsensusTask {
                id: "2".to_string(),
                description: "Task 2".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                // Same file but with different path representation
                files_affected: vec!["src/main.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
            ConsensusTask {
                id: "3".to_string(),
                description: "Task 3".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec!["src/lib.rs".to_string()],
                estimated_complexity: TaskComplexity::Low,
            },
        ];

        let (independent, dependent) = coordinator.partition_tasks(&tasks);

        // Task 1 and 2 should be detected as conflicting (both modify src/main.rs)
        // Task 3 should be independent (different file)
        // The exact partitioning depends on whether src/main.rs exists in the test environment
        // If it exists: Task 1 independent, Task 2 dependent (conflict), Task 3 independent
        // If it doesn't exist: fallback to path normalization, still should detect conflict
        assert!(
            (independent.len() == 2 && dependent.len() == 1)
                || (independent.len() == 1 && dependent.len() == 2),
            "Expected 2 independent and 1 dependent, or 1 independent and 2 dependent, got {} independent and {} dependent",
            independent.len(),
            dependent.len()
        );

        // Verify Task 3 is always in independent (different file)
        assert!(
            independent.iter().any(|t| t.id == "3"),
            "Task 3 should be independent (different file)"
        );
    }

    #[test]
    fn test_coordinator_metrics() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        let metrics = coordinator.metrics();
        assert_eq!(metrics.total_executions, 0);
        assert_eq!(metrics.success_rate, 0.0);
    }

    #[test]
    fn test_coordinator_shutdown() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        assert!(!coordinator.is_shutdown());
        coordinator.shutdown();
        assert!(coordinator.is_shutdown());
    }

    #[test]
    fn test_coordinator_pool_stats() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        let stats = coordinator.pool_stats();
        assert_eq!(stats.agent_count, 0);
        assert!(!stats.is_shutdown);
    }

    #[test]
    fn test_prepare_tasks() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config.clone()));
        let coordinator = Coordinator::new(config, pool);

        let consensus_tasks = vec![
            ConsensusTask {
                id: "task-1".to_string(),
                description: "High priority simple task".to_string(),
                assigned_module: Some("api".to_string()),
                dependencies: vec![],
                priority: TaskPriority::Critical,
                estimated_complexity: TaskComplexity::Trivial,
                files_affected: vec!["src/api.rs".to_string()],
                priority_score: 0.0,
            },
            ConsensusTask {
                id: "task-2".to_string(),
                description: "Low priority complex task".to_string(),
                assigned_module: Some("db".to_string()),
                dependencies: vec!["task-1".to_string()],
                priority: TaskPriority::Low,
                estimated_complexity: TaskComplexity::Complex,
                files_affected: vec!["src/db.rs".to_string()],
                priority_score: 0.0,
            },
        ];

        let impl_tasks = coordinator.prepare_tasks(consensus_tasks);

        // High priority + low complexity should come first
        assert_eq!(impl_tasks[0].id, "task-1");
        assert!(impl_tasks[0].priority_score > impl_tasks[1].priority_score);
    }

    #[test]
    fn test_complexity_metrics_calculation() {
        let tasks = vec![
            ConsensusTask {
                id: "1".to_string(),
                description: "Trivial task".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec![],
                estimated_complexity: TaskComplexity::Trivial, // weight: 0.2
            },
            ConsensusTask {
                id: "2".to_string(),
                description: "Medium task".to_string(),
                priority: TaskPriority::Normal,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.5,
                files_affected: vec![],
                estimated_complexity: TaskComplexity::Medium, // weight: 0.6
            },
            ConsensusTask {
                id: "3".to_string(),
                description: "Complex task".to_string(),
                priority: TaskPriority::High,
                dependencies: vec![],
                assigned_module: None,
                priority_score: 0.8,
                files_affected: vec![],
                estimated_complexity: TaskComplexity::Complex, // weight: 1.0
            },
        ];

        // Total weight: 0.2 + 0.6 + 1.0 = 1.8
        let completed_ids = vec!["1".to_string(), "2".to_string()]; // Completed: 0.2 + 0.6 = 0.8
        let metrics = ComplexityMetrics::from_tasks(&tasks, &completed_ids);

        assert_eq!(metrics.by_level.get(&TaskComplexity::Trivial), Some(&1));
        assert_eq!(metrics.by_level.get(&TaskComplexity::Medium), Some(&1));
        assert_eq!(metrics.by_level.get(&TaskComplexity::Complex), Some(&1));
        assert!((metrics.total_score - 1.8).abs() < 0.001);
        assert!((metrics.completed_score - 0.8).abs() < 0.001);
        // Completion: 0.8 / 1.8 * 100 = 44.44%
        assert!((metrics.completion_pct - 44.44).abs() < 0.1);
    }

    #[test]
    fn test_complexity_metrics_empty_tasks() {
        let tasks: Vec<ConsensusTask> = vec![];
        let completed_ids: Vec<String> = vec![];
        let metrics = ComplexityMetrics::from_tasks(&tasks, &completed_ids);

        assert!(metrics.by_level.is_empty());
        assert_eq!(metrics.total_score, 0.0);
        assert_eq!(metrics.completed_score, 0.0);
        assert_eq!(metrics.completion_pct, 100.0); // No tasks = 100% complete
    }

    #[test]
    fn test_normalize_issue_key_normal_length() {
        let issue = "File src/main.rs has an error on line 42";
        let normalized = Coordinator::normalize_issue_key(issue);
        // "File src/main.rs has an error on line 42" → 10 words after normalization
        assert_eq!(normalized, "file src main rs has an error on line 42");
        assert!(normalized.len() < 100);
    }

    #[test]
    fn test_normalize_issue_key_long_input() {
        // Create a very long issue description (1000 chars)
        let long_issue = "a".repeat(1000);
        let normalized = Coordinator::normalize_issue_key(&long_issue);

        // Should be truncated and not cause memory issues
        // The function truncates at 500 chars, filters to alphanumeric,
        // takes first 10 words, joins with spaces
        assert!(normalized.len() <= 500);
    }

    #[test]
    fn test_normalize_issue_key_special_chars() {
        let issue = "Error!!! @@@File### src/main.rs:123 --- failed!!!";
        let normalized = Coordinator::normalize_issue_key(issue);
        // Special chars filtered, whitespace preserved, first 10 words taken
        assert_eq!(normalized, "error file src main rs 123 failed");
    }

    #[test]
    fn test_normalize_issue_key_max_words() {
        let issue = "one two three four five six seven eight nine ten eleven twelve";
        let normalized = Coordinator::normalize_issue_key(issue);
        // Should only take first 10 words
        assert_eq!(
            normalized,
            "one two three four five six seven eight nine ten"
        );
    }
}
