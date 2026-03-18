use serde::{Deserialize, Serialize};
use std::fmt;

use crate::agent::multi::consensus::ConsensusTask;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierLevel {
    Module,
    Group,
    Domain,
    Workspace,
    CrossWorkspace,
}

impl TierLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Group => "group",
            Self::Domain => "domain",
            Self::Workspace => "workspace",
            Self::CrossWorkspace => "cross_workspace",
        }
    }

    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Module => Some(Self::Group),
            Self::Group => Some(Self::Domain),
            Self::Domain => Some(Self::Workspace),
            Self::Workspace => Some(Self::CrossWorkspace),
            Self::CrossWorkspace => None,
        }
    }

    pub fn escalation_order() -> &'static [TierLevel] {
        &[
            TierLevel::Module,
            TierLevel::Group,
            TierLevel::Domain,
            TierLevel::Workspace,
            TierLevel::CrossWorkspace,
        ]
    }
}

impl fmt::Display for TierLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusOutcome {
    Converged,
    PartialConvergence,
    Escalated,
    Timeout,
    Failed,
}

impl ConsensusOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Converged => "converged",
            Self::PartialConvergence => "partial",
            Self::Escalated => "escalated",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Converged | Self::PartialConvergence)
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::PartialConvergence)
    }
}

impl fmt::Display for ConsensusOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierResult {
    pub tier_level: TierLevel,
    pub unit_id: String,
    pub converged: bool,
    pub synthesis: String,
    pub respondent_count: usize,
    pub conflicts: Vec<String>,
    pub timed_out: bool,
    pub rounds: usize,
    #[serde(default)]
    pub tasks: Vec<ConsensusTask>,
}

impl TierResult {
    pub fn success(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        synthesis: impl Into<String>,
        respondent_count: usize,
        rounds: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: true,
            synthesis: synthesis.into(),
            respondent_count,
            conflicts: Vec::new(),
            timed_out: false,
            rounds,
            tasks: Vec::new(),
        }
    }

    pub fn failure(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        synthesis: impl Into<String>,
        respondent_count: usize,
        conflicts: Vec<String>,
        rounds: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: false,
            synthesis: synthesis.into(),
            respondent_count,
            conflicts,
            timed_out: false,
            rounds,
            tasks: Vec::new(),
        }
    }

    pub fn timeout(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        respondent_count: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: false,
            synthesis: "Timed out - workspace may be unavailable".to_string(),
            respondent_count,
            conflicts: vec!["timeout".to_string()],
            timed_out: true,
            rounds: 0,
            tasks: Vec::new(),
        }
    }

    pub fn with_tasks(mut self, tasks: Vec<ConsensusTask>) -> Self {
        self.tasks = tasks;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusSessionStatus {
    Running,
    Checkpointed,
    Completed,
    Escalated,
    Failed,
}

impl ConsensusSessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Checkpointed => "checkpointed",
            Self::Completed => "completed",
            Self::Escalated => "escalated",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Escalated | Self::Failed)
    }
}

impl fmt::Display for ConsensusSessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Quorum, VoteDecision};

    #[test]
    fn test_vote_decision() {
        assert!(VoteDecision::Approve.is_positive());
        assert!(VoteDecision::ApproveWithChanges.is_positive());
        assert!(!VoteDecision::Reject.is_positive());
        assert!(!VoteDecision::Abstain.is_positive());
    }

    #[test]
    fn test_tier_level_order() {
        assert!(TierLevel::Module < TierLevel::Group);
        assert!(TierLevel::Group < TierLevel::Domain);
        assert!(TierLevel::Domain < TierLevel::Workspace);
        assert!(TierLevel::Workspace < TierLevel::CrossWorkspace);
    }

    #[test]
    fn test_tier_level_parent() {
        assert_eq!(TierLevel::Module.parent(), Some(TierLevel::Group));
        assert_eq!(TierLevel::Group.parent(), Some(TierLevel::Domain));
        assert_eq!(TierLevel::Domain.parent(), Some(TierLevel::Workspace));
        assert_eq!(
            TierLevel::Workspace.parent(),
            Some(TierLevel::CrossWorkspace)
        );
        assert_eq!(TierLevel::CrossWorkspace.parent(), None);
    }

    #[test]
    fn test_tier_level_escalation() {
        let order = TierLevel::escalation_order();
        assert_eq!(order.len(), 5);
        assert_eq!(order[0], TierLevel::Module);
        assert_eq!(order[4], TierLevel::CrossWorkspace);
    }

    #[test]
    fn test_consensus_outcome() {
        assert_eq!(ConsensusOutcome::Converged.as_str(), "converged");
        assert_eq!(ConsensusOutcome::PartialConvergence.as_str(), "partial");
        assert_eq!(ConsensusOutcome::Escalated.as_str(), "escalated");
        assert_eq!(ConsensusOutcome::Timeout.as_str(), "timeout");
        assert_eq!(ConsensusOutcome::Failed.as_str(), "failed");

        assert!(ConsensusOutcome::Converged.is_success());
        assert!(ConsensusOutcome::PartialConvergence.is_success());
        assert!(!ConsensusOutcome::Failed.is_success());

        assert!(ConsensusOutcome::Converged.is_terminal());
        assert!(!ConsensusOutcome::PartialConvergence.is_terminal());
    }

    #[test]
    fn test_tier_result_constructors() {
        let success = TierResult::success(TierLevel::Module, "unit-1", "Synthesis", 3, 2);
        assert!(success.converged);
        assert!(!success.timed_out);
        assert_eq!(success.respondent_count, 3);
        assert_eq!(success.rounds, 2);

        let failure = TierResult::failure(
            TierLevel::Group,
            "unit-2",
            "Failed",
            2,
            vec!["conflict".to_string()],
            1,
        );
        assert!(!failure.converged);
        assert!(!failure.timed_out);
        assert_eq!(failure.respondent_count, 2);
        assert_eq!(failure.conflicts.len(), 1);
        assert_eq!(failure.rounds, 1);

        let timeout = TierResult::timeout(TierLevel::Domain, "unit-3", 5);
        assert!(!timeout.converged);
        assert!(timeout.timed_out);
        assert_eq!(timeout.respondent_count, 5);
    }

    #[test]
    fn test_session_status() {
        assert_eq!(ConsensusSessionStatus::Running.as_str(), "running");
        assert_eq!(ConsensusSessionStatus::Completed.as_str(), "completed");

        assert!(!ConsensusSessionStatus::Running.is_terminal());
        assert!(!ConsensusSessionStatus::Checkpointed.is_terminal());
        assert!(ConsensusSessionStatus::Completed.is_terminal());
        assert!(ConsensusSessionStatus::Escalated.is_terminal());
        assert!(ConsensusSessionStatus::Failed.is_terminal());
    }

    #[test]
    fn test_quorum() {
        assert_eq!(Quorum::Majority.threshold(5), 3);
        assert_eq!(Quorum::Supermajority.threshold(5), 4);
        assert_eq!(Quorum::Unanimous.threshold(5), 5);

        assert!(Quorum::Majority.is_met(3, 5));
        assert!(!Quorum::Majority.is_met(2, 5));
        assert!(Quorum::Supermajority.is_met(4, 5));
    }
}
