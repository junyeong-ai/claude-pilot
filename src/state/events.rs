//! Domain events for event sourcing.

use std::fmt;

use chrono::{DateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

use crate::agent::multi::consensus::ConflictSeverity;
use crate::agent::multi::escalation::EscalationLevel;
pub use crate::agent::multi::shared::{RoundOutcome, VoteDecision};
use crate::mission::TaskStatus;
use crate::planning::ComplexityTier;
use crate::state::MissionState;
use crate::verification::{FixStrategy, IssueCategory, IssueSeverity};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(pub(crate) String);

impl EventId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for EventId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for EventId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for EventId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToSql for EventId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Text(self.0.as_bytes())))
    }
}

impl FromSql for EventId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(s) => std::str::from_utf8(s)
                .map(|s| Self(s.to_string()))
                .map_err(|e| FromSqlError::Other(Box::new(e))),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AggregateId(pub(crate) String);

impl AggregateId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for AggregateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AggregateId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AggregateId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for AggregateId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&String> for AggregateId {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl ToSql for AggregateId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Text(self.0.as_bytes())))
    }
}

impl FromSql for AggregateId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Text(s) => std::str::from_utf8(s)
                .map(|s| Self(s.to_string()))
                .map_err(|e| FromSqlError::Other(Box::new(e))),
            _ => Err(FromSqlError::InvalidType),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainEvent {
    pub id: EventId,
    pub aggregate_id: AggregateId,
    pub version: u32,
    pub global_seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: EventPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<EventId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

impl DomainEvent {
    pub fn new(aggregate_id: impl Into<AggregateId>, payload: EventPayload) -> Self {
        Self {
            id: EventId::new(),
            aggregate_id: aggregate_id.into(),
            version: 0,
            global_seq: 0,
            timestamp: Utc::now(),
            payload,
            causation_id: None,
            correlation_id: None,
        }
    }

    pub fn with_causation(mut self, id: impl Into<EventId>) -> Self {
        self.causation_id = Some(id.into());
        self
    }

    pub fn with_correlation(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }

    pub fn mission_created(mission_id: &str, description: &str) -> Self {
        Self::new(
            mission_id,
            EventPayload::MissionCreated {
                description: description.into(),
            },
        )
    }

    pub fn state_changed(
        mission_id: &str,
        from: MissionState,
        to: MissionState,
        reason: &str,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::StateChanged {
                from,
                to,
                reason: reason.into(),
            },
        )
    }

    pub fn task_started(mission_id: &str, task_id: &str, description: &str) -> Self {
        Self::new(
            mission_id,
            EventPayload::TaskStarted {
                task_id: task_id.into(),
                description: description.into(),
            },
        )
    }

    pub fn task_completed(
        mission_id: &str,
        task_id: &str,
        files_modified: Vec<String>,
        duration_ms: u64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::TaskCompleted {
                task_id: task_id.into(),
                files_modified,
                duration_ms,
            },
        )
    }

    pub fn task_failed(mission_id: &str, task_id: &str, error: &str, retry_count: u32) -> Self {
        Self::new(
            mission_id,
            EventPayload::TaskFailed {
                task_id: task_id.into(),
                error: error.into(),
                retry_count,
            },
        )
    }

    pub fn verification_round(
        mission_id: &str,
        round: u32,
        passed: bool,
        issue_count: usize,
        duration_ms: u64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::VerificationRound {
                round,
                passed,
                issue_count,
                duration_ms,
            },
        )
    }

    pub fn fix_attempted(
        mission_id: &str,
        issue_id: &str,
        strategy: FixStrategy,
        success: bool,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::FixAttempted {
                issue_id: issue_id.into(),
                strategy,
                success,
            },
        )
    }

    pub fn pattern_learned(
        mission_id: &str,
        pattern_id: &str,
        category: IssueCategory,
        strategy: FixStrategy,
        success: bool,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::PatternLearned {
                pattern_id: pattern_id.into(),
                category,
                strategy,
                success,
            },
        )
    }

    pub fn checkpoint_created(mission_id: &str, checkpoint_id: &str, task_count: usize) -> Self {
        Self::new(
            mission_id,
            EventPayload::CheckpointCreated {
                checkpoint_id: checkpoint_id.into(),
                task_count,
            },
        )
    }

    pub fn consensus_round_started(
        mission_id: &str,
        round: u32,
        proposal_hash: &str,
        participants: Vec<String>,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusRoundStarted {
                round,
                proposal_hash: proposal_hash.into(),
                participants,
            },
        )
    }

    pub fn consensus_vote_received(
        mission_id: &str,
        round: u32,
        agent_id: &str,
        decision: VoteDecision,
        score: f64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusVoteReceived {
                round,
                agent_id: agent_id.into(),
                decision,
                score,
            },
        )
    }

    pub fn consensus_conflict_detected(
        mission_id: &str,
        round: u32,
        conflict_id: &str,
        agents: Vec<String>,
        severity: ConflictSeverity,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusConflictDetected {
                round,
                conflict_id: conflict_id.into(),
                agents,
                severity,
            },
        )
    }

    pub fn consensus_conflict_resolved(
        mission_id: &str,
        conflict_id: &str,
        strategy: &str,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusConflictResolved {
                conflict_id: conflict_id.into(),
                resolution_strategy: strategy.into(),
            },
        )
    }

    pub fn consensus_round_completed(
        mission_id: &str,
        round: u32,
        outcome: RoundOutcome,
        approval_ratio: f64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusRoundCompleted {
                round,
                outcome,
                approval_ratio,
            },
        )
    }

    pub fn consensus_escalated(mission_id: &str, level: EscalationLevel, strategy: &str) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusEscalated {
                level,
                strategy: strategy.into(),
            },
        )
    }

    pub fn consensus_completed(
        mission_id: &str,
        rounds: u32,
        respondent_count: usize,
        outcome: &str,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusCompleted {
                rounds,
                respondent_count,
                outcome: outcome.into(),
            },
        )
    }

    pub fn multi_agent_phase_completed(
        mission_id: &str,
        phase: &str,
        task_count: usize,
        success_count: usize,
        duration_ms: u64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::MultiAgentPhaseCompleted {
                phase: phase.into(),
                task_count,
                success_count,
                duration_ms,
            },
        )
    }

    pub fn convergence_achieved(mission_id: &str, total_rounds: u32, clean_rounds: u32) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConvergenceAchieved {
                total_rounds,
                clean_rounds,
            },
        )
    }

    // Agent Lifecycle Events

    pub fn agent_spawned(
        mission_id: &str,
        agent_id: &str,
        role: &str,
        persona: Option<&str>,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::AgentSpawned {
                agent_id: agent_id.into(),
                role: role.into(),
                persona: persona.map(String::from),
            },
        )
    }

    pub fn agent_terminated(
        mission_id: &str,
        agent_id: &str,
        reason: &str,
        tasks_completed: u32,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::AgentTerminated {
                agent_id: agent_id.into(),
                reason: reason.into(),
                tasks_completed,
            },
        )
    }

    pub fn agent_status_changed(
        mission_id: &str,
        agent_id: &str,
        previous_status: &str,
        new_status: &str,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::AgentStatusChanged {
                agent_id: agent_id.into(),
                previous_status: previous_status.into(),
                new_status: new_status.into(),
            },
        )
    }

    // Context Injection Events

    pub fn rules_injected(
        mission_id: &str,
        agent_id: &str,
        task_id: &str,
        rule_count: usize,
        rule_categories: Vec<String>,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::RulesInjected {
                agent_id: agent_id.into(),
                task_id: task_id.into(),
                rule_count,
                rule_categories,
            },
        )
    }

    pub fn skill_activated(
        mission_id: &str,
        agent_id: &str,
        task_id: &str,
        skill_type: &str,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::SkillActivated {
                agent_id: agent_id.into(),
                task_id: task_id.into(),
                skill_type: skill_type.into(),
            },
        )
    }

    pub fn persona_loaded(mission_id: &str, agent_id: &str, persona_name: &str) -> Self {
        Self::new(
            mission_id,
            EventPayload::PersonaLoaded {
                agent_id: agent_id.into(),
                persona_name: persona_name.into(),
            },
        )
    }

    // Enhanced Consensus Events

    pub fn consensus_proposal_submitted(
        mission_id: &str,
        round: u32,
        proposer_agent: &str,
        proposal_hash: &str,
        affected_modules: Vec<String>,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusProposalSubmitted {
                round,
                proposer_agent: proposer_agent.into(),
                proposal_hash: proposal_hash.into(),
                affected_modules,
            },
        )
    }

    pub fn consensus_module_vote(
        mission_id: &str,
        round: u32,
        agent_id: &str,
        module: &str,
        decision: &str,
        confidence: f64,
    ) -> Self {
        Self::new(
            mission_id,
            EventPayload::ConsensusModuleVote {
                round,
                agent_id: agent_id.into(),
                module: module.into(),
                decision: decision.into(),
                confidence,
            },
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    MissionCreated {
        description: String,
    },
    StateChanged {
        from: MissionState,
        to: MissionState,
        reason: String,
    },
    MissionEscalated {
        reason: String,
        context: Option<String>,
    },
    MissionResumed {
        human_response: Option<String>,
    },

    EvidenceGatheringStarted {
        strategy: String,
    },
    EvidenceFileDiscovered {
        file_path: String,
        relevance_score: f32,
    },
    EvidenceGatheringCompleted {
        file_count: usize,
        pattern_count: usize,
        quality_score: f32,
        confidence: f32,
    },

    ComplexityAssessed {
        tier: ComplexityTier,
        score: f32,
        confidence: f32,
        file_count: usize,
    },
    PlanGenerated {
        task_count: usize,
        phase_count: usize,
    },
    PlanValidated {
        passed: bool,
        issues: Vec<String>,
    },

    TaskStarted {
        task_id: String,
        description: String,
    },
    TaskCompleted {
        task_id: String,
        files_modified: Vec<String>,
        duration_ms: u64,
    },
    TaskFailed {
        task_id: String,
        error: String,
        retry_count: u32,
    },
    TaskSkipped {
        task_id: String,
        reason: String,
    },
    TaskStatusChanged {
        task_id: String,
        from: TaskStatus,
        to: TaskStatus,
    },

    VerificationRound {
        round: u32,
        passed: bool,
        issue_count: usize,
        duration_ms: u64,
    },
    IssueDetected {
        issue_id: String,
        category: IssueCategory,
        severity: IssueSeverity,
        message: String,
        file: Option<String>,
        line: Option<usize>,
    },
    IssueResolved {
        issue_id: String,
        fix_strategy: FixStrategy,
    },
    ConvergenceAchieved {
        total_rounds: u32,
        clean_rounds: u32,
    },

    FixAttempted {
        issue_id: String,
        strategy: FixStrategy,
        success: bool,
    },
    FixStrategiesExhausted {
        issue_id: String,
        attempts: u32,
    },

    PatternLearned {
        pattern_id: String,
        category: IssueCategory,
        strategy: FixStrategy,
        success: bool,
    },
    PatternApplied {
        pattern_id: String,
        issue_id: String,
        confidence: f32,
    },
    PatternEvolved {
        pattern_id: String,
        old_confidence: f32,
        new_confidence: f32,
    },

    CheckpointCreated {
        checkpoint_id: String,
        task_count: usize,
    },
    // Note: Session recovery events (SessionRecoveryStarted/Completed/Failed)
    // are used for hierarchical consensus recovery. The old RecoveryStarted
    // variant was removed as it was unused.
    DriftDetected {
        task_id: String,
        file_path: String,
        drift_type: String,
        severity: String,
    },
    ContextSwitch {
        task_id: String,
        from_module: Option<String>,
        to_module: Option<String>,
        switch_count: u32,
    },

    ConsensusRoundStarted {
        round: u32,
        proposal_hash: String,
        participants: Vec<String>,
    },
    ConsensusVoteReceived {
        round: u32,
        agent_id: String,
        decision: VoteDecision,
        score: f64,
    },
    ConsensusConflictDetected {
        round: u32,
        conflict_id: String,
        agents: Vec<String>,
        severity: ConflictSeverity,
    },
    ConsensusConflictResolved {
        conflict_id: String,
        resolution_strategy: String,
    },
    ConsensusRoundCompleted {
        round: u32,
        outcome: RoundOutcome,
        approval_ratio: f64,
    },
    ConsensusOscillationDetected {
        proposal_hash: String,
        cycle_length: usize,
    },
    ConsensusEscalated {
        level: EscalationLevel,
        strategy: String,
    },
    ConsensusCompleted {
        rounds: u32,
        respondent_count: usize,
        outcome: String,
    },
    MultiAgentPhaseCompleted {
        phase: String,
        task_count: usize,
        success_count: usize,
        duration_ms: u64,
    },

    AgentMessageSent {
        from_agent: String,
        to_agent: String,
        message_type: String,
        correlation_id: String,
    },
    AgentMessageReceived {
        from_agent: String,
        to_agent: String,
        message_type: String,
        correlation_id: String,
    },
    AgentTaskAssigned {
        agent_id: String,
        task_id: String,
        role: String,
    },
    AgentTaskCompleted {
        agent_id: String,
        task_id: String,
        success: bool,
        duration_ms: u64,
    },

    // Agent Lifecycle Events
    AgentSpawned {
        agent_id: String,
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        persona: Option<String>,
    },
    AgentTerminated {
        agent_id: String,
        reason: String,
        tasks_completed: u32,
    },
    AgentStatusChanged {
        agent_id: String,
        previous_status: String,
        new_status: String,
    },

    // Context Injection Events
    RulesInjected {
        agent_id: String,
        task_id: String,
        rule_count: usize,
        rule_categories: Vec<String>,
    },
    SkillActivated {
        agent_id: String,
        task_id: String,
        skill_type: String,
    },
    PersonaLoaded {
        agent_id: String,
        persona_name: String,
    },

    // Enhanced Consensus Events
    ConsensusProposalSubmitted {
        round: u32,
        proposer_agent: String,
        proposal_hash: String,
        affected_modules: Vec<String>,
    },
    ConsensusModuleVote {
        round: u32,
        agent_id: String,
        module: String,
        decision: String,
        confidence: f64,
    },

    // Hierarchical Consensus Events
    HierarchicalConsensusStarted {
        session_id: String,
        mission_id: String,
        strategy: String, // "direct", "flat", "hierarchical"
        participant_count: usize,
        tier_count: usize,
    },
    TierConsensusStarted {
        session_id: String,
        tier_level: String, // "module", "group", "domain", "workspace"
        unit_id: String,
        participants: Vec<String>,
    },
    TierConsensusCompleted {
        session_id: String,
        tier_level: String,
        unit_id: String,
        synthesis_hash: String,
        respondent_count: usize,
        converged: bool,
    },
    ConsensusCheckpointCreated {
        session_id: String,
        checkpoint_id: String,
        round: usize,
        tier_level: String,
        state_hash: String,
    },
    SessionRecoveryStarted {
        session_id: String,
        checkpoint_id: String,
        recovery_round: usize,
    },
    SessionRecoveryCompleted {
        session_id: String,
        checkpoint_id: String,
        outcome: String,
        recovered_rounds: usize,
        duration_ms: u64,
    },
    SessionRecoveryFailed {
        session_id: Option<String>,
        checkpoint_id: String,
        reason: String,
    },
    HierarchicalConsensusCompleted {
        session_id: String,
        outcome: String, // "converged", "partial", "escalated", "timeout"
        total_tiers: usize,
        total_rounds: usize,
        duration_ms: u64,
    },
}

impl EventPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::MissionCreated { .. } => "mission_created",
            Self::StateChanged { .. } => "state_changed",
            Self::MissionEscalated { .. } => "mission_escalated",
            Self::MissionResumed { .. } => "mission_resumed",
            Self::EvidenceGatheringStarted { .. } => "evidence_gathering_started",
            Self::EvidenceFileDiscovered { .. } => "evidence_file_discovered",
            Self::EvidenceGatheringCompleted { .. } => "evidence_gathering_completed",
            Self::ComplexityAssessed { .. } => "complexity_assessed",
            Self::PlanGenerated { .. } => "plan_generated",
            Self::PlanValidated { .. } => "plan_validated",
            Self::TaskStarted { .. } => "task_started",
            Self::TaskCompleted { .. } => "task_completed",
            Self::TaskFailed { .. } => "task_failed",
            Self::TaskSkipped { .. } => "task_skipped",
            Self::TaskStatusChanged { .. } => "task_status_changed",
            Self::VerificationRound { .. } => "verification_round",
            Self::IssueDetected { .. } => "issue_detected",
            Self::IssueResolved { .. } => "issue_resolved",
            Self::ConvergenceAchieved { .. } => "convergence_achieved",
            Self::FixAttempted { .. } => "fix_attempted",
            Self::FixStrategiesExhausted { .. } => "fix_strategies_exhausted",
            Self::PatternLearned { .. } => "pattern_learned",
            Self::PatternApplied { .. } => "pattern_applied",
            Self::PatternEvolved { .. } => "pattern_evolved",
            Self::CheckpointCreated { .. } => "checkpoint_created",
            Self::DriftDetected { .. } => "drift_detected",
            Self::ContextSwitch { .. } => "context_switch",
            Self::ConsensusRoundStarted { .. } => "consensus_round_started",
            Self::ConsensusVoteReceived { .. } => "consensus_vote_received",
            Self::ConsensusConflictDetected { .. } => "consensus_conflict_detected",
            Self::ConsensusConflictResolved { .. } => "consensus_conflict_resolved",
            Self::ConsensusRoundCompleted { .. } => "consensus_round_completed",
            Self::ConsensusOscillationDetected { .. } => "consensus_oscillation_detected",
            Self::ConsensusEscalated { .. } => "consensus_escalated",
            Self::ConsensusCompleted { .. } => "consensus_completed",
            Self::MultiAgentPhaseCompleted { .. } => "multi_agent_phase_completed",
            Self::AgentMessageSent { .. } => "agent_message_sent",
            Self::AgentMessageReceived { .. } => "agent_message_received",
            Self::AgentTaskAssigned { .. } => "agent_task_assigned",
            Self::AgentTaskCompleted { .. } => "agent_task_completed",
            Self::AgentSpawned { .. } => "agent_spawned",
            Self::AgentTerminated { .. } => "agent_terminated",
            Self::AgentStatusChanged { .. } => "agent_status_changed",
            Self::RulesInjected { .. } => "rules_injected",
            Self::SkillActivated { .. } => "skill_activated",
            Self::PersonaLoaded { .. } => "persona_loaded",
            Self::ConsensusProposalSubmitted { .. } => "consensus_proposal_submitted",
            Self::ConsensusModuleVote { .. } => "consensus_module_vote",
            Self::HierarchicalConsensusStarted { .. } => "hierarchical_consensus_started",
            Self::TierConsensusStarted { .. } => "tier_consensus_started",
            Self::TierConsensusCompleted { .. } => "tier_consensus_completed",
            Self::ConsensusCheckpointCreated { .. } => "consensus_checkpoint_created",
            Self::SessionRecoveryStarted { .. } => "session_recovery_started",
            Self::SessionRecoveryCompleted { .. } => "session_recovery_completed",
            Self::SessionRecoveryFailed { .. } => "session_recovery_failed",
            Self::HierarchicalConsensusCompleted { .. } => "hierarchical_consensus_completed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::StateChanged {
                to: MissionState::Completed | MissionState::Failed | MissionState::Cancelled,
                ..
            }
        )
    }

    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::TaskStarted { task_id, .. }
            | Self::TaskCompleted { task_id, .. }
            | Self::TaskFailed { task_id, .. }
            | Self::TaskSkipped { task_id, .. }
            | Self::TaskStatusChanged { task_id, .. }
            | Self::DriftDetected { task_id, .. }
            | Self::ContextSwitch { task_id, .. }
            | Self::AgentTaskAssigned { task_id, .. }
            | Self::AgentTaskCompleted { task_id, .. }
            | Self::RulesInjected { task_id, .. }
            | Self::SkillActivated { task_id, .. } => Some(task_id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub event_types: Option<Vec<&'static str>>,
    pub from_time: Option<DateTime<Utc>>,
    pub to_time: Option<DateTime<Utc>>,
    pub task_id: Option<String>,
    pub from_global_seq: Option<u64>,
    pub limit: Option<usize>,
}

