use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};

use super::focus::FocusTracker;
use super::lifecycle::LifecycleManager;
use super::signal::{Signal, SignalHandler};
use crate::agent::multi::Coordinator;
use crate::agent::{DirectAgent, TaskAgent};
use crate::config::{PilotConfig, ProjectPaths};
use crate::context::{ContextEstimator, ContextManager, LoadingRecommendation, MissionContext};
use crate::error::{PilotError, Result};
use crate::git::GitRunner;
use crate::isolation::{IsolationManager, IsolationPlanner, ScopeAnalysis};
use crate::learning::{LearningExtractor, RetryHistory};
use crate::mission::{
    IsolationMode, Learning, LearningCategory, Mission, MissionFlags, MissionStore, OnComplete,
    Priority, Task, TaskResult, TaskStatus,
};
use crate::notification::{EventType, MissionEvent, Notifier};
use crate::planning::{ChunkedPlanner, ComplexityEstimator, ComplexityTier, PlanningOrchestrator};
use crate::quality::{CoherenceChecker, CoherenceTaskResult, EnhancedEvidenceGatherer, FileChange};
use crate::recovery::{FailureHistory, RecoveryExecutor, RetryAnalyzer, RetryDecision};
use crate::state::{
    DomainEvent, EventReplayer, EventStore, MissionState, ProgressProjection,
    ProjectionSnapshotStore, StateTransition,
};
use crate::verification::{ConvergentVerifier, FileTracker, FixStrategy, Verifier};

/// Outcome of handling a task failure.
enum TaskFailureOutcome {
    /// Recovery succeeded, task will retry.
    Recovered,
    /// Task was skipped, continue with other tasks.
    Skipped,
    /// Recovery failed, task remains failed.
    Failed,
    /// Human escalation required - mission should suspend.
    Escalated {
        reason: String,
        analysis: Option<crate::recovery::FailureAnalysis>,
    },
}

pub struct Orchestrator {
    config: PilotConfig,
    paths: ProjectPaths,
    store: MissionStore,
    isolation: IsolationManager,
    verifier: Verifier,
    convergent_verifier: Arc<ConvergentVerifier>,
    agent: Arc<TaskAgent>,
    direct_agent: DirectAgent,
    evidence_gatherer: EnhancedEvidenceGatherer,
    notifier: Notifier,
    context_manager: ContextManager,
    recovery_executor: RecoveryExecutor,
    retry_analyzer: RetryAnalyzer,
    coherence_checker: CoherenceChecker,
    failure_histories: Arc<RwLock<std::collections::HashMap<String, FailureHistory>>>,
    lifecycle: LifecycleManager,
    signals: Arc<RwLock<std::collections::HashMap<String, SignalHandler>>>,
    focus_tracker: Arc<RwLock<FocusTracker>>,
    /// Multi-agent coordinator (optional, enabled via config).
    coordinator: Option<Coordinator>,
    /// Event store for event sourcing (optional, enabled via config).
    event_store: Option<Arc<EventStore>>,
    /// Event replayer for event-based state recovery.
    event_replayer: Option<Arc<EventReplayer>>,
}

impl Orchestrator {
    pub async fn new(config: PilotConfig, paths: ProjectPaths) -> Result<Self> {
        Self::with_manifest(config, paths, None).await
    }

    pub async fn with_manifest(
        config: PilotConfig,
        paths: ProjectPaths,
        manifest_path: Option<std::path::PathBuf>,
    ) -> Result<Self> {
        let store = MissionStore::new(&paths.pilot_dir);
        let isolation = IsolationManager::new(&paths);
        let verifier = Verifier::new(config.verification.clone().with_auto_detection(&paths.root));

        let agent = TaskAgent::with_verification(
            config.agent.clone(),
            &config.context.task_budget,
            &config.display,
            &config.verification,
            &paths.root,
        );

        let notifier = Notifier::new(config.notification.clone(), Some(paths.logs_dir.clone()));
        let context_manager = ContextManager::new(
            &paths.missions_dir,
            config.context.clone(),
            config.display.clone(),
        );
        let recovery_executor = RecoveryExecutor::new(
            &paths.missions_dir,
            Some(notifier.clone()),
            config.recovery.compaction_levels.clone(),
            config.recovery.checkpoint.clone(),
        );
        let evidence_gatherer = EnhancedEvidenceGatherer::new(
            &paths.root,
            config.evidence_sufficiency.gathering.clone(),
            config.evidence_sufficiency.clone(),
            config.agent.clone(),
            &config.evidence.exclude_dirs,
        );

        let agent = Arc::new(agent);

        // Log if max_fix_attempts_per_issue exceeds available strategies
        // (will cycle through strategies if needed)
        let configured_attempts = config
            .recovery
            .convergent_verification
            .max_fix_attempts_per_issue;
        let available_strategies = FixStrategy::all_strategies().len();
        if configured_attempts as usize > available_strategies {
            tracing::info!(
                configured_attempts,
                available_strategies,
                "max_fix_attempts_per_issue exceeds available strategies; will cycle through strategies"
            );
        }

        // Initialize convergent verifier (shared across orchestrator and direct executor)
        let convergent_config = config.recovery.convergent_verification.clone();
        let search_config = config.search.clone();
        let search_index_dir = paths.pilot_dir.join(&config.search.index_dir);
        let convergent_verifier = Arc::new(ConvergentVerifier::new(
            convergent_config,
            search_config,
            verifier.clone(),
            agent.clone(),
            search_index_dir,
            &paths.root,
        ));

        // Initialize direct executor (shares convergent verifier)
        let direct_agent = DirectAgent::new(agent.clone(), convergent_verifier.clone());

        // Initialize retry analyzer for intelligent failure handling with learning history
        let learning_history = RetryHistory::from_config(
            &paths.pilot_dir,
            config.learning.retry_history.retention_days,
        );
        let retry_analyzer = RetryAnalyzer::new(
            agent.clone(),
            config.recovery.retry_analyzer.clone(),
            learning_history,
            config.learning.retry_history.clone(),
        );

        // Initialize lifecycle manager for PID locking and orphan detection
        let lifecycle = LifecycleManager::with_config(&paths.pilot_dir, &config.orchestrator);

        let coherence_checker =
            CoherenceChecker::new(&paths.root, config.coherence.clone(), config.agent.clone());

        // Initialize focus tracker for anti-drift scope tracking
        let focus_tracker = FocusTracker::new(config.focus.clone());

        // Initialize event store for event sourcing if enabled
        let event_store = if config.state.event_store_enabled {
            let db_path = paths.pilot_dir.join("events.db");
            match EventStore::new(&db_path) {
                Ok(store) => {
                    info!(db_path = %db_path.display(), "Event store initialized");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize event store, event sourcing disabled");
                    None
                }
            }
        } else {
            None
        };

        // Initialize event replayer for recovery (requires event store)
        let event_replayer = event_store.as_ref().and_then(|store| {
            let snapshot_db = paths.pilot_dir.join("snapshots.db");
            match ProjectionSnapshotStore::new(&snapshot_db) {
                Ok(snapshot_store) => {
                    // EventStore is Clone - creates new Arc reference to same inner
                    let replayer = EventReplayer::new((**store).clone(), snapshot_store);
                    info!("Event replayer initialized for recovery");
                    Some(Arc::new(replayer))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize snapshot store, event replay disabled");
                    None
                }
            }
        });

        // Initialize multi-agent coordinator if enabled
        let coordinator = if config.multi_agent.enabled && config.multi_agent.coordinator_enabled {
            match crate::agent::multi::create_coordinator(
                config.multi_agent.clone(),
                agent.clone(),
                &paths.root,
                event_store.clone(),
                manifest_path.as_deref(),
            )
            .await
            {
                Ok(coord) => {
                    info!("Multi-agent coordinator initialized");
                    Some(coord)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to initialize multi-agent coordinator, falling back to single-agent mode");
                    None
                }
            }
        } else {
            None
        };

        let orchestrator = Self {
            config,
            paths,
            store,
            isolation,
            verifier,
            convergent_verifier,
            agent,
            direct_agent,
            evidence_gatherer,
            notifier,
            context_manager,
            recovery_executor,
            retry_analyzer,
            coherence_checker,
            failure_histories: Arc::new(RwLock::new(std::collections::HashMap::new())),
            lifecycle,
            signals: Arc::new(RwLock::new(std::collections::HashMap::new())),
            focus_tracker: Arc::new(RwLock::new(focus_tracker)),
            coordinator,
            event_store,
            event_replayer,
        };

        // Initialize and cleanup orphaned resources from crashed sessions
        orchestrator.init().await?;

        Ok(orchestrator)
    }

    /// Initialize the orchestrator and cleanup any orphaned resources.
    async fn init(&self) -> Result<()> {
        // Get active mission IDs to preserve their worktrees
        let active_ids: Vec<String> = self
            .store
            .get_active()
            .await?
            .iter()
            .filter(|m| m.worktree_path.is_some())
            .map(|m| m.id.clone())
            .collect();

        // Cleanup orphaned worktrees from crashed sessions
        self.isolation.cleanup_orphaned(&active_ids).await?;

        Ok(())
    }

