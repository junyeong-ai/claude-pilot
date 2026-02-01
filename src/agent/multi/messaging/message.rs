//! Message types for inter-agent communication.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::multi::consensus::ConflictSeverity;
use crate::agent::multi::escalation::EscalationLevel;
use crate::agent::multi::reviewer::ReviewIssue;
use crate::agent::multi::shared::{EventContext, ToEvent, VoteDecision};
use crate::agent::multi::traits::{AgentTask, AgentTaskResult};
use crate::state::{DomainEvent, EventPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    TaskAssignment,
    TaskResult,
    ConflictAlert,
    ConsensusRequest,
    ConsensusVote,
    EvidenceShare,
    ReviewFeedback,
    EscalationNotice,
    Broadcast,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessagePayload {
    TaskAssignment {
        task: AgentTask,
    },
    TaskResult {
        result: AgentTaskResult,
    },
    ConflictAlert {
        conflict_id: String,
        severity: ConflictSeverity,
        description: String,
    },
    ConsensusRequest {
        round: u32,
        proposal_hash: String,
        proposal: String,
    },
    ConsensusVote {
        round: u32,
        decision: VoteDecision,
        rationale: String,
    },
    EvidenceShare {
        evidence: Vec<String>,
        quality_score: f64,
    },
    ReviewFeedback {
        issues: Vec<ReviewIssue>,
        verdict: String,
    },
    EscalationNotice {
        level: String,
        reason: String,
    },
    Text {
        content: String,
    },
}

impl MessagePayload {
    pub fn message_type(&self) -> MessageType {
        match self {
            Self::TaskAssignment { .. } => MessageType::TaskAssignment,
            Self::TaskResult { .. } => MessageType::TaskResult,
            Self::ConflictAlert { .. } => MessageType::ConflictAlert,
            Self::ConsensusRequest { .. } => MessageType::ConsensusRequest,
            Self::ConsensusVote { .. } => MessageType::ConsensusVote,
            Self::EvidenceShare { .. } => MessageType::EvidenceShare,
            Self::ReviewFeedback { .. } => MessageType::ReviewFeedback,
            Self::EscalationNotice { .. } => MessageType::EscalationNotice,
            Self::Text { .. } => MessageType::Broadcast,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TaskAssignment { .. } => "task_assignment",
            Self::TaskResult { .. } => "task_result",
            Self::ConflictAlert { .. } => "conflict_alert",
            Self::ConsensusRequest { .. } => "consensus_request",
            Self::ConsensusVote { .. } => "consensus_vote",
            Self::EvidenceShare { .. } => "evidence_share",
            Self::ReviewFeedback { .. } => "review_feedback",
            Self::EscalationNotice { .. } => "escalation_notice",
            Self::Text { .. } => "text",
        }
    }
}

fn parse_escalation_level(level: &str) -> EscalationLevel {
    match level.to_lowercase().as_str() {
        "retry" => EscalationLevel::Retry,
        "scope_reduction" => EscalationLevel::ScopeReduction,
        "architectural_mediation" => EscalationLevel::ArchitecturalMediation,
        "split_and_conquer" => EscalationLevel::SplitAndConquer,
        "human_escalation" | "human" => EscalationLevel::HumanEscalation,
        _ => EscalationLevel::Retry,
    }
}

impl ToEvent for MessagePayload {
    fn to_event(&self, ctx: &EventContext) -> DomainEvent {
        let agent_id = ctx.agent_id.clone().unwrap_or_default();

        let payload = match self {
            MessagePayload::ConsensusVote {
                round, decision, ..
            } => EventPayload::ConsensusVoteReceived {
                round: *round,
                agent_id: agent_id.clone(),
                decision: *decision,
                score: 0.0,
            },
            MessagePayload::TaskAssignment { task } => EventPayload::AgentTaskAssigned {
                agent_id: agent_id.clone(),
                task_id: task.id.clone(),
                role: task
                    .role
                    .as_ref()
                    .map(|r| r.to_string())
                    .unwrap_or_default(),
            },
            MessagePayload::TaskResult { result } => EventPayload::AgentTaskCompleted {
                agent_id: agent_id.clone(),
                task_id: result.task_id.clone(),
                success: result.success,
                duration_ms: 0,
            },
            MessagePayload::ConflictAlert {
                conflict_id,
                severity,
                ..
            } => EventPayload::ConsensusConflictDetected {
                round: 0,
                conflict_id: conflict_id.clone(),
                agents: vec![agent_id.clone()],
                severity: *severity,
            },
            MessagePayload::ConsensusRequest {
                round,
                proposal_hash,
                ..
            } => EventPayload::ConsensusRoundStarted {
                round: *round,
                proposal_hash: proposal_hash.clone(),
                participants: vec![],
            },
            MessagePayload::EscalationNotice { level, reason } => {
                let escalation_level = parse_escalation_level(level);
                EventPayload::ConsensusEscalated {
                    level: escalation_level,
                    strategy: reason.clone(),
                }
            }
            MessagePayload::EvidenceShare { .. }
            | MessagePayload::ReviewFeedback { .. }
            | MessagePayload::Text { .. } => EventPayload::AgentMessageSent {
                from_agent: agent_id,
                to_agent: String::new(),
                message_type: self.type_name().to_string(),
                correlation_id: ctx.correlation_id.clone().unwrap_or_default(),
            },
        };

        let mut event = DomainEvent::new(&ctx.mission_id, payload);
        if let Some(ref corr_id) = ctx.correlation_id {
            event = event.with_correlation(corr_id);
        }
        event
    }
}

#[derive(Debug, Clone)]
pub struct AgentMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub payload: MessagePayload,
    pub timestamp: DateTime<Utc>,
    pub correlation_id: String,
    pub reply_to: Option<String>,
}

