//! Checkpoint management for session recovery.
//!
//! Provides checkpoint creation, persistence, and recovery capabilities
//! for orchestration sessions. Checkpoints are lightweight references
//! to external state stores (notification log, consensus state, task graph).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Reason for creating a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointReason {
    /// Scheduled checkpoint (time-based).
    Scheduled,
    /// Phase transition.
    PhaseTransition { from: String, to: String },
    /// Context budget threshold reached.
    ContextBudgetThreshold { used: u32, threshold: u32 },
    /// Manual checkpoint request.
    Manual { description: String },
    /// Before risky operation.
    PreOperation { operation: String },
    /// After significant progress.
    Milestone { description: String },
    /// Error recovery point.
    ErrorRecovery { error: String },
}

/// Reference to external state stores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRef {
    /// Latest notification ID at checkpoint time.
    pub notification_id: u64,
    /// Task graph snapshot (task statuses).
    pub task_statuses: HashMap<String, String>,
    /// Consensus state reference (if any).
    pub consensus_round: Option<u32>,
    /// File modification snapshots.
    pub file_snapshots: HashMap<String, FileSnapshot>,
}

/// Snapshot of a file's state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub hash: String,
    pub modified_at: u64,
    pub size: u64,
}

/// Reference to compaction archive for full history retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveRef {
    pub archive_id: String,
    pub snapshot_count: usize,
    pub last_snapshot_seq: u64,
}

/// Compacted context for LLM continuity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactedContext {
    /// Summary of completed work.
    pub work_summary: String,
    /// Critical decisions made.
    pub decisions: Vec<String>,
    /// Active focus areas.
    pub focus_areas: Vec<String>,
    /// Token count of the compacted context.
    pub token_count: usize,
}

impl CompactedContext {
    pub fn to_llm_context(&self) -> String {
        let mut context = String::new();
        context.push_str("## Work Summary\n");
        context.push_str(&self.work_summary);
        context.push_str("\n\n## Key Decisions\n");
        for decision in &self.decisions {
            context.push_str(&format!("- {}\n", decision));
        }
        if !self.focus_areas.is_empty() {
            context.push_str("\n## Active Focus\n");
            for focus in &self.focus_areas {
                context.push_str(&format!("- {}\n", focus));
            }
        }
        context
    }
}

/// A checkpoint capturing session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique checkpoint ID.
    pub id: String,
    /// Session ID this checkpoint belongs to.
    pub session_id: String,
    /// Reason for the checkpoint.
    pub reason: CheckpointReason,
    /// References to external state.
    pub refs: CheckpointRef,
    /// Current phase at checkpoint time.
    pub phase: String,
    /// Completed task IDs.
    pub completed_tasks: Vec<String>,
    /// Failed task IDs.
    pub failed_tasks: Vec<String>,
    /// In-progress task IDs.
    pub in_progress_tasks: Vec<String>,
    /// Context budget used at checkpoint.
    pub context_used: u32,
    /// Timestamp (milliseconds since session start).
    #[serde(skip)]
    pub created_at: Option<Instant>,
    /// Metadata.
    pub metadata: HashMap<String, String>,
    /// Compacted context for LLM continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacted_context: Option<CompactedContext>,
    /// Archive reference for full history retrieval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_ref: Option<ArchiveRef>,
}

impl Checkpoint {
    pub fn new(session_id: &str, reason: CheckpointReason) -> Self {
        Self {
            id: format!("cp-{}", uuid_v4()),
            session_id: session_id.to_string(),
            reason,
            refs: CheckpointRef {
                notification_id: 0,
                task_statuses: HashMap::new(),
                consensus_round: None,
                file_snapshots: HashMap::new(),
            },
            phase: String::new(),
            completed_tasks: Vec::new(),
            failed_tasks: Vec::new(),
            in_progress_tasks: Vec::new(),
            context_used: 0,
            created_at: Some(Instant::now()),
            metadata: HashMap::new(),
            compacted_context: None,
            archive_ref: None,
        }
    }

    pub fn with_compacted_context(mut self, context: CompactedContext) -> Self {
        self.compacted_context = Some(context);
        self
    }

    pub fn with_archive_ref(mut self, archive_ref: ArchiveRef) -> Self {
        self.archive_ref = Some(archive_ref);
        self
    }

    pub fn with_refs(mut self, refs: CheckpointRef) -> Self {
        self.refs = refs;
        self
    }

    pub fn with_phase(mut self, phase: &str) -> Self {
        self.phase = phase.to_string();
        self
    }

    pub fn with_tasks(
        mut self,
        completed: Vec<String>,
        failed: Vec<String>,
        in_progress: Vec<String>,
    ) -> Self {
        self.completed_tasks = completed;
        self.failed_tasks = failed;
        self.in_progress_tasks = in_progress;
        self
    }

