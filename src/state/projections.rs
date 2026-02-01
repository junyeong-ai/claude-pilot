//! Read models (projections) built from events with snapshot support.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::events::{DomainEvent, EventPayload};
use crate::agent::multi::consensus::ConflictSeverity;
use crate::mission::TaskStatus;
use crate::state::MissionState;
use crate::verification::{FixStrategy, IssueCategory};

pub trait Projection: Default {
    fn apply(&mut self, event: &DomainEvent);
    fn version(&self) -> u32;
    fn last_applied_seq(&self) -> u64;
    fn set_last_applied_seq(&mut self, seq: u64);

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
                format!("Mission created: {}", truncate(description, 50))
            }
            EventPayload::StateChanged { from, to, reason } => {
                format!("{:?} → {:?}: {}", from, to, truncate(reason, 30))
            }
            EventPayload::TaskStarted {
                task_id,
                description,
            } => {
                format!("Task {} started: {}", task_id, truncate(description, 30))
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
                    truncate(error, 30)
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
                    truncate(issue_id, 10),
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
                    outcome, rounds, respondent_count
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
                    phase, success_count, task_count, duration_ms
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

const MAX_TRACKED_TASKS: usize = 1000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskStatusProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub tasks: HashMap<String, TaskState>,
    #[serde(skip)]
    task_order: VecDeque<String>,
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
                if self.tasks.len() >= MAX_TRACKED_TASKS
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

const MAX_PROPOSAL_HISTORY: usize = 20;
const MAX_ACTIVE_CONFLICTS: usize = 50;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConsensusProjection {
    pub version: u32,
    pub last_applied_seq: u64,
    pub mission_id: Option<String>,
    pub consensus_reached: bool,
    pub rounds: u32,
    pub respondent_count: usize,
    pub outcome: String,
    pub phase_results: Vec<PhaseResult>,
    pub timestamp: Option<DateTime<Utc>>,
    pub current_round: Option<CurrentRoundState>,
    pub pending_votes: Vec<String>,
    pub active_conflicts: VecDeque<ActiveConflict>,
    pub proposal_hashes: VecDeque<String>,
    pub escalation_level: Option<String>,
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
    pub decision: String,
    pub score: f64,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveConflict {
    pub id: String,
    pub agents: Vec<String>,
    pub severity: ConflictSeverity,
    pub detected_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseResult {
    pub phase: String,
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
                if self.proposal_hashes.len() >= MAX_PROPOSAL_HISTORY {
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
                        decision: decision.as_str().into(),
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
                if self.active_conflicts.len() >= MAX_ACTIVE_CONFLICTS {
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
                self.outcome = outcome.clone();
                self.consensus_reached = outcome == "agreed";
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

fn truncate(s: &str, max_len: usize) -> String {
    crate::utils::truncate_at_boundary(s, max_len)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