impl AgentMessage {
    pub fn new(from: impl Into<String>, to: impl Into<String>, payload: MessagePayload) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            from: from.into(),
            to: to.into(),
            payload,
            timestamp: Utc::now(),
            correlation_id: Uuid::new_v4().to_string(),
            reply_to: None,
        }
    }

    pub fn message_type(&self) -> MessageType {
        self.payload.message_type()
    }

    pub fn broadcast(from: impl Into<String>, payload: MessagePayload) -> Self {
        Self::new(from, "*", payload)
    }

    pub fn with_correlation(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = id.into();
        self
    }

    pub fn reply_to(mut self, id: impl Into<String>) -> Self {
        self.reply_to = Some(id.into());
        self
    }

    pub fn is_broadcast(&self) -> bool {
        self.to == "*"
    }

    pub fn is_for(&self, agent_id: &str) -> bool {
        self.to == agent_id || self.is_broadcast()
    }

    pub fn task_assignment(from: &str, to: &str, task: AgentTask) -> Self {
        Self::new(from, to, MessagePayload::TaskAssignment { task })
    }

    pub fn task_result(from: &str, to: &str, result: AgentTaskResult) -> Self {
        Self::new(from, to, MessagePayload::TaskResult { result })
    }

    pub fn consensus_vote(
        from: &str,
        round: u32,
        decision: VoteDecision,
        rationale: String,
    ) -> Self {
        Self::new(
            from,
            "coordinator",
            MessagePayload::ConsensusVote {
                round,
                decision,
                rationale,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let msg = AgentMessage::new(
            "agent-1",
            "agent-2",
            MessagePayload::Text {
                content: "test".into(),
            },
        );

        assert_eq!(msg.from, "agent-1");
        assert_eq!(msg.to, "agent-2");
        assert!(!msg.is_broadcast());
        assert!(msg.is_for("agent-2"));
        assert!(!msg.is_for("agent-3"));
    }

    #[test]
    fn test_broadcast_message() {
        let msg = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::Text {
                content: "announcement".into(),
            },
        );

        assert!(msg.is_broadcast());
        assert!(msg.is_for("any-agent"));
    }

    #[test]
    fn test_consensus_vote() {
        let msg = AgentMessage::consensus_vote(
            "reviewer-0",
            1,
            VoteDecision::Approve,
            "looks good".into(),
        );

        assert_eq!(msg.from, "reviewer-0");
        assert_eq!(msg.to, "coordinator");
        assert_eq!(msg.message_type(), MessageType::ConsensusVote);
    }

    #[test]
    fn test_escalation_notice_to_event() {
        use crate::agent::multi::shared::EventContext;
        use crate::state::EventPayload;

        let payload = MessagePayload::EscalationNotice {
            level: "human_escalation".to_string(),
            reason: "Conflict requires human decision".to_string(),
        };

        let ctx = EventContext::new("mission-1")
            .with_agent("escalator-0")
            .with_correlation("corr-1");

        let event = payload.to_event(&ctx);

        match event.payload {
            EventPayload::ConsensusEscalated { level, strategy } => {
                assert_eq!(
                    level,
                    crate::agent::multi::escalation::EscalationLevel::HumanEscalation
                );
                assert_eq!(strategy, "Conflict requires human decision");
            }
            _ => panic!("Expected ConsensusEscalated event"),
        }
    }

    #[test]
    fn test_message_payload_type_name() {
        let payloads = vec![
            (
                MessagePayload::TaskAssignment {
                    task: crate::agent::multi::traits::AgentTask::new("t1", "test"),
                },
                "task_assignment",
            ),
            (
                MessagePayload::Text {
                    content: "test".into(),
                },
                "text",
            ),
            (
                MessagePayload::EscalationNotice {
                    level: "retry".into(),
                    reason: "test".into(),
                },
                "escalation_notice",
            ),
        ];

        for (payload, expected_name) in payloads {
            assert_eq!(payload.type_name(), expected_name);
        }
    }
}