    pub async fn create_mission(
        &self,
        description: &str,
        flags: MissionFlags,
        priority: Option<Priority>,
        on_complete: Option<OnComplete>,
    ) -> Result<Mission> {
        let id = self.store.next_id().await?;

        let mut mission = Mission::new(&id, description);
        mission.flags = flags;
        mission.max_iterations = self.config.orchestrator.max_iterations;
        if let Some(p) = priority {
            mission.priority = p;
        }
        if let Some(oc) = on_complete {
            mission.on_complete = oc;
        }

        let scope = ScopeAnalysis::analyze(description);
        let active = self.store.get_active().await?;
        let active_refs: Vec<_> = active.iter().collect();
        let planner = IsolationPlanner::new(&self.config, active_refs);

        let isolation = planner.determine_isolation(&mission, scope.estimated_files);
        mission.isolation = isolation;

        self.store.save(&mission).await?;

        {
            let mut signals = self.signals.write().await;
            signals.insert(id.clone(), SignalHandler::new());
        } // Drop write lock before async operations

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionCreated, &mission.id))
            .await;

        info!(
            mission_id = %mission.id,
            isolation = %isolation,
            "Mission created"
        );

        // Emit mission created event
        self.emit_event(DomainEvent::mission_created(&mission.id, description))
            .await;

        Ok(mission)
    }

    pub async fn execute(&self, mission_id: &str) -> Result<()> {
        // Acquire execution lock - prevents concurrent execution and enables orphan detection
        let _lock_guard = self.lifecycle.acquire_lock(mission_id).await?;

        let mut mission = self.store.load(mission_id).await?;

        if mission.status.is_terminal() {
            return Err(PilotError::InvalidMissionState {
                expected: "non-terminal".into(),
                actual: mission.status.to_string(),
            });
        }

        // Restore from checkpoint if available (durable execution for long-running missions)
        let restored = {
            let mut context = self.context_manager.load_or_init(&mission).await?;
            if let Some((checkpoint_id, failure_histories)) = self
                .recovery_executor
                .try_restore_on_startup(&mut mission, &mut context)
                .await?
            {
                info!(checkpoint_id = %checkpoint_id, "Mission state restored from checkpoint");
                self.store.save(&mission).await?;
                self.context_manager.save(&context).await?;
                // Restore failure histories for retry pattern learning
                if let Some(histories) = failure_histories {
                    let mut fh = self.failure_histories.write().await;
                    fh.extend(histories);
                }
                true
            } else {
                false
            }
        };

        if restored {
            debug!(mission_id = %mission.id, "Continuing from restored checkpoint state");
        }

        // Enhanced recovery: reconcile mission state from event history
        if let Some(replayer) = &self.event_replayer {
            match replayer
                .rebuild_projection::<ProgressProjection>(&mission.id)
                .await
            {
                Ok(projection) if projection.last_applied_seq > 0 => {
                    // Reconcile task completion counts if projection has more accurate data
                    let mission_completed = mission
                        .tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Completed)
                        .count();
                    if projection.completed_tasks > mission_completed {
                        info!(
                            mission_id = %mission.id,
                            events_replayed = projection.last_applied_seq,
                            projection_completed = projection.completed_tasks,
                            mission_completed = mission_completed,
                            "Event history shows more completed tasks than mission state"
                        );
                    }

                    // Log recovery context for debugging
                    debug!(
                        mission_id = %mission.id,
                        projection_state = ?projection.state,
                        total_tasks = projection.total_tasks,
                        completed_tasks = projection.completed_tasks,
                        issues_detected = projection.total_issues_detected,
                        issues_resolved = projection.total_issues_resolved,
                        "Mission progress recovered from event history"
                    );
                }
                Ok(_) => {
                    // No events to replay - fresh mission
                }
                Err(e) => {
                    debug!(error = %e, "Event replay not available (non-fatal)");
                }
            }
        }

        // Handle escalation state - enforce human intervention requirement
        if mission.status == MissionState::Escalated {
            // Mission is in Escalated state - must go through resume()
            // This applies even if escalation context is missing (data loss/legacy)
            return Err(PilotError::InvalidMissionState {
                expected: "Use 'resume' command for escalated missions".into(),
                actual: "Escalated (pending human intervention)".into(),
            });
        }

        // Clear residual escalation context if mission was properly resumed
        if mission.escalation.is_some() {
            debug!(mission_id = %mission.id, "Clearing resolved escalation context");
            mission.escalation = None;
            self.store.save(&mission).await?;
        }

        self.recover_in_progress_tasks(&mut mission).await?;
        self.setup_isolation(&mut mission).await?;

        // Check context budget before proceeding (prevents API overflow errors)
        self.check_context_budget(&mission).await?;

        // 3-tier complexity routing:
        // - Trivial: Skip evidence gathering entirely, execute directly
        // - Simple: Gather evidence, execute directly without full planning
        // - Complex: Full planning with spec/plan/tasks
        if mission.tasks.is_empty() {
            // Fast path: --direct flag bypasses all complexity evaluation and evidence gathering
            if mission.flags.direct {
                info!(
                    mission_id = %mission.id,
                    "Direct mode enabled, skipping evidence gathering and planning"
                );
                return self.execute_direct(&mut mission).await;
            }

            // Initial fast assessment without evidence
            let initial_tier = self.evaluate_complexity_tier(&mission, None).await;

            match initial_tier {
                ComplexityTier::Trivial => {
                    // Trivial tasks skip evidence gathering entirely
                    info!(
                        mission_id = %mission.id,
                        "Trivial task detected, skipping evidence gathering"
                    );
                    mission.estimated_complexity = Some(initial_tier);
                    return self.execute_direct(&mut mission).await;
                }
                ComplexityTier::Simple | ComplexityTier::Complex => {
                    // Gather evidence for more accurate assessment and planning reuse
                    let evidence = self
                        .evidence_gatherer
                        .gather(&mission.description, None)
                        .await?;

                    // Re-evaluate with evidence for better accuracy
                    let final_tier = self
                        .evaluate_complexity_tier(&mission, Some(&evidence))
                        .await;

                    match final_tier {
                        ComplexityTier::Trivial | ComplexityTier::Simple => {
                            // Simple (or downgraded to trivial) - execute directly
                            debug!(
                                mission_id = %mission.id,
                                initial_tier = ?initial_tier,
                                final_tier = ?final_tier,
                                "Using direct execution after evidence assessment"
                            );
                            mission.estimated_complexity = Some(final_tier);
                            return self.execute_direct(&mut mission).await;
                        }
                        ComplexityTier::Complex => {
                            mission.estimated_complexity = Some(final_tier);

                            // Use multi-agent coordinator if available
                            if self.coordinator.is_some() {
                                info!(
                                    mission_id = %mission.id,
                                    "Using multi-agent coordinator for complex task"
                                );
                                return self.execute_with_coordinator(&mut mission).await;
                            }

                            // Fall back to single-agent planning + execution
                            self.transition_state(
                                &mut mission,
                                MissionState::Planning,
                                "Complex task requires full planning",
                            )
                            .await?;

                            if let Err(e) = self
                                .plan_mission_with_evidence(&mut mission, evidence)
                                .await
                            {
                                self.cleanup_on_failure(&mut mission).await;
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }

        // Only transition to Running if not already in Running state
        // (resume() may have already transitioned from Paused/Escalated to Running)
        if mission.status != MissionState::Running {
            self.transition_state(&mut mission, MissionState::Running, "Starting execution")
                .await?;
        }
        if mission.started_at.is_none() {
            mission.started_at = Some(Utc::now());
        }
        self.store.save(&mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, mission_id))
            .await;

        if let Err(e) = self.execute_loop(&mut mission).await {
            // Only cleanup if NOT in Escalated state.
            // Escalated state means we intentionally suspended for human intervention
            // and must preserve worktree and state for durable resume.
            if !mission.status.is_suspended() {
                self.cleanup_on_failure(&mut mission).await;
            }
            return Err(e);
        }

        // Lock is automatically released when _lock_guard goes out of scope
        Ok(())
    }

    /// Determines the complexity tier for mission routing.
    /// Returns Trivial, Simple, or Complex tier based on LLM assessment.
    /// Uses Planning model for higher quality strategic decisions.
    ///
    /// Note: --direct flag is handled before this method is called,
    /// bypassing complexity evaluation entirely.
    async fn evaluate_complexity_tier(
        &self,
        mission: &Mission,
        evidence: Option<&crate::planning::Evidence>,
    ) -> ComplexityTier {
        // Force full planning if explicitly using isolated mode
        if mission.flags.isolated {
            return ComplexityTier::Complex;
        }

        // Use PlanAgent (Planning model) for complexity assessment
        // Complexity evaluation is a strategic decision that benefits from higher quality reasoning
        let plan_agent = match crate::planning::PlanAgent::new(&self.paths.root, &self.config.agent)
        {
            Ok(agent) => agent,
            Err(e) => {
                warn!(error = %e, "Failed to create planning agent for complexity, defaulting to Complex");
                return ComplexityTier::Complex;
            }
        };

        let mut estimator = ComplexityEstimator::new(
            &self.paths.root,
            &self.config.complexity,
            plan_agent,
            self.config.task_scoring.clone(),
        );
        estimator.load_examples().await;
        let gate = estimator
            .evaluate_with_evidence(&mission.description, evidence)
            .await;

        debug!(
            mission_id = %mission.id,
            tier = ?gate.tier,
            reasoning = %gate.reasoning,
            evidence_based = evidence.is_some(),
            "Complexity tier evaluated"
        );

        gate.tier
    }

    /// Pre-flight check for context budget.
    /// Prevents API errors by detecting overflow before execution.
    async fn check_context_budget(&self, mission: &Mission) -> Result<()> {
        use crate::config::AuthMode;

        let working_dir = self.isolation.working_dir(mission);
        let model_config = self.config.agent.model_config()?;
        let max_context = self.config.context.effective_max_tokens(&model_config);

        // Warn if extended_context is configured but ignored due to OAuth mode
        if self.config.agent.extended_context && self.config.agent.auth_mode == AuthMode::Oauth {
            warn!(
                configured_extended_context = self.config.agent.extended_context,
                auth_mode = ?self.config.agent.auth_mode,
                effective_context_window = max_context,
                "extended_context setting ignored in OAuth mode - using 200k limit"
            );
        }

        let estimator =
            ContextEstimator::with_config(max_context, self.config.context.estimator.clone());
        let estimate = estimator.estimate(&working_dir).await;

        debug!(
            auth_mode = ?self.config.agent.auth_mode,
            extended_context_effective = self.config.agent.effective_extended_context(),
            max_context,
            "Context budget check"
        );

        match estimate.recommendation {
            LoadingRecommendation::Emergency => {
                error!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    max = estimate.max_context,
                    "Context overflow detected - cannot proceed"
                );
                return Err(PilotError::ContextOverflow {
                    estimated: estimate.total_estimated,
                    max: estimate.max_context,
                });
            }
            LoadingRecommendation::SkipMcpResources => {
                warn!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget tight - MCP resources will be skipped"
                );
            }
            LoadingRecommendation::LoadSelective => {
                info!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget moderate - using selective resource loading"
                );
            }
            LoadingRecommendation::LoadAll => {
                debug!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget sufficient"
                );
            }
        }

        Ok(())
    }

    /// Executes a mission directly without full planning phase.
    /// Includes proper post-processing: commit, on_complete actions, cleanup, and learning extraction.
    async fn execute_direct(&self, mission: &mut Mission) -> Result<()> {
        info!(mission_id = %mission.id, "Using direct execution mode");

        self.transition_state(mission, MissionState::Running, "Starting direct execution")
            .await?;
        mission.started_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, &mission.id))
            .await;

        let working_dir = self.isolation.working_dir(mission);

        let result = self
            .direct_agent
            .execute(&mission.description, &working_dir)
            .await?;

        if result.success {
            info!(
                mission_id = %mission.id,
                convergence_rounds = result.convergence_rounds,
                "Direct execution completed successfully"
            );

            // Transition through Verifying before Completed (state machine requirement)
            self.transition_state(mission, MissionState::Verifying, "Verification passed")
                .await?;

            let commit_message = format!("feat({}): {}", mission.id, mission.description);
            self.isolation.commit(mission, &commit_message).await?;

            self.complete_mission(mission).await?;
        } else {
            // cleanup_on_failure handles state transition and notification
            self.cleanup_on_failure(mission).await;
        }

        Ok(())
    }

    /// Execute mission using multi-agent coordinator.
    ///
    /// This method provides an alternative execution path when multi-agent mode is enabled.
    /// The coordinator orchestrates specialized agents (Research, Planning, Coder, Verifier)
    /// through a 4-phase workflow:
    /// 1. Research: Evidence gathering and codebase analysis
    /// 2. Planning: Implementation plan generation
    /// 3. Implementation: Parallel/sequential task execution
    /// 4. Verification: Quality checks and convergent verification
    async fn execute_with_coordinator(&self, mission: &mut Mission) -> Result<()> {
        let coordinator = self.coordinator.as_ref().ok_or_else(|| {
            PilotError::AgentExecution("Multi-agent coordinator not initialized".into())
        })?;

        info!(mission_id = %mission.id, "Using multi-agent coordinator execution");

        self.transition_state(
            mission,
            MissionState::Running,
            "Starting multi-agent execution",
        )
        .await?;
        mission.started_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, &mission.id))
            .await;

        let working_dir = self.isolation.working_dir(mission);

        let result = coordinator
            .execute_mission(&mission.id, &mission.description, &working_dir)
            .await?;

        let success_count = result.results.iter().filter(|r| r.success).count();
        self.emit_event(DomainEvent::multi_agent_phase_completed(
            &mission.id,
            "execution",
            result.results.len(),
            success_count,
            0,
        ))
        .await;

        // Log execution metrics
        let metrics = result.metrics;
        info!(
            mission_id = %mission.id,
            total_executions = metrics.total_executions,
            success_rate = metrics.success_rate,
            avg_duration_ms = metrics.avg_duration_ms,
            "Multi-agent execution completed"
        );

        if result.success {
            info!(
                mission_id = %mission.id,
                summary = %result.summary,
                phase_count = result.results.len(),
                "Multi-agent mission completed successfully"
            );

            // Transition through Verifying before Completed (state machine requirement)
            self.transition_state(
                mission,
                MissionState::Verifying,
                "Multi-agent verification passed",
            )
            .await?;

            let commit_message = format!("feat({}): {}", mission.id, mission.description);
            self.isolation.commit(mission, &commit_message).await?;

            self.complete_mission(mission).await?;
        } else {
            warn!(
                mission_id = %mission.id,
                summary = %result.summary,
                "Multi-agent mission failed"
            );
            self.cleanup_on_failure(mission).await;
        }

        Ok(())
    }

    async fn cleanup_on_failure(&self, mission: &mut Mission) {
        // Cleanup worktree if applicable
        if mission.isolation == IsolationMode::Worktree
            && let Err(e) = self.isolation.cleanup(mission, false).await
        {
            warn!(mission_id = %mission.id, error = %e, "Failed to cleanup worktree");
        }

        // Only transition if not already in a terminal state
        if !mission.status.is_terminal() {
            if let Err(e) = self
                .transition_state(mission, MissionState::Failed, "Cleanup on failure")
                .await
            {
                error!(mission_id = %mission.id, error = %e, "Failed to transition to Failed state");
                mission.status = MissionState::Failed;
                if let Err(e) = self.store.save(mission).await {
                    error!(mission_id = %mission.id, error = %e, "Failed to save mission state");
                }
            }

            self.notifier
                .notify(&MissionEvent::new(EventType::MissionFailed, &mission.id))
                .await;
        }
    }

    async fn recover_in_progress_tasks(&self, mission: &mut Mission) -> Result<()> {
        let mission_id = mission.id.clone();
        let in_progress_ids: Vec<String> = mission
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InProgress || t.status == TaskStatus::Verifying)
            .map(|t| t.id.clone())
            .collect();

        for task_id in in_progress_ids {
            warn!(
                mission_id = %mission_id,
                task_id = %task_id,
                "Recovering interrupted task"
            );
            if let Some(task) = mission.task_mut(&task_id) {
                task.status = TaskStatus::Pending;
            }
        }

        if !mission.tasks.is_empty() {
            self.store.save(mission).await?;
        }

        Ok(())
    }

    async fn setup_isolation(&self, mission: &mut Mission) -> Result<()> {
        if mission.isolation == IsolationMode::Auto {
            let scope = ScopeAnalysis::analyze(&mission.description);
            let active = self.store.get_active().await?;
            let active_refs: Vec<_> = active.iter().collect();
            let planner = IsolationPlanner::new(&self.config, active_refs);
            mission.isolation = planner.determine_isolation(mission, scope.estimated_files);
        }

        self.isolation.setup(mission).await?;
        self.store.save(mission).await?;

        info!(
            mission_id = %mission.id,
            isolation = %mission.isolation,
            branch = ?mission.branch,
            worktree = ?mission.worktree_path,
            "Isolation configured"
        );

        Ok(())
    }

    /// Plan mission using pre-gathered evidence.
    /// This allows evidence reuse from complexity assessment, avoiding duplicate collection.
    async fn plan_mission_with_evidence(
        &self,
        mission: &mut Mission,
        evidence: crate::planning::Evidence,
    ) -> Result<()> {
        // Tier-based quality assessment
        let assessment = evidence.get_quality_tier(&self.config.quality);
        match assessment.tier {
            crate::planning::QualityTier::Red => {
                warn!(
                    mission_id = %mission.id,
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "RED",
                    "Evidence quality insufficient"
                );
                return Err(PilotError::EvidenceGathering(format!(
                    "Evidence RED tier: {} - cannot proceed safely",
                    assessment.format_for_llm()
                )));
            }
            crate::planning::QualityTier::Yellow => {
                warn!(
                    mission_id = %mission.id,
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "YELLOW",
                    "Proceeding with caution"
                );
            }
            crate::planning::QualityTier::Green => {}
        }

        let evidence_tokens = evidence.estimated_tokens(&self.config.quality);
        let chunking_threshold = self.config.evidence.plan_evidence_budget;

        // Chunking decision based on evidence size (deterministic, no LLM needed)
        let needs_chunking =
            self.config.chunked_planning.enabled && evidence_tokens > chunking_threshold;

        if needs_chunking {
            info!(
                mission_id = %mission.id,
                evidence_tokens = evidence_tokens,
                threshold = chunking_threshold,
                "Evidence exceeds threshold, using chunked planning"
            );
        }

        // CRITICAL: Use the mission's working directory (worktree path for isolated execution)
        // This ensures the planner initializes from the correct directory
        let working_dir = self.isolation.working_dir(mission);

        // Create planner with correct working_dir for isolated execution
        let planner = PlanningOrchestrator::new(
            &working_dir,
            self.config.verification.ai_plan_validation,
            &self.config.agent,
            self.config.quality.clone(),
            self.config.evidence.clone(),
            self.config.task_decomposition.clone(),
        )?;

        let result = if needs_chunking {
            let chunked = ChunkedPlanner::new(
                &working_dir,
                self.config.agent.clone(),
                self.config.quality.clone(),
                self.config.evidence.clone(),
                self.config.task_decomposition.clone(),
                self.config.chunked_planning.clone(),
            )?;
            chunked.plan(&mission.description, evidence).await?
        } else {
            planner.plan(&mission.description, evidence).await?
        };

        if !result.validation.passed {
            return Err(PilotError::PlanValidation(result.validation.to_summary()));
        }

        planner.apply_to_mission(mission, &result);
        planner.save_artifacts(&mission.id, &result).await?;

        let model_config = self.config.agent.model_config()?;
        let mut context = self
            .context_manager
            .load_or_init_with_model_config(mission, &model_config)
            .await?;
        self.context_manager
            .initialize_from_planning(&mut context, mission, &result.evidence);
        self.context_manager.save(&context).await?;

        if result.recommended_isolation != mission.isolation {
            info!(
                mission_id = %mission.id,
                current = %mission.isolation,
                recommended = %result.recommended_isolation,
                "Isolation mode recommendation differs"
            );
        }

        self.store.save(mission).await?;

        info!(
            mission_id = %mission.id,
            task_count = mission.tasks.len(),
            phase_count = mission.phases.len(),
            risk = %result.plan.risk_assessment.overall_risk,
            "Mission planned"
        );

        Ok(())
    }

    async fn check_signal(&self, mission: &mut Mission) -> Result<bool> {
        let signal = {
            let signals = self.signals.read().await;
            signals
                .get(&mission.id)
                .map(|h| h.check_and_acknowledge())
                .unwrap_or(Signal::None)
        };

        match signal {
            Signal::None => Ok(false),
            Signal::Pause => {
                self.transition_state(mission, MissionState::Paused, "Pause signal received")
                    .await?;
                self.notifier
                    .notify(&MissionEvent::new(EventType::MissionPaused, &mission.id))
                    .await;
                info!(mission_id = %mission.id, "Mission paused by signal");
                Err(PilotError::MissionPaused)
            }
            Signal::Cancel => {
                self.transition_state(mission, MissionState::Cancelled, "Cancel signal received")
                    .await?;
                if mission.isolation == IsolationMode::Worktree {
                    self.isolation.cleanup(mission, true).await?;
                }
                self.notifier
                    .notify(&MissionEvent::new(EventType::MissionCancelled, &mission.id))
                    .await;
                info!(mission_id = %mission.id, "Mission cancelled by signal");
                Err(PilotError::MissionCancelled)
            }
        }
    }

    async fn execute_loop(&self, mission: &mut Mission) -> Result<()> {
        let mut checkpoint_counter = 0;
        let mut coherence_counter = 0;
        let mut completed_task_results: Vec<CoherenceTaskResult> = Vec::new();
        let max_parallel = self.config.orchestrator.max_parallel_tasks;
        let semaphore = Arc::new(Semaphore::new(max_parallel));

        let checkpoint_interval_minutes = self.config.recovery.checkpoint.interval_minutes;
        let mut last_checkpoint_time = std::time::Instant::now();
        let mission_start = std::time::Instant::now();
        let mission_timeout =
            std::time::Duration::from_secs(self.config.orchestrator.mission_timeout_secs);

        let model_config = self.config.agent.model_config()?;
        let mut context = self
            .context_manager
            .load_or_init_with_model_config(mission, &model_config)
            .await?;

        while !mission.all_tasks_done() && mission.iteration < mission.max_iterations {
            self.check_signal(mission).await?;

            // Check mission-level timeout (0 = unlimited)
            if mission_timeout.as_secs() > 0 && mission_start.elapsed() > mission_timeout {
                let reason = format!(
                    "Mission timeout exceeded: {}s (can resume with 'pilot resume {}')",
                    mission_timeout.as_secs(),
                    mission.id
                );

                // Save checkpoint before timeout - allows resume later
                if let Err(cp_err) = self.checkpoint(mission).await {
                    warn!(error = %cp_err, "Failed to save checkpoint on timeout");
                }

                // Escalate instead of failing immediately
                self.transition_state(mission, MissionState::Escalated, &reason)
                    .await?;

                // Create and persist escalation context for timeout
                let mut escalation_ctx =
                    crate::recovery::EscalationContext::new(&mission.id, &reason);
                escalation_ctx.add_action(crate::recovery::HumanAction::Retry);
                mission.escalation = Some(escalation_ctx);

                self.notifier
                    .notify(
                        &MissionEvent::new(EventType::MissionPaused, &mission.id)
                            .with_message(reason.clone()),
                    )
                    .await;

                if let Err(save_err) = self.store.save(mission).await {
                    error!(error = %save_err, "Failed to save mission on timeout");
                }

                return Err(PilotError::Timeout(reason));
            }

            if self.config.context.enable_auto_compaction
                && let Some(trigger) = self.context_manager.needs_compaction(&context)
            {
                info!(?trigger, "Context compaction triggered");
                self.context_manager.compact(&mut context).await?;
            }

            mission.iteration += 1;

            let next_tasks = mission.next_tasks();
            if next_tasks.is_empty() {
                if mission.active_tasks().is_empty() {
                    if mission.all_tasks_done() {
                        break;
                    }

                    // Check for tasks blocked by permanently failed dependencies
                    // These tasks can never start, so auto-skip them to prevent stalling
                    let blocked_tasks: Vec<(String, String)> = mission
                        .tasks_blocked_by_failed_deps()
                        .into_iter()
                        .map(|(task, deps)| (task.id.clone(), deps.join(", ")))
                        .collect();

                    if !blocked_tasks.is_empty() {
                        for (task_id, failed_deps) in &blocked_tasks {
                            let reason = format!("Dependency failed permanently: {}", failed_deps);
                            warn!(
                                task_id = %task_id,
                                failed_deps = %failed_deps,
                                "Auto-skipping task due to failed dependencies"
                            );
                            if let Some(t) = mission.task_mut(task_id) {
                                t.skip(reason.clone());
                            }
                            self.context_manager
                                .skip_task(&mut context, task_id, &reason);
                        }
                        self.store.save(mission).await?;
                        self.context_manager.save(&context).await?;
                        // Continue loop to re-evaluate after skipping blocked tasks
                        continue;
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    continue;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                continue;
            }

            let working_dir = self.isolation.working_dir(mission);
            let task_snapshots: Vec<(String, Task)> = next_tasks
                .iter()
                .map(|t| (t.id.clone(), (*t).clone()))
                .collect();

            // Start focus tracking for each task (anti-drift)
            {
                let mut focus = self.focus_tracker.write().await;
                for (task_id, _) in &task_snapshots {
                    let scope =
                        focus.derive_scope(task_id, None, None, context.evidence_snapshot.as_ref());
                    focus.start_task(task_id, scope);
                }
            }

            for (task_id, task) in &task_snapshots {
                if let Some(t) = mission.task_mut(task_id) {
                    t.start();
                    self.context_manager.start_task(&mut context, task_id);
                    self.notifier
                        .notify(
                            &MissionEvent::new(EventType::TaskStarted, &mission.id)
                                .with_task(task_id),
                        )
                        .await;

                    // Emit task started event
                    self.emit_event(DomainEvent::task_started(
                        &mission.id,
                        task_id,
                        &task.description,
                    ))
                    .await;
                }
            }
            self.store.save(mission).await?;
            self.context_manager.save(&context).await?;

            // Execute tasks with file tracking
            let results = self
                .execute_tasks_with_tracking(
                    mission,
                    task_snapshots,
                    &working_dir,
                    semaphore.clone(),
                )
                .await;

            // Process results with convergent verification and focus tracking
            for (task_id, result, file_changes) in results {
                // Validate focus/scope for completed task (anti-drift)
                {
                    let mut focus = self.focus_tracker.write().await;
                    let validation = focus.validate_completion(&task_id, &file_changes);
                    if !validation.warnings.is_empty() {
                        for warning in &validation.warnings {
                            warn!(
                                task_id = %task_id,
                                warning = ?warning,
                                "Focus drift detected"
                            );
                        }
                    }
                }
                match self
                    .apply_task_result_with_convergence(
                        mission,
                        &task_id,
                        result,
                        file_changes.clone(),
                        &mut context,
                        &working_dir,
                    )
                    .await
                {
                    Ok(_) => {
                        if let Some(task) = mission.task(&task_id) {
                            let mut changes: Vec<FileChange> = file_changes
                                .created
                                .iter()
                                .map(|p| FileChange::created(p.clone()))
                                .collect();
                            changes.extend(
                                file_changes
                                    .modified
                                    .iter()
                                    .map(|p| FileChange::modified(p.clone())),
                            );
                            completed_task_results.push(
                                CoherenceTaskResult::new(&task_id, &task.description)
                                    .with_file_changes(changes),
                            );
                        }
                    }
                    Err(e) => {
                        error!(task_id = %task_id, error = %e, "Task execution failed");
                        let outcome = self
                            .handle_task_failure(mission, &task_id, e, &mut context)
                            .await;

                        match outcome {
                            TaskFailureOutcome::Recovered => {
                                // Task was reset to Pending by execute_recovery_action
                                // It will be picked up in the next iteration
                                debug!(task_id = %task_id, "Task recovered, will retry");
                            }
                            TaskFailureOutcome::Skipped => {
                                // Mark task as skipped with proper result
                                let reason = "Task skipped by recovery decision";
                                if let Some(task) = mission.task_mut(&task_id) {
                                    task.skip(reason.to_string());
                                    info!(task_id = %task_id, "Task marked as skipped");
                                }
                                // Correct context to reflect skip (removes failed blocker, sets Skipped status)
                                self.context_manager
                                    .skip_task(&mut context, &task_id, reason);
                            }
                            TaskFailureOutcome::Failed => {
                                // Task stays in Failed state (set by task.fail())
                                // If it can't retry, it will be counted as permanently failed
                                debug!(task_id = %task_id, "Task permanently failed");
                            }
                            TaskFailureOutcome::Escalated { reason, analysis } => {
                                // Save checkpoint before escalation
                                if let Err(cp_err) = self.checkpoint(mission).await {
                                    warn!(error = %cp_err, "Failed to save checkpoint before escalation");
                                }

                                // Transition to Escalated state
                                if let Err(state_err) = self
                                    .transition_state(mission, MissionState::Escalated, &reason)
                                    .await
                                {
                                    error!(error = %state_err, "Failed to transition to Escalated state");
                                }

                                // Create escalation context with full details
                                let mut escalation_ctx =
                                    crate::recovery::EscalationContext::new(&mission.id, &reason);
                                if let Some(a) = analysis {
                                    escalation_ctx = escalation_ctx.with_analysis(a);
                                }
                                escalation_ctx.add_action(crate::recovery::HumanAction::Retry);

                                // Persist escalation context to mission for durable resume
                                mission.escalation = Some(escalation_ctx.clone());

                                // Notify about escalation
                                self.notifier
                                    .notify(
                                        &MissionEvent::new(EventType::MissionPaused, &mission.id)
                                            .with_message(format!("Escalation: {}", reason)),
                                    )
                                    .await;

                                // Save mission state with escalation context
                                if let Err(save_err) = self.store.save(mission).await {
                                    error!(error = %save_err, "Failed to save mission after escalation");
                                }

                                // Return error to exit execution loop
                                return Err(PilotError::EscalationRequired { summary: reason });
                            }
                        }
                    }
                }
            }

            self.store.save(mission).await?;
            self.context_manager
                .update_from_mission(&mut context, mission);
            self.context_manager.save(&context).await?;

            // Incremental coherence check
            coherence_counter += 1;
            if self.config.coherence.incremental_enabled
                && coherence_counter >= self.config.coherence.incremental_interval as usize
                && !completed_task_results.is_empty()
            {
                self.run_incremental_coherence(mission, &completed_task_results)
                    .await;
                coherence_counter = 0;
            }

            // Task-based checkpoint
            checkpoint_counter += 1;
            let task_checkpoint_needed =
                checkpoint_counter >= self.config.recovery.checkpoint.interval_tasks as usize;

            // Time-based checkpoint
            let time_checkpoint_needed = checkpoint_interval_minutes > 0
                && last_checkpoint_time.elapsed().as_secs() >= checkpoint_interval_minutes * 60;

            if task_checkpoint_needed || time_checkpoint_needed {
                self.checkpoint(mission).await?;
                if self.config.recovery.enabled {
                    let evidence = if self.config.recovery.checkpoint.persist_evidence {
                        context.evidence_snapshot.as_ref()
                    } else {
                        None
                    };
                    let histories = self.failure_histories.read().await;
                    self.recovery_executor
                        .create_checkpoint(mission, &context, evidence, Some(&*histories))
                        .await?;
                    drop(histories);

                    // Emit checkpoint created event
                    let checkpoint_id = format!("cp-{}-{}", mission.id, mission.iteration);
                    self.emit_event(DomainEvent::checkpoint_created(
                        &mission.id,
                        &checkpoint_id,
                        mission.tasks.len(),
                    ))
                    .await;
                }
                self.cleanup_stale_failure_histories().await;
                checkpoint_counter = 0;
                last_checkpoint_time = std::time::Instant::now();
            }
        }

        if mission.iteration >= mission.max_iterations {
            self.transition_state(mission, MissionState::Failed, "Maximum iterations exceeded")
                .await?;
            return Err(PilotError::MaxIterationsExceeded(mission.id.clone()));
        }

        let failed_ids: Vec<String> = mission
            .permanently_failed_tasks()
            .iter()
            .map(|t| t.id.clone())
            .collect();

        if !failed_ids.is_empty() {
            let reason = format!(
                "{} permanently failed tasks: {}",
                failed_ids.len(),
                failed_ids.join(", ")
            );
            self.transition_state(mission, MissionState::Failed, &reason)
                .await?;
            return Err(PilotError::VerificationFailed {
                message: reason,
                checks: Vec::new(),
            });
        }

        self.finalize(mission).await?;

        Ok(())
    }

    /// Execute tasks with file tracking using FileTracker for accurate change detection.
    async fn execute_tasks_with_tracking(
        &self,
        mission: &Mission,
        tasks: Vec<(String, Task)>,
        working_dir: &std::path::Path,
        semaphore: Arc<Semaphore>,
    ) -> Vec<(String, Result<TaskResult>, crate::verification::FileChanges)> {
        use futures::future::join_all;

        let task_ids: Vec<String> = tasks.iter().map(|(id, _)| id.clone()).collect();
        let mission_arc = Arc::new(mission.clone());
        // Parallel mode only when both: multiple tasks AND semaphore permits > 1
        // If semaphore permits == 1, tasks run sequentially even with multiple tasks
        let actual_concurrency = semaphore.available_permits().min(tasks.len());
        let parallel_mode = actual_concurrency > 1;

        let handles: Vec<_> = tasks
            .into_iter()
            .map(|(task_id, task)| {
                let sem = semaphore.clone();
                let agent = Arc::clone(&self.agent);
                let mission_ref = Arc::clone(&mission_arc);
                let working_dir = working_dir.to_path_buf();

                tokio::spawn(async move {
                    let permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => {
                            return (
                                task_id,
                                Err(PilotError::AgentExecution("semaphore closed".into())),
                                crate::verification::FileChanges::default(),
                            );
                        }
                    };

                    debug!(task_id = %task_id, "Starting task execution with file tracking");

                    // Use parallel mode when multiple tasks execute concurrently
                    let mut file_tracker = FileTracker::new().with_parallel_mode(parallel_mode);
                    file_tracker.capture_before(&working_dir);

                    let result = agent.execute_task(&mission_ref, &task, &working_dir).await;

                    // Capture filesystem state after execution
                    file_tracker.capture_after(&working_dir);

                    let file_changes = file_tracker.changes();

                    drop(permit);

                    debug!(
                        task_id = %task_id,
                        success = result.is_ok(),
                        files_created = file_changes.created.len(),
                        files_modified = file_changes.modified.len(),
                        files_deleted = file_changes.deleted.len(),
                        "Task execution completed with file tracking"
                    );

                    (task_id, result, file_changes)
                })
            })
            .collect();

        let results = join_all(handles).await;

        results
            .into_iter()
            .zip(task_ids)
            .map(|(r, original_id)| match r {
                Ok(result) => result,
                Err(e) => {
                    error!(task_id = %original_id, error = %e, "Task panicked during execution");
                    (
                        original_id,
                        Err(PilotError::AgentExecution(format!("task panicked: {}", e))),
                        crate::verification::FileChanges::default(),
                    )
                }
            })
            .collect()
    }

    /// Apply task result with convergent verification for automatic error correction.
    async fn apply_task_result_with_convergence(
        &self,
        mission: &mut Mission,
        task_id: &str,
        result: Result<TaskResult>,
        file_changes: crate::verification::FileChanges,
        context: &mut MissionContext,
        working_dir: &std::path::Path,
    ) -> Result<()> {
        let mission_id = mission.id.clone();
        let task_not_found = || PilotError::TaskNotFound {
            mission_id: mission_id.clone(),
            task_id: task_id.to_string(),
        };

        match result {
            Ok(mut task_result) => {
                // Update file changes from FileTracker (more accurate than pattern matching)
                task_result.files_created = file_changes
                    .created
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                task_result.files_modified = file_changes
                    .modified
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                task_result.files_deleted = file_changes
                    .deleted
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();

                // Validate that actual implementation occurred
                let has_file_changes = !file_changes.is_empty();
                if !has_file_changes {
                    warn!(
                        task_id = %task_id,
                        "No file changes detected - task may not have been implemented"
                    );
                }

                mission.task_mut(task_id).ok_or_else(task_not_found)?.status =
                    TaskStatus::Verifying;

                let task_for_verify = mission
                    .tasks
                    .iter()
                    .find(|t| t.id == task_id)
                    .ok_or_else(task_not_found)?;

                let verification = self
                    .verifier
                    .verify_task(task_for_verify, &task_result, working_dir)
                    .await;

                // Check basic requirements before convergent verification
                let has_implementation = has_file_changes || !verification.checks.is_empty();

                if !has_implementation {
                    self.fail_task_with_learning(
                        mission,
                        task_id,
                        "No implementation detected - agent did not make any file changes",
                        "NoImplementation",
                        context,
                    )
                    .await;
                    return Ok(());
                }

                let diff = GitRunner::new(working_dir)
                    .diff_for_review(&task_result.files_modified, &task_result.files_created)
                    .await;

                info!(
                    task_id = %task_id,
                    initial_passed = verification.passed,
                    failed_checks = verification.failed_checks().len(),
                    files_created = task_result.files_created.len(),
                    files_modified = task_result.files_modified.len(),
                    has_diff = diff.is_some(),
                    has_test_patterns = !task_for_verify.test_patterns.is_empty(),
                    "Running task-scoped convergent verification"
                );

                let convergence = self
                    .convergent_verifier
                    .verify_until_convergent_task_scoped(
                        mission,
                        task_for_verify.test_patterns.clone(),
                        task_for_verify.module.clone(),
                        &task_result.files_created,
                        &task_result.files_modified,
                        diff.as_deref(),
                        working_dir,
                    )
                    .await?;

                if convergence.converged {
                    info!(
                        task_id = %task_id,
                        rounds = convergence.total_rounds,
                        "Convergent verification succeeded"
                    );
                    self.complete_task_success(mission, task_id, task_result, context)
                        .await?;
                } else {
                    let error_msg = format!(
                        "Convergent verification failed after {} rounds: {:?}",
                        convergence.total_rounds,
                        convergence
                            .persistent_issues
                            .iter()
                            .map(|i| &i.message)
                            .take(self.config.display.max_recent_items)
                            .collect::<Vec<_>>()
                    );

                    self.fail_task_with_learning(
                        mission,
                        task_id,
                        &error_msg,
                        "ConvergenceFailure",
                        context,
                    )
                    .await;
                }
            }
            Err(e) => {
                return Err(e);
            }
        }

        Ok(())
    }

    async fn complete_task_success(
        &self,
        mission: &mut Mission,
        task_id: &str,
        task_result: TaskResult,
        context: &mut MissionContext,
    ) -> Result<()> {
        let mission_id = mission.id.clone();
        let task_not_found = || PilotError::TaskNotFound {
            mission_id: mission_id.clone(),
            task_id: task_id.to_string(),
        };

        let files: Vec<String> = task_result
            .files_modified
            .iter()
            .chain(&task_result.files_created)
            .cloned()
            .collect();

        mission
            .task_mut(task_id)
            .ok_or_else(task_not_found)?
            .complete(task_result);

        // Emit task completed event (before files ownership is transferred)
        self.emit_event(DomainEvent::task_completed(
            &mission_id,
            task_id,
            files.clone(),
            0, // Duration tracked by task itself
        ))
        .await;

        self.context_manager.complete_task(
            context,
            task_id,
            Some(format!("Completed: {} files changed", files.len())),
            files,
        );

        let progress = mission.progress();
        self.notifier
            .notify(
                &MissionEvent::new(EventType::TaskCompleted, &mission_id)
                    .with_task(task_id)
                    .with_progress(progress.completed, progress.total),
            )
            .await;

        info!(
            mission_id = %mission_id,
            task_id = %task_id,
            "Task completed and verified"
        );

        Ok(())
    }

    async fn fail_task_with_learning(
        &self,
        mission: &mut Mission,
        task_id: &str,
        error_msg: &str,
        error_category: &str,
        context: &mut MissionContext,
    ) {
        let mission_id = mission.id.clone();
        let working_dir = self.isolation.working_dir(mission);

        // Track failure in history for intelligent retry analysis
        {
            let mut histories = self.failure_histories.write().await;
            let history = histories
                .entry(format!("{}:{}", mission_id, task_id))
                .or_insert_with(|| {
                    if let Some(task) = mission.task(task_id) {
                        FailureHistory::new(task)
                    } else {
                        FailureHistory {
                            task_id: task_id.to_string(),
                            task_description: "Unknown task".to_string(),
                            attempts: Vec::new(),
                            last_updated_at: Utc::now(),
                        }
                    }
                });
            history.add_attempt(
                error_msg,
                error_category,
                None,
                self.config.recovery.retry_analyzer.max_error_history,
            );
        }

        // Check if task exists and get its retry status
        let task_can_retry = mission
            .task(task_id)
            .map(|t| t.can_retry())
            .unwrap_or(false);
        let task_max_retries = mission.task(task_id).map(|t| t.max_retries).unwrap_or(3);

        // Use LLM-based retry analysis if adaptive limits enabled
        let (should_retry, learning_to_add, escalation_learning) = if self
            .config
            .orchestrator
            .adaptive_limits
            && task_can_retry
        {
            let histories = self.failure_histories.read().await;
            if let Some(history) = histories.get(&format!("{}:{}", mission_id, task_id)) {
                match self.retry_analyzer.analyze(history, &working_dir).await {
                    Ok(RetryDecision::ContinueWithNewApproach {
                        strategy,
                        reasoning,
                    }) => {
                        info!(
                            task_id = %task_id,
                            strategy = %strategy,
                            "LLM suggests new approach for retry"
                        );
                        self.context_manager.add_learning(
                            context,
                            &format!("New approach for {}: {} ({})", task_id, strategy, reasoning),
                        );
                        let learning = Learning::new(
                            LearningCategory::Workaround,
                            format!("Retry strategy for {}: {}", task_id, strategy),
                        )
                        .with_source_task(task_id);
                        (true, Some(learning), None)
                    }
                    Ok(RetryDecision::ContinueSameApproach { reason }) => {
                        debug!(task_id = %task_id, reason = %reason, "Retrying with same approach");
                        (true, None, None)
                    }
                    Ok(RetryDecision::Escalate {
                        reason,
                        diagnosis,
                        suggested_actions,
                    }) => {
                        warn!(
                            task_id = %task_id,
                            reason = %reason,
                            "LLM recommends escalation, marking task as permanently failed"
                        );
                        let escalation = Learning::new(
                            LearningCategory::Gotcha,
                            format!(
                                "Task {} escalated: {}. Diagnosis: {}. Suggested: {}",
                                task_id,
                                reason,
                                diagnosis,
                                suggested_actions.join(", ")
                            ),
                        )
                        .with_source_task(task_id);
                        (false, None, Some((escalation, task_max_retries + 1)))
                    }
                    Err(e) => {
                        warn!(task_id = %task_id, error = %e, "Retry analysis failed, using default behavior");
                        (true, None, None) // Default to retry on analysis failure
                    }
                }
            } else {
                (true, None, None) // No history yet, allow retry
            }
        } else {
            (task_can_retry, None, None)
        };

        // Now apply the changes to the task
        if let Some(task) = mission.task_mut(task_id) {
            // Apply escalation if needed (set retry_count beyond max)
            if let Some((_, new_retry_count)) = &escalation_learning {
                task.retry_count = *new_retry_count;
            }
            task.fail(error_msg.to_string());
        }

        // Add learnings after releasing task borrow
        if let Some(learning) = learning_to_add {
            mission.add_learning(learning);
        }
        if let Some((learning, _)) = escalation_learning {
            mission.add_learning(learning);
        }

        self.context_manager.fail_task(context, task_id, error_msg);

        self.notifier
            .notify(
                &MissionEvent::new(EventType::VerificationFailed, &mission_id)
                    .with_task(task_id)
                    .with_message(error_msg),
            )
            .await;

        if should_retry {
            mission.add_learning(
                Learning::new(
                    LearningCategory::Gotcha,
                    format!("Task {} failed: {}", task_id, error_msg),
                )
                .with_source_task(task_id),
            );
            self.context_manager
                .add_learning(context, &format!("Task {} failed: {}", task_id, error_msg));
        }

        // Emit task failed event
        let retry_count = mission.task(task_id).map(|t| t.retry_count).unwrap_or(0);
        self.emit_event(DomainEvent::task_failed(
            &mission_id,
            task_id,
            error_msg,
            retry_count,
        ))
        .await;

        warn!(
            mission_id = %mission_id,
            task_id = %task_id,
            error = %error_msg,
            will_retry = should_retry,
            "Task verification failed"
        );
    }

    async fn handle_task_failure(
        &self,
        mission: &mut Mission,
        task_id: &str,
        error: PilotError,
        context: &mut MissionContext,
    ) -> TaskFailureOutcome {
        let mission_id = mission.id.clone();

        if let Some(task) = mission.task_mut(task_id) {
            task.fail(error.to_string());
        }

        self.context_manager
            .fail_task(context, task_id, &error.to_string());

        // Upgrade budget complexity if failure rate is high
        let total_retries: u32 = mission.tasks.iter().map(|t| t.retry_count).sum();
        let task_count = mission.tasks.len() as u32;
        self.context_manager
            .upgrade_budget_on_failures(context, total_retries, task_count);

        self.notifier
            .notify(
                &MissionEvent::new(EventType::TaskFailed, &mission_id)
                    .with_task(task_id)
                    .with_message(error.to_string()),
            )
            .await;

        // Attempt recovery if enabled
        if !self.config.recovery.enabled {
            return TaskFailureOutcome::Failed;
        }

        let analysis = crate::recovery::FailureAnalyzer::analyze(
            &error,
            mission,
            crate::recovery::FailurePhase::Execution,
        );

        if !self.recovery_executor.should_auto_recover(&analysis).await {
            return TaskFailureOutcome::Failed;
        }

        let strategy = analysis.suggested_strategy.clone();
        match self
            .recovery_executor
            .execute_recovery(&strategy, mission, context)
            .await
        {
            Ok(crate::recovery::RecoveryStatus::Recovered { message, action }) => {
                info!(task_id = %task_id, message = %message, "Executing recovery action");
                self.execute_recovery_action(mission, context, task_id, action)
                    .await;
                TaskFailureOutcome::Recovered
            }
            Ok(crate::recovery::RecoveryStatus::Escalated { reason }) => {
                warn!(task_id = %task_id, reason = %reason, "Task escalated - suspending mission");
                TaskFailureOutcome::Escalated {
                    reason,
                    analysis: Some(analysis),
                }
            }
            Ok(crate::recovery::RecoveryStatus::Skipped { reason }) => {
                warn!(task_id = %task_id, reason = %reason, "Task skipped");
                TaskFailureOutcome::Skipped
            }
            Ok(crate::recovery::RecoveryStatus::Failed { reason, .. }) => {
                error!(task_id = %task_id, reason = %reason, "Recovery failed");
                TaskFailureOutcome::Failed
            }
            Err(recovery_err) => {
                error!(task_id = %task_id, error = %recovery_err, "Recovery error");
                TaskFailureOutcome::Failed
            }
        }
    }

    async fn execute_recovery_action(
        &self,
        mission: &mut Mission,
        _context: &mut MissionContext,
        task_id: &str,
        action: crate::recovery::RecoveryAction,
    ) {
        use crate::recovery::RecoveryAction;

        match action {
            RecoveryAction::Retry => {
                // Reset task status to pending for retry
                // Note: retry_count was already incremented by task.fail() in handle_task_failure
                if let Some(task) = mission.task_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    info!(task_id = %task_id, retry_count = task.retry_count, "Task reset for retry");
                }
            }
            RecoveryAction::RetryWithReducedChunks { reduction_factor } => {
                // Reset task for retry with reduced scope
                // Note: retry_count was already incremented by task.fail() in handle_task_failure
                if let Some(task) = mission.task_mut(task_id) {
                    task.status = TaskStatus::Pending;
                    // Apply reduction to scope_factor (compounds with previous reductions)
                    task.scope_factor *= reduction_factor;
                    info!(
                        task_id = %task_id,
                        reduction_factor = reduction_factor,
                        new_scope_factor = task.scope_factor,
                        "Task reset for retry with reduced scope"
                    );
                }
            }
            RecoveryAction::StartConvergentVerification { max_rounds } => {
                // Run convergent verification loop with specified max rounds
                // Uses verify_until_convergent_recovery to include AI review (NON-NEGOTIABLE)
                info!(
                    task_id = %task_id,
                    max_rounds = max_rounds,
                    "Starting convergent verification with AI review"
                );
                let working_dir = self.isolation.working_dir(mission);
                match self
                    .convergent_verifier
                    .verify_until_convergent_recovery(mission, Some(max_rounds), &working_dir)
                    .await
                {
                    Ok(result) if result.converged => {
                        if let Some(task) = mission.task_mut(task_id) {
                            task.complete(crate::mission::TaskResult {
                                success: true,
                                output: format!(
                                    "Completed after {} convergent verification rounds",
                                    result.total_rounds
                                ),
                                files_modified: Vec::new(),
                                files_created: Vec::new(),
                                files_deleted: Vec::new(),
                                input_tokens: None,
                                output_tokens: None,
                            });
                            info!(
                                task_id = %task_id,
                                rounds = result.total_rounds,
                                "Task completed after convergent verification"
                            );
                        }
                    }
                    Ok(result) => {
                        warn!(
                            task_id = %task_id,
                            rounds = result.total_rounds,
                            "Convergent verification did not converge"
                        );
                    }
                    Err(e) => {
                        error!(task_id = %task_id, error = %e, "Convergent verification failed");
                    }
                }
            }
        }
    }

    async fn checkpoint(&self, mission: &Mission) -> Result<()> {
        if !self.config.orchestrator.auto_commit {
            return Ok(());
        }

        let progress = mission.progress();
        let message = format!(
            "wip({}): checkpoint {}% ({}/{})",
            mission.id, progress.percentage, progress.completed, progress.total
        );

        self.isolation.commit(mission, &message).await?;

        info!(
            mission_id = %mission.id,
            progress = %progress,
            "Checkpoint created"
        );

        Ok(())
    }

    async fn finalize(&self, mission: &mut Mission) -> Result<()> {
        self.transition_state(
            mission,
            MissionState::Verifying,
            "Starting final verification",
        )
        .await?;

        let working_dir = self.isolation.working_dir(mission);

        // Build task results from mission for final coherence check
        let task_results: Vec<CoherenceTaskResult> = mission
            .tasks
            .iter()
            .filter(|t| t.status == crate::mission::TaskStatus::Completed)
            .map(|t| CoherenceTaskResult::new(&t.id, &t.description))
            .collect();

        // Run final coherence check before other verification
        self.run_final_coherence_check(mission, &task_results)
            .await?;

        let qa_result = self.agent.run_qa(mission, &working_dir).await?;

        if !qa_result.success {
            self.transition_state(mission, MissionState::Failed, "Final QA failed")
                .await?;
            return Err(PilotError::VerificationFailed {
                message: "Final QA failed".into(),
                checks: Vec::new(),
            });
        }

        // Collect all file changes from completed tasks for AI review context
        let (files_created, files_modified): (Vec<String>, Vec<String>) = {
            let mut created = Vec::new();
            let mut modified = Vec::new();
            for task in &mission.tasks {
                if task.status == crate::mission::TaskStatus::Completed
                    && let Some(ref result) = task.result
                {
                    created.extend(result.files_created.clone());
                    modified.extend(result.files_modified.clone());
                }
            }
            // Deduplicate
            created.sort();
            created.dedup();
            modified.sort();
            modified.dedup();
            (created, modified)
        };

        // Compute git diff for AI review (includes both modified and created files)
        let diff = GitRunner::new(&working_dir)
            .diff_for_review(&files_modified, &files_created)
            .await;

        // Run convergent verification for 2-pass requirement (NON-NEGOTIABLE)
        // This ensures the final state passes verification twice with AI review
        // Use verify_until_convergent_with_mission_changes to provide file change info AND diff
        info!(
            mission_id = %mission.id,
            files_created = files_created.len(),
            files_modified = files_modified.len(),
            has_diff = diff.is_some(),
            "Running final convergent verification with AI review for 2-pass requirement"
        );

        let convergence = self
            .convergent_verifier
            .verify_until_convergent_with_mission_changes(
                mission,
                &files_created,
                &files_modified,
                diff.as_deref(),
                &working_dir,
            )
            .await?;

        if !convergence.converged {
            let summary = format!(
                "Final convergent verification failed after {} rounds",
                convergence.total_rounds
            );
            self.transition_state(mission, MissionState::Failed, &summary)
                .await?;

            let checks: Vec<_> = convergence
                .persistent_issues
                .iter()
                .map(|i| {
                    use crate::error::{FailedCheck, FailedCheckType};
                    use crate::verification::IssueCategory;

                    let check_type = match i.category {
                        IssueCategory::BuildError => Some(FailedCheckType::Build),
                        IssueCategory::TestFailure => Some(FailedCheckType::Test),
                        IssueCategory::LintError => Some(FailedCheckType::Lint),
                        IssueCategory::TypeCheckError => Some(FailedCheckType::TypeCheck),
                        IssueCategory::MissingFile => Some(FailedCheckType::FileOperation),
                        _ => Some(FailedCheckType::Custom),
                    };

                    FailedCheck {
                        name: format!("{:?}", i.category),
                        message: i.message.clone(),
                        check_type,
                    }
                })
                .collect();

            return Err(PilotError::VerificationFailed {
                message: summary,
                checks,
            });
        }

        info!(
            mission_id = %mission.id,
            rounds = convergence.total_rounds,
            "Final convergent verification succeeded"
        );

        let final_message = format!("feat({}): {}", mission.id, mission.description);
        self.isolation.commit(mission, &final_message).await?;

        self.complete_mission(mission).await?;

        Ok(())
    }

    async fn complete_mission(&self, mission: &mut Mission) -> Result<()> {
        match &mission.on_complete {
            OnComplete::Direct => {
                if mission.branch.is_some() {
                    self.isolation.merge_to_base(mission).await?;
                }
            }
            OnComplete::PullRequest { reviewers, .. } => {
                let pr_url = self
                    .isolation
                    .create_pull_request(mission, reviewers)
                    .await?;
                info!(mission_id = %mission.id, pr_url = %pr_url, "Pull request created");
            }
            OnComplete::Manual => {
                info!(
                    mission_id = %mission.id,
                    "Mission complete. Run 'claude-pilot merge {}' when ready.",
                    mission.id
                );
            }
        }

        if mission.isolation == IsolationMode::Worktree {
            self.isolation.cleanup(mission, false).await?;
        }

        self.transition_state(
            mission,
            MissionState::Completed,
            "All tasks completed successfully",
        )
        .await?;
        mission.completed_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionCompleted, &mission.id))
            .await;

        let mut signals = self.signals.write().await;
        signals.remove(&mission.id);

        info!(mission_id = %mission.id, "Mission completed");

        // Auto-extract learnings if enabled
        if self.config.learning.auto_extract {
            self.extract_learnings(mission).await;
        }

        // Clean up stale failure histories to prevent memory leak
        self.cleanup_stale_failure_histories().await;

        Ok(())
    }

    async fn extract_learnings(&self, mission: &mut Mission) {
        debug!(mission_id = %mission.id, "Auto-extracting learnings");

        let extractor = LearningExtractor::new(self.paths.clone(), &self.config.agent);
        let working_dir = self.isolation.working_dir(mission);

        match extractor.extract(mission, &working_dir).await {
            Ok(result) => {
                let filtered = self.config.learning.filter(result);
                if !filtered.is_empty() {
                    info!(
                        mission_id = %mission.id,
                        skills = filtered.skills.len(),
                        rules = filtered.rules.len(),
                        agents = filtered.agents.len(),
                        "Extracted learnings (pending approval)"
                    );
                    mission.extracted_candidates = Some(filtered);

                    // Save mission with extracted candidates
                    if let Err(e) = self.store.save(mission).await {
                        warn!(error = %e, "Failed to save extracted candidates");
                    }
                } else {
                    debug!(mission_id = %mission.id, "No learnings extracted (filtered out)");
                }
            }
            Err(e) => {
                warn!(mission_id = %mission.id, error = %e, "Learning extraction failed");
            }
        }
    }

    /// Remove stale failure history entries to prevent unbounded memory growth.
    /// TTL is configured via `recovery.failure_history_ttl_secs`.
    async fn cleanup_stale_failure_histories(&self) {
        let ttl = std::time::Duration::from_secs(self.config.recovery.failure_history_ttl_secs);
        let mut histories = self.failure_histories.write().await;
        let before_count = histories.len();

        histories.retain(|_, history| !history.is_stale(ttl));

        let removed = before_count - histories.len();
        if removed > 0 {
            debug!(
                removed_count = removed,
                remaining_count = histories.len(),
                "Cleaned up stale failure histories"
            );
        }
    }

    async fn run_incremental_coherence(
        &self,
        mission: &Mission,
        task_results: &[CoherenceTaskResult],
    ) {
        if !self.config.coherence.enabled {
            return;
        }

        match self
            .coherence_checker
            .check_all(task_results, std::slice::from_ref(&mission.description))
            .await
        {
            Ok(report) => {
                if report.overall_passed {
                    debug!(
                        mission_id = %mission.id,
                        integration = report.integration_soundness.score,
                        "Incremental coherence check passed"
                    );
                } else {
                    warn!(
                        mission_id = %mission.id,
                        summary = %report.summary(),
                        "Incremental coherence issues detected"
                    );
                    for issue in report.all_issues() {
                        warn!(
                            check_type = ?issue.check_type,
                            severity = ?issue.severity,
                            description = %issue.description,
                            "Coherence issue"
                        );
                    }
                }
            }
            Err(e) => {
                warn!(mission_id = %mission.id, error = %e, "Incremental coherence check failed");
            }
        }
    }

    async fn run_final_coherence_check(
        &self,
        mission: &Mission,
        task_results: &[CoherenceTaskResult],
    ) -> Result<()> {
        if !self.config.coherence.enabled || !self.config.coherence.run_after_all_tasks {
            return Ok(());
        }

        info!(mission_id = %mission.id, "Running final coherence check");

        let report = self
            .coherence_checker
            .check_all(task_results, std::slice::from_ref(&mission.description))
            .await?;

        if !report.overall_passed && self.config.coherence.fail_fast {
            return Err(PilotError::VerificationFailed {
                message: report.summary(),
                checks: report
                    .all_issues()
                    .iter()
                    .map(|i| {
                        // Coherence issues are all Custom type (not build/test/lint)
                        crate::error::FailedCheck::new(
                            format!("{:?}", i.check_type),
                            i.description.clone(),
                        )
                        .with_type(crate::error::FailedCheckType::Custom)
                    })
                    .collect(),
            });
        }

        info!(
            mission_id = %mission.id,
            passed = report.overall_passed,
            summary = %report.summary(),
            "Final coherence check complete"
        );

        Ok(())
    }

    pub async fn pause(&self, mission_id: &str) -> Result<()> {
        let mission = self.store.load(mission_id).await?;

        if !mission.status.is_active() {
            return Err(PilotError::InvalidMissionState {
                expected: "active".into(),
                actual: mission.status.to_string(),
            });
        }

        let has_handler = {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.pause();
                info!(mission_id = %mission_id, "Pause signal sent");
                true
            } else {
                false
            }
        }; // Drop read lock before async operations

        if !has_handler {
            let mut mission = mission;
            self.transition_state(&mut mission, MissionState::Paused, "Pause requested")
                .await?;
            self.notifier
                .notify(&MissionEvent::new(EventType::MissionPaused, mission_id))
                .await;
            info!(mission_id = %mission_id, "Mission paused (not running)");
        }

        Ok(())
    }

    pub async fn resume(&self, mission_id: &str) -> Result<()> {
        let mut mission = self.store.load(mission_id).await?;

        if !mission.status.can_resume() {
            return Err(PilotError::InvalidMissionState {
                expected: "paused or escalated".into(),
                actual: mission.status.to_string(),
            });
        }

        // Transition from Escalated/Paused to Running before executing
        // This must happen BEFORE execute() which rejects Escalated state
        if mission.status == MissionState::Escalated || mission.status == MissionState::Paused {
            let previous_state = mission.status;
            self.transition_state(&mut mission, MissionState::Running, "Resumed by user")
                .await?;
            info!(
                mission_id = %mission_id,
                previous_state = %previous_state,
                "Mission resumed, transitioned to Running"
            );
        }

        // Clear escalation context on resume - the human intervention is complete
        if mission.escalation.is_some() {
            info!(
                mission_id = %mission_id,
                escalation_id = mission.escalation.as_ref().map(|e| e.id.as_str()).unwrap_or(""),
                "Clearing escalation context on resume"
            );
            mission.escalation = None;
        }

        // Save state changes before executing
        self.store.save(&mission).await?;

        {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.clear();
            }
        }

        self.execute(mission_id).await
    }

    pub async fn retry(&self, mission_id: &str, reset_all: bool, force: bool) -> Result<()> {
        let mut mission = self.store.load(mission_id).await?;

        let is_orphan = force
            && mission.status == MissionState::Running
            && self
                .lifecycle
                .is_orphan_state(mission_id)
                .await
                .unwrap_or(false);

        let can_retry = mission.status.can_retry() || is_orphan;

        if !can_retry {
            if force && mission.status == MissionState::Running {
                return Err(PilotError::MissionAlreadyRunning {
                    mission_id: mission_id.to_string(),
                    pid: self
                        .lifecycle
                        .read_lock(mission_id)
                        .await?
                        .map(|l| l.pid)
                        .unwrap_or(0),
                });
            }
            return Err(PilotError::InvalidMissionState {
                expected: "failed or cancelled".into(),
                actual: mission.status.to_string(),
            });
        }

        if is_orphan {
            info!(mission_id, "Recovering orphan mission");
            let _ = self.lifecycle.release_lock(mission_id).await;
        }

        mission.iteration = 0;

        if reset_all {
            for task in &mut mission.tasks {
                task.status = TaskStatus::Pending;
                task.retry_count = 0;
            }
            self.transition_state(&mut mission, MissionState::Planning, "Retry from start")
                .await?;
            info!(mission_id = %mission_id, "Retrying mission from start");
        } else {
            for task in &mut mission.tasks {
                if task.status == TaskStatus::Failed || task.status == TaskStatus::InProgress {
                    task.status = TaskStatus::Pending;
                    task.retry_count = 0;
                }
            }
            self.transition_state(&mut mission, MissionState::Running, "Retry failed tasks")
                .await?;
            info!(mission_id = %mission_id, "Retrying failed and orphaned tasks");
        }

        // Sync context with updated task states to maintain consistency
        let mut context = self.context_manager.load_or_init(&mission).await?;
        self.context_manager
            .update_from_mission(&mut context, &mission);
        self.context_manager.save(&context).await?;

        {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.clear();
            }
        }

        self.execute(mission_id).await
    }

    pub async fn cancel(&self, mission_id: &str) -> Result<()> {
        let mission = self.store.load(mission_id).await?;

        if !mission.status.can_cancel() {
            return Err(PilotError::InvalidMissionState {
                expected: "non-terminal".into(),
                actual: mission.status.to_string(),
            });
        }

        let signals = self.signals.read().await;
        if let Some(handler) = signals.get(mission_id) {
            handler.cancel();
            info!(mission_id = %mission_id, "Cancel signal sent");
        } else {
            drop(signals);
            let mut mission = mission;
            if mission.isolation == IsolationMode::Worktree {
                self.isolation.cleanup(&mission, true).await?;
            }
            self.transition_state(&mut mission, MissionState::Cancelled, "Cancel requested")
                .await?;
            self.notifier
                .notify(&MissionEvent::new(EventType::MissionCancelled, mission_id))
                .await;
            info!(mission_id = %mission_id, "Mission cancelled (not running)");
        }

        Ok(())
    }

    pub async fn get_status(&self, mission_id: &str) -> Result<Mission> {
        self.store.load(mission_id).await
    }

    /// Check if a mission in "running" state is actually orphaned (process died).
    pub async fn is_orphan_state(&self, mission_id: &str) -> Result<bool> {
        let mission = self.store.load(mission_id).await?;

        // Only check for running missions
        if mission.status != MissionState::Running {
            return Ok(false);
        }

        self.lifecycle.is_orphan_state(mission_id).await
    }

    /// Mark an orphan running mission as failed.
    /// Returns true if the mission was actually orphaned and marked as failed.
    pub async fn recover_orphan_state(&self, mission_id: &str) -> Result<bool> {
        if !self.is_orphan_state(mission_id).await? {
            return Ok(false);
        }

        let mut mission = self.store.load(mission_id).await?;

        warn!(
            mission_id = %mission_id,
            "Detected orphan running state, marking as failed"
        );

        // Mark any in_progress tasks as failed too
        for task in &mut mission.tasks {
            if task.status == TaskStatus::InProgress {
                task.status = TaskStatus::Failed;
            }
        }

        self.transition_state(&mut mission, MissionState::Failed, "Orphan state detected")
            .await?;

        // Clean up any stale lock file
        let _ = self.lifecycle.release_lock(mission_id).await;

        Ok(true)
    }

    pub async fn list_missions(&self) -> Result<Vec<Mission>> {
        self.store.list().await
    }

    pub fn store(&self) -> &MissionStore {
        &self.store
    }

    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    pub fn lifecycle(&self) -> &LifecycleManager {
        &self.lifecycle
    }

    /// Shutdown the multi-agent coordinator if enabled.
    ///
    /// This should be called when the orchestrator is shutting down to ensure
    /// proper cleanup of agent resources.
    pub fn shutdown_coordinator(&self) {
        if let Some(ref coordinator) = self.coordinator {
            info!("Shutting down multi-agent coordinator");
            coordinator.shutdown();
        }
    }

    /// Check if multi-agent coordinator is enabled and active.
    pub fn is_coordinator_active(&self) -> bool {
        self.coordinator.as_ref().is_some_and(|c| !c.is_shutdown())
    }

    /// Get coordinator execution metrics if available.
    pub fn coordinator_metrics(&self) -> Option<crate::agent::MetricsSnapshot> {
        self.coordinator.as_ref().map(|c| c.metrics())
    }

    /// Check if event sourcing is enabled and active.
    pub fn is_event_store_active(&self) -> bool {
        self.event_store.is_some()
    }

    /// Get a reference to the event store if enabled.
    pub fn event_store(&self) -> Option<&Arc<EventStore>> {
        self.event_store.as_ref()
    }

    async fn transition_state(
        &self,
        mission: &mut Mission,
        target: MissionState,
        reason: &str,
    ) -> Result<()> {
        if !mission.status.can_transition_to(target) {
            error!(
                mission_id = %mission.id,
                from = %mission.status,
                to = %target,
                reason = %reason,
                allowed = ?mission.status.allowed_transitions(),
                "Invalid state transition attempted"
            );
            return Err(PilotError::InvalidMissionState {
                expected: format!("one of {:?}", mission.status.allowed_transitions()),
                actual: format!("{} -> {} ({})", mission.status, target, reason),
            });
        }

        let transition = StateTransition::new(mission.status, target, reason);

        debug!(
            mission_id = %mission.id,
            from = %mission.status,
            to = %target,
            reason = %reason,
            "State transition"
        );

        mission.state_history.push(transition);
        let from_state = mission.status;
        mission.status = target;
        self.store.save(mission).await?;

        // Emit state change event to event store
        if let Some(ref store) = self.event_store {
            let event = DomainEvent::state_changed(&mission.id, from_state, target, reason);
            if let Err(e) = store.append(event).await {
                warn!(error = %e, "Failed to emit state change event");
            }
        }

        Ok(())
    }

    /// Emit a domain event to the event store if enabled.
    async fn emit_event(&self, event: DomainEvent) {
        if let Some(ref store) = self.event_store {
            let event_type = event.event_type();
            if let Err(e) = store.append(event).await {
                warn!(error = %e, event_type, "Failed to emit event");
            }
        }
    }
}
