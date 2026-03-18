//! Read models (projections) built from events with snapshot support.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::events::{DomainEvent, EventPayload};
use crate::agent::multi::session::SessionPhase;
use crate::agent::multi::shared::ConsensusOutcome;
use crate::domain::{Severity, VoteDecision};
use crate::mission::TaskStatus;
use crate::state::MissionState;
use crate::verification::{FixStrategy, IssueCategory};

pub trait Projection: Default {
    fn apply(&mut self, event: &DomainEvent);
    fn version(&self) -> u32;
    fn last_applied_seq(&self) -> u64;
    fn set_last_applied_seq(&mut self, seq: u64);

    /// Schema version for snapshot compatibility.
    /// Bump when adding/removing/renaming fields to invalidate old snapshots.
    fn schema_version() -> u32 {
        1
    }

    fn apply_idempotent(&mut self, event: &DomainEvent) {
        if event.global_seq <= self.last_applied_seq() {
            return;
        }
        self.apply(event);
        self.set_last_applied_seq(event.global_seq);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgressProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub mission_id: Option<String>,
    pub state: MissionState,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
    pub deferred_tasks: usize,
    pub current_task: Option<String>,
    pub current_verification_round: u32,
    pub total_issues_detected: usize,
    pub total_issues_resolved: usize,
    pub started_at: Option<DateTime<Utc>>,
    pub last_activity: Option<DateTime<Utc>>,
}

impl Projection for ProgressProjection {
    fn apply(&mut self, event: &DomainEvent) {
        self.version = event.version;
        self.last_activity = Some(event.timestamp);

        if self.mission_id.is_none() {
            self.mission_id = Some(event.aggregate_id.to_string());
        }

        match &event.payload {
            EventPayload::MissionCreated { .. } => {
                self.started_at = Some(event.timestamp);
            }
            EventPayload::StateChanged { to, .. } => {
                self.state = *to;
            }
            EventPayload::TaskStarted { task_id, .. } => {
                self.current_task = Some(task_id.clone());
            }
            EventPayload::TaskCompleted { .. } => {
                self.completed_tasks += 1;
                self.current_task = None;
            }
            EventPayload::TaskFailed { .. } => {
                self.failed_tasks += 1;
            }
            EventPayload::TaskDeferred { .. } => {
                self.deferred_tasks += 1;
            }
            EventPayload::PlanGenerated { task_count, .. } => {
                self.total_tasks = *task_count;
            }
            EventPayload::VerificationRound { round, .. } => {
                self.current_verification_round = *round;
            }
            EventPayload::IssueDetected { .. } => {
                self.total_issues_detected += 1;
            }
            EventPayload::IssueResolved { .. } => {
                self.total_issues_resolved += 1;
            }
            _ => {}
        }
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn last_applied_seq(&self) -> u64 {
        self.last_applied_seq
    }

    fn set_last_applied_seq(&mut self, seq: u64) {
        self.last_applied_seq = seq;
    }
}

impl ProgressProjection {
    pub fn completion_percentage(&self) -> f32 {
        if self.total_tasks == 0 {
            return 0.0;
        }
        (self.completed_tasks as f32 / self.total_tasks as f32) * 100.0
    }

