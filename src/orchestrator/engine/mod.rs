use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::focus::FocusTracker;
use super::lifecycle::LifecycleManager;
use super::signal::SignalHandler;
use crate::agent::multi::Coordinator;
use crate::agent::{DirectAgent, TaskAgent};
use crate::config::{PilotConfig, ProjectPaths};
use crate::context::ContextManager;
use crate::error::{MissionError, Result};
use crate::notification::Notifier;
use crate::quality::{CoherenceChecker, EnhancedEvidenceGatherer};
use crate::recovery::{FailureHistory, RecoveryExecutor, RetryAnalyzer};
use crate::state::{DomainEvent, EventReplayer, EventStore, MissionState};
use crate::verification::ConvergentVerifier;

use crate::isolation::IsolationManager;
use crate::mission::MissionStore;
use crate::verification::Verifier;

mod execution;
mod lifecycle;
mod phases;
mod recovery;
mod verification;

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
        let available_strategies = crate::verification::FixStrategy::all_strategies().len();
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
        let learning_history = crate::learning::RetryHistory::from_config(
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
            match crate::state::ProjectionSnapshotStore::new(&snapshot_db) {
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
            .active_missions()
            .await?
            .iter()
            .filter(|m| m.worktree_path.is_some())
            .map(|m| m.id.clone())
            .collect();

        // Cleanup orphaned worktrees from crashed sessions
        self.isolation.cleanup_orphaned(&active_ids).await?;

        Ok(())
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
    pub fn coordinator_metrics(&self) -> Option<crate::agent::AgentExecutionMetrics> {
        self.coordinator.as_ref().map(|c| c.metrics())
    }

    /// Broadcast a signal to all active mission handlers.
    pub async fn broadcast_signal(&self, signal: super::signal::Signal) {
        let signals = self.signals.read().await;
        for handler in signals.values() {
            handler.send(signal);
        }
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
        mission: &mut crate::mission::Mission,
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
            return Err(MissionError::InvalidState {
                expected: format!("one of {:?}", mission.status.allowed_transitions()),
                actual: format!("{} -> {} ({})", mission.status, target, reason),
            }
            .into());
        }

        let transition = crate::state::StateTransition::new(mission.status, target, reason);

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