    pub fn with_context_used(mut self, used: u32) -> Self {
        self.context_used = used;
        self
    }

    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata.insert(key.to_string(), value.to_string());
        self
    }

    pub fn age(&self) -> Option<Duration> {
        self.created_at.map(|t| t.elapsed())
    }

    pub fn summary(&self) -> String {
        format!(
            "Checkpoint {} ({}): phase={}, completed={}, failed={}, in_progress={}",
            self.id,
            match &self.reason {
                CheckpointReason::Scheduled => "scheduled".to_string(),
                CheckpointReason::PhaseTransition { from, to } => format!("{}->{}", from, to),
                CheckpointReason::ContextBudgetThreshold { used, threshold } => {
                    format!("budget {}/{}", used, threshold)
                }
                CheckpointReason::Manual { description } => format!("manual: {}", description),
                CheckpointReason::PreOperation { operation } => format!("pre-op: {}", operation),
                CheckpointReason::Milestone { description } =>
                    format!("milestone: {}", description),
                CheckpointReason::ErrorRecovery { error } => format!("error: {}", error),
            },
            self.phase,
            self.completed_tasks.len(),
            self.failed_tasks.len(),
            self.in_progress_tasks.len()
        )
    }
}

/// Context for recovery from a checkpoint.
#[derive(Debug, Clone)]
pub struct RecoveryContext {
    /// The checkpoint to recover from.
    pub checkpoint: Checkpoint,
    /// Tasks to retry (were in-progress).
    pub tasks_to_retry: Vec<String>,
    /// Tasks to skip (already completed).
    pub tasks_to_skip: Vec<String>,
    /// Whether to replay notifications from checkpoint.
    pub replay_notifications: bool,
    /// Starting notification ID for replay.
    pub notification_start_id: u64,
}

impl RecoveryContext {
    pub fn from_checkpoint(checkpoint: Checkpoint) -> Self {
        let tasks_to_retry = checkpoint.in_progress_tasks.clone();
        let tasks_to_skip = checkpoint.completed_tasks.clone();
        let notification_start_id = checkpoint.refs.notification_id;

        Self {
            checkpoint,
            tasks_to_retry,
            tasks_to_skip,
            replay_notifications: true,
            notification_start_id,
        }
    }

    pub fn without_notification_replay(mut self) -> Self {
        self.replay_notifications = false;
        self
    }
}

/// Manager for checkpoint operations.
#[derive(Debug)]
pub struct CheckpointManager {
    session_id: String,
    checkpoints: Vec<Checkpoint>,
    storage_path: Option<PathBuf>,
    auto_checkpoint_interval: Duration,
    context_budget_threshold: f32,
    last_auto_checkpoint: Instant,
    max_checkpoints: usize,
}