    pub fn is_healthy(&self) -> bool {
        !matches!(self.state, MissionState::Failed | MissionState::Cancelled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub summary: String,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimelineProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub entries: VecDeque<TimelineEntry>,
    pub max_entries: usize,
}

impl TimelineProjection {
    pub fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            ..Default::default()
        }
    }
}

impl Projection for TimelineProjection {
    fn apply(&mut self, event: &DomainEvent) {
        self.version = event.version;

        let summary = match &event.payload {
            EventPayload::MissionCreated { description } => {
                format!("Mission created: {}", crate::utils::truncate_at_boundary(description, 50))
            }
            EventPayload::StateChanged { from, to, reason } => {
                format!("{:?} → {:?}: {}", from, to, crate::utils::truncate_at_boundary(reason, 30))
            }
            EventPayload::TaskStarted {
                task_id,
                description,
            } => {
                format!("Task {} started: {}", task_id, crate::utils::truncate_at_boundary(description, 30))
            }
            EventPayload::TaskCompleted {
                task_id,
                files_modified,
                duration_ms,
            } => {
                format!(
                    "Task {} completed: {} files, {}ms",
                    task_id,
                    files_modified.len(),
                    duration_ms
                )
            }
            EventPayload::TaskFailed {
                task_id,
                error,
                retry_count,
            } => {
                format!(
                    "Task {} failed (retry {}): {}",
                    task_id,
                    retry_count,
                    crate::utils::truncate_at_boundary(error, 30)
                )
            }
            EventPayload::VerificationRound {
                round,
                passed,
                issue_count,
                ..
            } => {
                format!(
                    "Verification round {}: {} ({} issues)",
                    round,
                    if *passed { "PASS" } else { "FAIL" },
                    issue_count
                )
            }
            EventPayload::ConvergenceAchieved {
                total_rounds,
                clean_rounds,
            } => {
                format!(
                    "Converged after {} rounds ({} clean)",
                    total_rounds, clean_rounds
                )
            }
            EventPayload::FixAttempted {
                issue_id,
                strategy,
                success,
            } => {
                format!(
                    "Fix {:?} on {}: {}",
                    strategy,
                    crate::utils::truncate_at_boundary(issue_id, 10),
                    if *success { "OK" } else { "FAIL" }
                )
            }
            EventPayload::ConsensusCompleted {
                rounds,
                respondent_count,
                outcome,
            } => {
                format!(
                    "Consensus {} ({} rounds, {} respondents)",
                    outcome.as_str(),
                    rounds,
                    respondent_count
                )
            }
            EventPayload::MultiAgentPhaseCompleted {
                phase,
                task_count,
                success_count,
                duration_ms,
            } => {
                format!(
                    "Phase '{}': {}/{} tasks succeeded ({}ms)",
                    phase.as_str(), success_count, task_count, duration_ms
                )
            }
            _ => event.event_type().to_string(),
        };

        self.entries.push_back(TimelineEntry {
            timestamp: event.timestamp,
            event_type: event.event_type().to_string(),
            summary,
            task_id: event.payload.task_id().map(String::from),
        });

        if self.entries.len() > self.max_entries {
            self.entries.pop_front();
        }
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn last_applied_seq(&self) -> u64 {
        self.last_applied_seq
    }

    fn set_last_applied_seq(&mut self, seq: u64) {
        self.last_applied_seq = seq;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternStats {
    pub category: IssueCategory,
    pub strategy: FixStrategy,
    pub success_count: u32,
    pub failure_count: u32,
}

impl PatternStats {
    pub fn success_rate(&self) -> f32 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f32 / total as f32
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternStatsProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub stats: HashMap<String, PatternStats>,
    pub total_patterns_learned: usize,
    pub total_patterns_applied: usize,
}

impl Projection for PatternStatsProjection {
    fn apply(&mut self, event: &DomainEvent) {
        self.version = event.version;

        match &event.payload {
            EventPayload::PatternLearned {
                category,
                strategy,
                success,
                ..
            } => {
                let key = format!("{category:?}:{strategy:?}");
                let entry = self.stats.entry(key).or_insert_with(|| PatternStats {
                    category: category.clone(),
                    strategy: *strategy,
                    success_count: 0,
                    failure_count: 0,
                });
                if *success {
                    entry.success_count += 1;
                } else {
                    entry.failure_count += 1;
                }
                self.total_patterns_learned += 1;
            }
            EventPayload::PatternApplied { .. } => {
                self.total_patterns_applied += 1;
            }
            EventPayload::FixAttempted {
                strategy, success, ..
            } => {
                let key = format!("Other:{:?}", strategy);
                let entry = self.stats.entry(key).or_insert_with(|| PatternStats {
                    category: IssueCategory::Other,
                    strategy: *strategy,
                    success_count: 0,
                    failure_count: 0,
                });
                if *success {
                    entry.success_count += 1;
                } else {
                    entry.failure_count += 1;
                }
            }
            _ => {}
        }
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn last_applied_seq(&self) -> u64 {
        self.last_applied_seq
    }

    fn set_last_applied_seq(&mut self, seq: u64) {
        self.last_applied_seq = seq;
    }
}

impl PatternStatsProjection {
    pub fn best_strategy_for(&self, category: IssueCategory) -> Option<FixStrategy> {
        self.stats
            .values()
            .filter(|stats| stats.category == category && stats.success_rate() > 0.5)
            .max_by(|a, b| {
                a.success_rate()
                    .partial_cmp(&b.success_rate())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|stats| stats.strategy)
    }

    pub fn strategies_by_success_rate(&self, category: IssueCategory) -> Vec<(FixStrategy, f32)> {
        let mut strategies: Vec<_> = self
            .stats
            .values()
            .filter(|stats| stats.category == category)
            .map(|stats| (stats.strategy, stats.success_rate()))
            .collect();
        strategies.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        strategies
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStatusProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub tasks: HashMap<String, TaskState>,
    #[serde(skip)]
    task_order: VecDeque<String>,
    #[serde(skip)]
    max_tracked_tasks: usize,
}

impl Default for TaskStatusProjection {
    fn default() -> Self {
        use crate::config::StateConfig;
        Self {
            version: 0,
            last_applied_seq: 0,
            tasks: HashMap::new(),
            task_order: VecDeque::new(),
            max_tracked_tasks: StateConfig::default().max_tracked_tasks,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskState {
    pub task_id: String,
    pub description: String,
    pub status: TaskStatus,
    pub files_modified: Vec<String>,
    pub retry_count: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Projection for TaskStatusProjection {
    fn apply(&mut self, event: &DomainEvent) {
        self.version = event.version;

        match &event.payload {
            EventPayload::TaskStarted {
                task_id,
                description,
            } => {
                if self.tasks.len() >= self.max_tracked_tasks
                    && let Some(oldest) = self.task_order.pop_front()
                {
                    self.tasks.remove(&oldest);
                }
                self.task_order.push_back(task_id.clone());
                self.tasks.insert(
                    task_id.clone(),
                    TaskState {
                        task_id: task_id.clone(),
                        description: description.clone(),
                        status: TaskStatus::InProgress,
                        files_modified: vec![],
                        retry_count: 0,
                        started_at: Some(event.timestamp),
                        completed_at: None,
                    },
                );
            }
            EventPayload::TaskCompleted {
                task_id,
                files_modified,
                ..
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Completed;
                    task.files_modified = files_modified.clone();
                    task.completed_at = Some(event.timestamp);
                }
            }
            EventPayload::TaskFailed {
                task_id,
                retry_count,
                ..
            } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Failed;
                    task.retry_count = *retry_count;
                }
            }
            EventPayload::TaskSkipped { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Skipped;
                }
            }
            EventPayload::TaskDeferred { task_id, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = TaskStatus::Deferred;
                }
            }
            EventPayload::TaskStatusChanged { task_id, to, .. } => {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.status = *to;
                }
            }
            _ => {}
        }
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn last_applied_seq(&self) -> u64 {
        self.last_applied_seq
    }

    fn set_last_applied_seq(&mut self, seq: u64) {
        self.last_applied_seq = seq;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionSnapshot<T> {
    pub version: u32,
    pub snapshot_type: String,
    pub data: T,
    pub created_at: DateTime<Utc>,
}

impl<T: Clone> ProjectionSnapshot<T> {
    pub fn new(data: T, version: u32, snapshot_type: impl Into<String>) -> Self {
        Self {
            version,
            snapshot_type: snapshot_type.into(),
            data,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub mission_id: Option<String>,
    pub consensus_reached: bool,
    pub rounds: u32,
    pub respondent_count: usize,
    pub outcome: Option<ConsensusOutcome>,
    pub phase_results: Vec<PhaseResult>,
    pub timestamp: Option<DateTime<Utc>>,
    pub current_round: Option<CurrentRoundState>,
    pub pending_votes: Vec<String>,
    pub active_conflicts: VecDeque<ActiveConflict>,
    pub proposal_hashes: VecDeque<String>,
    pub escalation_level: Option<String>,
    #[serde(skip)]
    max_proposal_history: usize,
    #[serde(skip)]
    max_active_conflicts: usize,
}

impl Default for ConsensusProjection {
    fn default() -> Self {
        use crate::config::StateConfig;
        let config = StateConfig::default();
        Self {
            version: 0,
            last_applied_seq: 0,
            mission_id: None,
            consensus_reached: false,
            rounds: 0,
            respondent_count: 0,
            outcome: None,
            phase_results: Vec::new(),
            timestamp: None,
            current_round: None,
            pending_votes: Vec::new(),
            active_conflicts: VecDeque::new(),
            proposal_hashes: VecDeque::new(),
            escalation_level: None,
            max_proposal_history: config.max_proposal_history,
            max_active_conflicts: config.max_active_conflicts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentRoundState {
    pub round_number: u32,
    pub proposal_hash: String,
    pub votes_received: Vec<ReceivedVote>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceivedVote {
    pub agent_id: String,
    pub decision: VoteDecision,
    pub score: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveConflict {
    pub id: String,
    pub agents: Vec<String>,
    pub severity: Severity,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase: SessionPhase,
    pub task_count: usize,
    pub success_count: usize,
    pub duration_ms: u64,
}

impl Projection for ConsensusProjection {
    fn apply(&mut self, event: &DomainEvent) {
        self.version = event.version;

        if self.mission_id.is_none() {
            self.mission_id = Some(event.aggregate_id.to_string());
        }

        match &event.payload {
            EventPayload::ConsensusRoundStarted {
                round,
                proposal_hash,
                participants,
            } => {
                self.current_round = Some(CurrentRoundState {
                    round_number: *round,
                    proposal_hash: proposal_hash.clone(),
                    votes_received: Vec::new(),
                    started_at: event.timestamp,
                });
                self.pending_votes.clone_from(participants);
                if self.proposal_hashes.len() >= self.max_proposal_history {
                    self.proposal_hashes.pop_front();
                }
                self.proposal_hashes.push_back(proposal_hash.clone());
            }
            EventPayload::ConsensusVoteReceived {
                round,
                agent_id,
                decision,
                score,
            } => {
                if let Some(ref mut current) = self.current_round
                    && current.round_number == *round
                {
                    current.votes_received.push(ReceivedVote {
                        agent_id: agent_id.clone(),
                        decision: *decision,
                        score: *score,
                        timestamp: event.timestamp,
                    });
                }
                self.pending_votes.retain(|id| id != agent_id);
            }
            EventPayload::ConsensusConflictDetected {
                conflict_id,
                agents,
                severity,
                ..
            } => {
                if self.active_conflicts.len() >= self.max_active_conflicts {
                    self.active_conflicts.pop_front();
                }
                self.active_conflicts.push_back(ActiveConflict {
                    id: conflict_id.clone(),
                    agents: agents.clone(),
                    severity: *severity,
                    detected_at: event.timestamp,
                });
            }
            EventPayload::ConsensusConflictResolved { conflict_id, .. } => {
                self.active_conflicts.retain(|c| c.id != *conflict_id);
            }
            EventPayload::ConsensusRoundCompleted { round, .. } => {
                if self.current_round.as_ref().map(|r| r.round_number) == Some(*round) {
                    self.current_round = None;
                }
                self.rounds = *round;
            }
            EventPayload::ConsensusEscalated { level, strategy } => {
                self.escalation_level = Some(format!("{}: {}", level.as_str(), strategy));
            }
            EventPayload::ConsensusCompleted {
                rounds,
                respondent_count,
                outcome,
            } => {
                self.rounds = *rounds;
                self.respondent_count = *respondent_count;
                self.outcome = Some(*outcome);
                self.consensus_reached = *outcome == ConsensusOutcome::Converged;
                self.timestamp = Some(event.timestamp);
                self.current_round = None;
                self.pending_votes.clear();
            }
            EventPayload::MultiAgentPhaseCompleted {
                phase,
                task_count,
                success_count,
                duration_ms,
            } => {
                self.phase_results.push(PhaseResult {
                    phase: phase.clone(),
                    task_count: *task_count,
                    success_count: *success_count,
                    duration_ms: *duration_ms,
                });
            }
            _ => {}
        }
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn last_applied_seq(&self) -> u64 {
        self.last_applied_seq
    }

    fn set_last_applied_seq(&mut self, seq: u64) {
        self.last_applied_seq = seq;
    }
}

impl ConsensusProjection {
    pub fn has_incomplete_round(&self) -> bool {
        self.current_round.is_some()
    }

    pub fn pending_vote_count(&self) -> usize {
        self.pending_votes.len()
    }

    pub fn has_oscillation(&self, proposal_hash: &str) -> bool {
        self.proposal_hashes.contains(&proposal_hash.to_string())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::consensus::conflict::ResolutionStrategy;
    use crate::domain::{EscalationLevel, RoundOutcome};

    #[test]
    fn test_progress_projection() {
        let mut proj = ProgressProjection::default();

        proj.apply(&DomainEvent::mission_created("m-1", "Test"));
        assert!(proj.started_at.is_some());

        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::PlanGenerated {
                task_count: 5,
                phase_count: 2,
            },
        ));
        assert_eq!(proj.total_tasks, 5);

        proj.apply(&DomainEvent::task_started("m-1", "t-1", "Task 1"));
        assert_eq!(proj.current_task, Some("t-1".into()));

        proj.apply(&DomainEvent::task_completed(
            "m-1",
            "t-1",
            vec!["f.rs".into()],
            100,
        ));
        assert_eq!(proj.completed_tasks, 1);
        assert_eq!(proj.completion_percentage(), 20.0);
    }

    #[test]
    fn test_timeline_projection() {
        let mut proj = TimelineProjection::new(100);

        proj.apply(&DomainEvent::mission_created("m-1", "Test mission"));
        proj.apply(&DomainEvent::task_started("m-1", "t-1", "Task 1"));

        assert_eq!(proj.entries.len(), 2);
        assert!(proj.entries[0].summary.contains("Mission created"));
    }

    #[test]
    fn test_pattern_stats_projection() {
        let mut proj = PatternStatsProjection::default();

        proj.apply(&DomainEvent::pattern_learned(
            "m-1",
            "p-1",
            IssueCategory::BuildError,
            FixStrategy::DirectFix,
            true,
        ));
        proj.apply(&DomainEvent::pattern_learned(
            "m-1",
            "p-2",
            IssueCategory::BuildError,
            FixStrategy::DirectFix,
            true,
        ));
        proj.apply(&DomainEvent::pattern_learned(
            "m-1",
            "p-3",
            IssueCategory::BuildError,
            FixStrategy::DirectFix,
            false,
        ));

        let key = format!(
            "{:?}:{:?}",
            IssueCategory::BuildError,
            FixStrategy::DirectFix
        );
        let stats = proj.stats.get(&key).unwrap();
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.failure_count, 1);
        assert!((stats.success_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_progress_projection_handles_key_events() {
        let mut proj = ProgressProjection::default();

        // Mission lifecycle
        proj.apply(&DomainEvent::mission_created("m-1", "Test mission"));
        assert!(proj.started_at.is_some());

        proj.apply(&DomainEvent::state_changed(
            "m-1",
            MissionState::Pending,
            MissionState::Planning,
            "Starting",
        ));
        assert_eq!(proj.state, MissionState::Planning);

        // Task lifecycle
        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::PlanGenerated {
                task_count: 10,
                phase_count: 3,
            },
        ));
        assert_eq!(proj.total_tasks, 10);

        proj.apply(&DomainEvent::task_started("m-1", "t-1", "Task 1"));
        assert_eq!(proj.current_task, Some("t-1".into()));

        proj.apply(&DomainEvent::task_completed("m-1", "t-1", vec!["a.rs".into()], 50));
        assert_eq!(proj.completed_tasks, 1);
        assert!(proj.current_task.is_none());

        proj.apply(&DomainEvent::task_failed("m-1", "t-2", "compile error", 0));
        assert_eq!(proj.failed_tasks, 1);

        proj.apply(&DomainEvent::task_deferred("m-1", "t-3", "conflict", "coder-0"));
        assert_eq!(proj.deferred_tasks, 1);

        // Verification
        proj.apply(&DomainEvent::verification_round("m-1", 1, false, 3, 500));
        assert_eq!(proj.current_verification_round, 1);

        proj.apply(&DomainEvent::verification_round("m-1", 2, true, 0, 400));
        assert_eq!(proj.current_verification_round, 2);

        // Issue tracking
        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::IssueDetected {
                issue_id: "i-1".into(),
                category: IssueCategory::BuildError,
                severity: Severity::Error,
                message: "type mismatch".into(),
                file: Some("src/lib.rs".into()),
                line: Some(42),
            },
        ));
        assert_eq!(proj.total_issues_detected, 1);

        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::IssueResolved {
                issue_id: "i-1".into(),
                fix_strategy: FixStrategy::DirectFix,
            },
        ));
        assert_eq!(proj.total_issues_resolved, 1);

        // Completion percentage
        assert!((proj.completion_percentage() - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_timeline_projection_handles_key_events() {
        let mut proj = TimelineProjection::new(100);

        let events = vec![
            DomainEvent::mission_created("m-1", "Build feature"),
            DomainEvent::state_changed("m-1", MissionState::Pending, MissionState::Planning, "go"),
            DomainEvent::task_started("m-1", "t-1", "Implement auth"),
            DomainEvent::task_completed("m-1", "t-1", vec!["auth.rs".into()], 200),
            DomainEvent::task_failed("m-1", "t-2", "error", 1),
            DomainEvent::verification_round("m-1", 1, true, 0, 100),
            DomainEvent::convergence_achieved("m-1", 3, 2),
            DomainEvent::fix_attempted("m-1", "i-1", FixStrategy::DirectFix, true),
            DomainEvent::consensus_completed("m-1", 2, 4, ConsensusOutcome::Converged),
            DomainEvent::multi_agent_phase_completed(
                "m-1",
                SessionPhase::Planning,
                5,
                4,
                1000,
            ),
        ];

        for event in &events {
            proj.apply(event);
        }

        assert_eq!(proj.entries.len(), events.len());

        // Every explicitly-handled event should produce a meaningful summary (not just event_type)
        assert!(proj.entries[0].summary.contains("Mission created"));
        assert!(proj.entries[1].summary.contains("Planning"));
        assert!(proj.entries[2].summary.contains("started"));
        assert!(proj.entries[3].summary.contains("completed"));
        assert!(proj.entries[4].summary.contains("failed"));
        assert!(proj.entries[5].summary.contains("Verification"));
        assert!(proj.entries[6].summary.contains("Converged"));
        assert!(proj.entries[7].summary.contains("Fix"));
        assert!(proj.entries[8].summary.contains("Consensus"));
        assert!(proj.entries[9].summary.contains("Phase"));

        // Unhandled event falls back to event_type string
        proj.apply(&DomainEvent::checkpoint_created("m-1", "cp-1", 3));
        let last = proj.entries.back().unwrap();
        assert_eq!(last.summary, "checkpoint_created");
    }

    #[test]
    fn test_consensus_projection_handles_full_cycle() {
        let mut proj = ConsensusProjection::default();
        let participants = vec!["agent-1".into(), "agent-2".into(), "agent-3".into()];

        // Round 1: start
        proj.apply(&DomainEvent::consensus_round_started(
            "m-1",
            1,
            "hash-abc",
            participants.clone(),
        ));
        assert!(proj.has_incomplete_round());
        assert_eq!(proj.pending_vote_count(), 3);
        assert!(proj.has_oscillation("hash-abc"));

        // Votes
        proj.apply(&DomainEvent::consensus_vote_received(
            "m-1",
            1,
            "agent-1",
            VoteDecision::Approve,
            0.9,
        ));
        assert_eq!(proj.pending_vote_count(), 2);
        let round = proj.current_round.as_ref().unwrap();
        assert_eq!(round.votes_received.len(), 1);

        proj.apply(&DomainEvent::consensus_vote_received(
            "m-1",
            1,
            "agent-2",
            VoteDecision::Approve,
            0.85,
        ));
        proj.apply(&DomainEvent::consensus_vote_received(
            "m-1",
            1,
            "agent-3",
            VoteDecision::Reject,
            0.3,
        ));
        assert_eq!(proj.pending_vote_count(), 0);

        // Conflict detected and resolved
        proj.apply(&DomainEvent::consensus_conflict_detected(
            "m-1",
            1,
            "c-1",
            vec!["agent-1".into(), "agent-3".into()],
            Severity::Warning,
        ));
        assert_eq!(proj.active_conflicts.len(), 1);

        proj.apply(&DomainEvent::consensus_conflict_resolved(
            "m-1",
            "c-1",
            ResolutionStrategy::HighestScore,
        ));
        assert!(proj.active_conflicts.is_empty());

        // Round completed
        proj.apply(&DomainEvent::consensus_round_completed(
            "m-1",
            1,
            RoundOutcome::Approved,
            0.67,
        ));
        assert!(!proj.has_incomplete_round());
        assert_eq!(proj.rounds, 1);

        // Escalation
        proj.apply(&DomainEvent::consensus_escalated(
            "m-1",
            EscalationLevel::ArchitecturalMediation,
            "Design conflict",
        ));
        assert!(proj.escalation_level.is_some());

        // Final completion
        proj.apply(&DomainEvent::consensus_completed(
            "m-1",
            2,
            3,
            ConsensusOutcome::Converged,
        ));
        assert!(proj.consensus_reached);
        assert_eq!(proj.rounds, 2);
        assert_eq!(proj.respondent_count, 3);
        assert_eq!(proj.outcome, Some(ConsensusOutcome::Converged));
        assert!(proj.pending_votes.is_empty());

        // Phase result tracking
        proj.apply(&DomainEvent::multi_agent_phase_completed(
            "m-1",
            SessionPhase::Implementation,
            8,
            7,
            5000,
        ));
        assert_eq!(proj.phase_results.len(), 1);
        assert_eq!(proj.phase_results[0].success_count, 7);
    }

    #[test]
    fn test_task_projection_handles_task_lifecycle() {
        let mut proj = TaskStatusProjection::default();

        // Start task
        proj.apply(&DomainEvent::task_started("m-1", "t-1", "Implement auth"));
        let task = proj.tasks.get("t-1").unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(task.started_at.is_some());
        assert!(task.completed_at.is_none());

        // Complete task
        proj.apply(&DomainEvent::task_completed(
            "m-1",
            "t-1",
            vec!["auth.rs".into(), "lib.rs".into()],
            250,
        ));
        let task = proj.tasks.get("t-1").unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.files_modified.len(), 2);
        assert!(task.completed_at.is_some());

        // Failed task
        proj.apply(&DomainEvent::task_started("m-1", "t-2", "Fix bug"));
        proj.apply(&DomainEvent::task_failed("m-1", "t-2", "compile error", 2));
        let task = proj.tasks.get("t-2").unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.retry_count, 2);

        // Deferred task
        proj.apply(&DomainEvent::task_started("m-1", "t-3", "Update config"));
        proj.apply(&DomainEvent::task_deferred("m-1", "t-3", "file conflict", "coder-0"));
        assert_eq!(proj.tasks.get("t-3").unwrap().status, TaskStatus::Deferred);

        // Skipped task
        proj.apply(&DomainEvent::task_started("m-1", "t-4", "Optional step"));
        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::TaskSkipped {
                task_id: "t-4".into(),
                reason: "dependency failed".into(),
            },
        ));
        assert_eq!(proj.tasks.get("t-4").unwrap().status, TaskStatus::Skipped);

        // Direct status change
        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::TaskStatusChanged {
                task_id: "t-3".into(),
                from: TaskStatus::Deferred,
                to: TaskStatus::InProgress,
            },
        ));
        assert_eq!(proj.tasks.get("t-3").unwrap().status, TaskStatus::InProgress);
    }

    #[test]
    fn test_pattern_stats_fix_attempted_and_pattern_applied() {
        let mut proj = PatternStatsProjection::default();

        // FixAttempted events contribute to stats
        proj.apply(&DomainEvent::fix_attempted("m-1", "i-1", FixStrategy::DirectFix, true));
        proj.apply(&DomainEvent::fix_attempted("m-1", "i-2", FixStrategy::DirectFix, true));
        proj.apply(&DomainEvent::fix_attempted("m-1", "i-3", FixStrategy::DirectFix, false));
        proj.apply(&DomainEvent::fix_attempted("m-1", "i-4", FixStrategy::Rollback, true));

        let key_direct = format!("Other:{:?}", FixStrategy::DirectFix);
        let stats = proj.stats.get(&key_direct).unwrap();
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.failure_count, 1);
        assert_eq!(stats.category, IssueCategory::Other);

        let key_rollback = format!("Other:{:?}", FixStrategy::Rollback);
        let stats = proj.stats.get(&key_rollback).unwrap();
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.failure_count, 0);

        // PatternApplied increments counter
        assert_eq!(proj.total_patterns_applied, 0);
        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::PatternApplied {
                pattern_id: "p-1".into(),
                issue_id: "i-5".into(),
                confidence: 0.92,
            },
        ));
        assert_eq!(proj.total_patterns_applied, 1);

        proj.apply(&DomainEvent::new(
            "m-1",
            EventPayload::PatternApplied {
                pattern_id: "p-2".into(),
                issue_id: "i-6".into(),
                confidence: 0.75,
            },
        ));
        assert_eq!(proj.total_patterns_applied, 2);
    }

    #[test]
    fn test_task_projection_eviction() {
        let mut proj = TaskStatusProjection {
            max_tracked_tasks: 3,
            ..Default::default()
        };

        // Fill to capacity
        proj.apply(&DomainEvent::task_started("m-1", "t-1", "Task 1"));
        proj.apply(&DomainEvent::task_started("m-1", "t-2", "Task 2"));
        proj.apply(&DomainEvent::task_started("m-1", "t-3", "Task 3"));
        assert_eq!(proj.tasks.len(), 3);
        assert!(proj.tasks.contains_key("t-1"));

        // Adding a 4th task should evict the oldest (t-1)
        proj.apply(&DomainEvent::task_started("m-1", "t-4", "Task 4"));
        assert_eq!(proj.tasks.len(), 3);
        assert!(!proj.tasks.contains_key("t-1"), "oldest task should be evicted");
        assert!(proj.tasks.contains_key("t-2"));
        assert!(proj.tasks.contains_key("t-3"));
        assert!(proj.tasks.contains_key("t-4"));

        // Adding another should evict t-2
        proj.apply(&DomainEvent::task_started("m-1", "t-5", "Task 5"));
        assert!(!proj.tasks.contains_key("t-2"), "second oldest should be evicted");
        assert!(proj.tasks.contains_key("t-3"));
        assert!(proj.tasks.contains_key("t-5"));
    }
}
