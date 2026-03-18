//! Message types for inter-agent communication.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::multi::reviewer::ReviewIssue;
use crate::domain::{EscalationLevel, Severity, VoteDecision};
use crate::agent::multi::traits::{AgentTask, AgentTaskResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageType {
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

/// Verdict from a reviewer agent's review.
///
/// Separate from `VerificationVerdict` (different domain: reviewer = code quality,
/// verifier = build/test/convergence) and from `Verdict` in architect.rs
/// (different domain: design validation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// No issues found.
    Pass,
    /// Issues found that need attention.
    Issues,
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
        severity: Severity,
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
        verdict: ReviewVerdict,
    },
    EscalationNotice {
        level: EscalationLevel,
        reason: String,
    },
    Broadcast {
        content: String,
    },
}

impl MessagePayload {
    pub fn message_type(&self) -> AgentMessageType {
        match self {
            Self::TaskAssignment { .. } => AgentMessageType::TaskAssignment,
            Self::TaskResult { .. } => AgentMessageType::TaskResult,
            Self::ConflictAlert { .. } => AgentMessageType::ConflictAlert,
            Self::ConsensusRequest { .. } => AgentMessageType::ConsensusRequest,
            Self::ConsensusVote { .. } => AgentMessageType::ConsensusVote,
            Self::EvidenceShare { .. } => AgentMessageType::EvidenceShare,
            Self::ReviewFeedback { .. } => AgentMessageType::ReviewFeedback,
            Self::EscalationNotice { .. } => AgentMessageType::EscalationNotice,
            Self::Broadcast { .. } => AgentMessageType::Broadcast,
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
            Self::Broadcast { .. } => "broadcast",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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

    pub fn message_type(&self) -> AgentMessageType {
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
        coordinator_id: &str,
        round: u32,
        decision: VoteDecision,
        rationale: String,
    ) -> Self {
        Self::new(
            from,
            coordinator_id,
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
            MessagePayload::Broadcast {
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
            MessagePayload::Broadcast {
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
            "group-coordinator-backend",
            1,
            VoteDecision::Approve,
            "looks good".into(),
        );

        assert_eq!(msg.from, "reviewer-0");
        assert_eq!(msg.to, "group-coordinator-backend");
        assert_eq!(msg.message_type(), AgentMessageType::ConsensusVote);
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
                MessagePayload::Broadcast {
                    content: "test".into(),
                },
                "broadcast",
            ),
            (
                MessagePayload::EscalationNotice {
                    level: EscalationLevel::Retry,
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