impl CheckpointManager {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            checkpoints: Vec::new(),
            storage_path: None,
            auto_checkpoint_interval: Duration::from_secs(300), // 5 minutes
            context_budget_threshold: 0.8,                      // 80%
            last_auto_checkpoint: Instant::now(),
            max_checkpoints: 50,
        }
    }

    pub fn with_storage(mut self, path: PathBuf) -> Self {
        self.storage_path = Some(path);
        self
    }

    pub fn with_auto_interval(mut self, interval: Duration) -> Self {
        self.auto_checkpoint_interval = interval;
        self
    }

    pub fn with_budget_threshold(mut self, threshold: f32) -> Self {
        self.context_budget_threshold = threshold.clamp(0.5, 0.95);
        self
    }

    pub fn with_max_checkpoints(mut self, max: usize) -> Self {
        self.max_checkpoints = max.max(5);
        self
    }

    /// Create a checkpoint.
    pub fn create(&mut self, checkpoint: Checkpoint) -> &Checkpoint {
        self.checkpoints.push(checkpoint);
        self.cleanup_old_checkpoints();

        if let Some(path) = &self.storage_path
            && let Err(e) = self.persist_to_disk(path)
        {
            tracing::warn!("Failed to persist checkpoint: {}", e);
        }

        self.checkpoints.last().unwrap()
    }

    /// Create a checkpoint for phase transition.
    pub fn create_phase_transition(
        &mut self,
        from: &str,
        to: &str,
        refs: CheckpointRef,
        completed: Vec<String>,
        failed: Vec<String>,
        in_progress: Vec<String>,
    ) -> &Checkpoint {
        let cp = Checkpoint::new(
            &self.session_id,
            CheckpointReason::PhaseTransition {
                from: from.to_string(),
                to: to.to_string(),
            },
        )
        .with_refs(refs)
        .with_phase(to)
        .with_tasks(completed, failed, in_progress);

        self.create(cp)
    }

    /// Create a checkpoint for context budget threshold.
    pub fn create_budget_threshold(
        &mut self,
        used: u32,
        threshold: u32,
        refs: CheckpointRef,
        phase: &str,
    ) -> &Checkpoint {
        let cp = Checkpoint::new(
            &self.session_id,
            CheckpointReason::ContextBudgetThreshold { used, threshold },
        )
        .with_refs(refs)
        .with_phase(phase)
        .with_context_used(used);

        self.create(cp)
    }

    /// Create a scheduled checkpoint if interval elapsed.
    pub fn maybe_create_scheduled(
        &mut self,
        refs: CheckpointRef,
        phase: &str,
        completed: Vec<String>,
        failed: Vec<String>,
        in_progress: Vec<String>,
    ) -> Option<&Checkpoint> {
        if self.last_auto_checkpoint.elapsed() >= self.auto_checkpoint_interval {
            self.last_auto_checkpoint = Instant::now();
            let cp = Checkpoint::new(&self.session_id, CheckpointReason::Scheduled)
                .with_refs(refs)
                .with_phase(phase)
                .with_tasks(completed, failed, in_progress);
            Some(self.create(cp))
        } else {
            None
        }
    }

    /// Check if context budget threshold is reached.
    pub fn should_checkpoint_for_budget(&self, used: u32, total: u32) -> bool {
        if total == 0 {
            return false;
        }
        (used as f32 / total as f32) >= self.context_budget_threshold
    }

    /// Get the latest checkpoint.
    pub fn latest(&self) -> Option<&Checkpoint> {
        self.checkpoints.last()
    }

    /// Get checkpoint by ID.
    pub fn get(&self, id: &str) -> Option<&Checkpoint> {
        self.checkpoints.iter().find(|cp| cp.id == id)
    }

    /// Get all checkpoints.
    pub fn all(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Create recovery context from latest checkpoint.
    pub fn create_recovery_context(&self) -> Option<RecoveryContext> {
        self.latest()
            .map(|cp| RecoveryContext::from_checkpoint(cp.clone()))
    }

    /// Create recovery context from specific checkpoint.
    pub fn create_recovery_context_from(&self, checkpoint_id: &str) -> Option<RecoveryContext> {
        self.get(checkpoint_id)
            .map(|cp| RecoveryContext::from_checkpoint(cp.clone()))
    }

    /// Count checkpoints.
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    fn cleanup_old_checkpoints(&mut self) {
        while self.checkpoints.len() > self.max_checkpoints {
            // Keep phase transition and error recovery checkpoints longer
            let idx = self
                .checkpoints
                .iter()
                .position(|cp| {
                    !matches!(
                        cp.reason,
                        CheckpointReason::PhaseTransition { .. }
                            | CheckpointReason::ErrorRecovery { .. }
                    )
                })
                .unwrap_or(0);
            self.checkpoints.remove(idx);
        }
    }

    fn persist_to_disk(&self, path: &PathBuf) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&self.checkpoints)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load checkpoints from disk.
    pub fn load_from_disk(session_id: &str, path: &PathBuf) -> std::io::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let checkpoints: Vec<Checkpoint> = serde_json::from_str(&json)?;

        Ok(Self {
            session_id: session_id.to_string(),
            checkpoints,
            storage_path: Some(path.clone()),
            auto_checkpoint_interval: Duration::from_secs(300),
            context_budget_threshold: 0.8,
            last_auto_checkpoint: Instant::now(),
            max_checkpoints: 50,
        })
    }
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_creation() {
        let mut manager = CheckpointManager::new("session-1");

        let refs = CheckpointRef {
            notification_id: 42,
            task_statuses: HashMap::new(),
            consensus_round: Some(3),
            file_snapshots: HashMap::new(),
        };

        let cp = manager.create_phase_transition(
            "planning",
            "implementation",
            refs,
            vec!["task-1".to_string()],
            Vec::new(),
            vec!["task-2".to_string()],
        );

        assert!(cp.id.starts_with("cp-"));
        assert_eq!(cp.session_id, "session-1");
        assert_eq!(cp.phase, "implementation");
        assert_eq!(cp.completed_tasks.len(), 1);
        assert_eq!(cp.in_progress_tasks.len(), 1);
    }

    #[test]
    fn test_recovery_context() {
        let mut manager = CheckpointManager::new("session-1");

        let refs = CheckpointRef {
            notification_id: 100,
            task_statuses: HashMap::new(),
            consensus_round: None,
            file_snapshots: HashMap::new(),
        };

        manager.create_phase_transition(
            "planning",
            "implementation",
            refs,
            vec!["task-1".to_string()],
            vec!["task-2".to_string()],
            vec!["task-3".to_string(), "task-4".to_string()],
        );

        let recovery = manager.create_recovery_context().unwrap();

        assert_eq!(recovery.tasks_to_skip.len(), 1);
        assert_eq!(recovery.tasks_to_retry.len(), 2);
        assert_eq!(recovery.notification_start_id, 100);
    }

    #[test]
    fn test_budget_threshold_check() {
        let manager = CheckpointManager::new("session-1").with_budget_threshold(0.8);

        assert!(!manager.should_checkpoint_for_budget(7000, 10000));
        assert!(manager.should_checkpoint_for_budget(8000, 10000));
        assert!(manager.should_checkpoint_for_budget(9000, 10000));
    }

    #[test]
    fn test_max_checkpoints_cleanup() {
        let mut manager = CheckpointManager::new("session-1").with_max_checkpoints(5);

        for i in 0..10 {
            let cp = Checkpoint::new("session-1", CheckpointReason::Scheduled)
                .with_phase(&format!("phase-{}", i));
            manager.create(cp);
        }

        assert!(manager.len() <= 5);
    }
}