impl EventFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_types(mut self, types: Vec<&'static str>) -> Self {
        self.event_types = Some(types);
        self
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn since(mut self, time: DateTime<Utc>) -> Self {
        self.from_time = Some(time);
        self
    }

    pub fn until(mut self, time: DateTime<Utc>) -> Self {
        self.to_time = Some(time);
        self
    }

    pub fn from_global_seq(mut self, seq: u64) -> Self {
        self.from_global_seq = Some(seq);
        self
    }

    pub fn matches(&self, event: &DomainEvent) -> bool {
        if let Some(ref types) = self.event_types
            && !types.contains(&event.event_type())
        {
            return false;
        }
        if let Some(ref from) = self.from_time
            && event.timestamp < *from
        {
            return false;
        }
        if let Some(ref to) = self.to_time
            && event.timestamp > *to
        {
            return false;
        }
        if let Some(ref task_id) = self.task_id
            && event.payload.task_id() != Some(task_id.as_str())
        {
            return false;
        }
        if let Some(from_seq) = self.from_global_seq
            && event.global_seq < from_seq
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = DomainEvent::mission_created("mission-1", "Test mission");
        assert_eq!(event.aggregate_id.as_str(), "mission-1");
        assert_eq!(event.event_type(), "mission_created");
    }

    #[test]
    fn test_event_filter() {
        let event = DomainEvent::task_started("mission-1", "task-1", "Test task");
        let filter = EventFilter::new()
            .with_types(vec!["task_started"])
            .with_task("task-1");
        assert!(filter.matches(&event));
    }

    #[test]
    fn test_agent_spawned_event() {
        let event = DomainEvent::agent_spawned("m-1", "coder-0", "coder", Some("default"));
        assert_eq!(event.event_type(), "agent_spawned");
        if let EventPayload::AgentSpawned {
            agent_id,
            role,
            persona,
        } = &event.payload
        {
            assert_eq!(agent_id, "coder-0");
            assert_eq!(role, "coder");
            assert_eq!(persona, &Some("default".to_string()));
        } else {
            panic!("wrong payload type");
        }
    }

    #[test]
    fn test_agent_terminated_event() {
        let event = DomainEvent::agent_terminated("m-1", "coder-0", "shutdown", 5);
        assert_eq!(event.event_type(), "agent_terminated");
    }

    #[test]
    fn test_rules_injected_event() {
        let event = DomainEvent::rules_injected(
            "m-1",
            "coder-0",
            "t-1",
            3,
            vec!["tech".into(), "domain".into()],
        );
        assert_eq!(event.event_type(), "rules_injected");
        assert_eq!(event.payload.task_id(), Some("t-1"));
    }

    #[test]
    fn test_skill_activated_event() {
        let event = DomainEvent::skill_activated("m-1", "coder-0", "t-1", "implement");
        assert_eq!(event.event_type(), "skill_activated");
        assert_eq!(event.payload.task_id(), Some("t-1"));
    }

    #[test]
    fn test_persona_loaded_event() {
        let event = DomainEvent::persona_loaded("m-1", "reviewer-0", "code-reviewer");
        assert_eq!(event.event_type(), "persona_loaded");
    }

    #[test]
    fn test_consensus_proposal_submitted_event() {
        let event = DomainEvent::consensus_proposal_submitted(
            "m-1",
            1,
            "architect-0",
            "abc123",
            vec!["auth".into()],
        );
        assert_eq!(event.event_type(), "consensus_proposal_submitted");
    }

    #[test]
    fn test_consensus_module_vote_event() {
        let event =
            DomainEvent::consensus_module_vote("m-1", 1, "domain-auth-0", "auth", "approve", 0.85);
        assert_eq!(event.event_type(), "consensus_module_vote");
    }

    #[test]
    fn test_all_new_events_serialize() {
        let events = vec![
            DomainEvent::agent_spawned("m-1", "a-0", "coder", None),
            DomainEvent::agent_terminated("m-1", "a-0", "done", 3),
            DomainEvent::agent_status_changed("m-1", "a-0", "idle", "busy"),
            DomainEvent::rules_injected("m-1", "a-0", "t-1", 2, vec![]),
            DomainEvent::skill_activated("m-1", "a-0", "t-1", "plan"),
            DomainEvent::persona_loaded("m-1", "a-0", "reviewer"),
            DomainEvent::consensus_proposal_submitted("m-1", 1, "a-0", "hash", vec![]),
            DomainEvent::consensus_module_vote("m-1", 1, "a-0", "auth", "approve", 0.9),
        ];

        for event in events {
            let json = serde_json::to_string(&event).expect("serialization failed");
            let _: DomainEvent = serde_json::from_str(&json).expect("deserialization failed");
        }
    }
}
