use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    #[default]
    Pending,
    Planning,
    Running,
    Paused,
    Verifying,
    /// Waiting for human intervention. Mission is suspended until response received.
    Escalated,
    Completed,
    Failed,
    Cancelled,
}

impl MissionState {
    pub fn allowed_transitions(&self) -> &'static [MissionState] {
        use MissionState::*;
        match self {
            Pending => &[Planning, Running, Failed, Cancelled],
            Planning => &[Running, Failed, Cancelled, Escalated],
            Running => &[Verifying, Failed, Paused, Cancelled, Escalated],
            Verifying => &[Completed, Running, Failed, Escalated],
            Paused => &[Running, Failed, Cancelled],
            Escalated => &[Running, Paused, Failed, Cancelled],
            Completed => &[],
            Failed => &[],
            Cancelled => &[],
        }
    }

    pub fn can_transition_to(&self, target: MissionState) -> bool {
        self.allowed_transitions().contains(&target)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            MissionState::Completed | MissionState::Failed | MissionState::Cancelled
        )
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            MissionState::Planning | MissionState::Running | MissionState::Verifying
        )
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self, MissionState::Paused | MissionState::Escalated)
    }

    pub fn can_resume(&self) -> bool {
        matches!(self, MissionState::Paused | MissionState::Escalated)
    }

    pub fn can_retry(&self) -> bool {
        matches!(self, MissionState::Failed | MissionState::Cancelled)
    }

    pub fn can_cancel(&self) -> bool {
        !self.is_terminal()
    }

    pub fn requires_human_input(&self) -> bool {
        matches!(self, MissionState::Escalated)
    }
}

impl fmt::Display for MissionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Pending => "Pending",
            Self::Planning => "Planning",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Verifying => "Verifying",
            Self::Escalated => "Escalated",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: MissionState,
    pub to: MissionState,
    pub reason: String,
    pub at: DateTime<Utc>,
}

impl StateTransition {
    pub fn new(from: MissionState, to: MissionState, reason: impl Into<String>) -> Self {
        Self {
            from,
            to,
            reason: reason.into(),
            at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(MissionState::Pending.can_transition_to(MissionState::Planning));
        assert!(MissionState::Planning.can_transition_to(MissionState::Running));
        assert!(MissionState::Running.can_transition_to(MissionState::Verifying));
        assert!(MissionState::Verifying.can_transition_to(MissionState::Completed));
        assert!(MissionState::Verifying.can_transition_to(MissionState::Running));
    }

    #[test]
    fn test_escalation_transitions() {
        assert!(MissionState::Running.can_transition_to(MissionState::Escalated));
        assert!(MissionState::Verifying.can_transition_to(MissionState::Escalated));
        assert!(MissionState::Escalated.can_transition_to(MissionState::Running));
        assert!(MissionState::Escalated.can_transition_to(MissionState::Failed));
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(!MissionState::Completed.can_transition_to(MissionState::Running));
        assert!(!MissionState::Failed.can_transition_to(MissionState::Planning));
        assert!(!MissionState::Cancelled.can_transition_to(MissionState::Running));
    }

    #[test]
    fn test_terminal_states() {
        assert!(MissionState::Completed.is_terminal());
        assert!(MissionState::Failed.is_terminal());
        assert!(MissionState::Cancelled.is_terminal());
        assert!(!MissionState::Running.is_terminal());
        assert!(!MissionState::Paused.is_terminal());
        assert!(!MissionState::Escalated.is_terminal());
    }

    #[test]
    fn test_suspended_states() {
        assert!(MissionState::Paused.is_suspended());
        assert!(MissionState::Escalated.is_suspended());
        assert!(!MissionState::Running.is_suspended());
    }

    #[test]
    fn test_can_resume() {
        assert!(MissionState::Paused.can_resume());
        assert!(MissionState::Escalated.can_resume());
        assert!(!MissionState::Running.can_resume());
        assert!(!MissionState::Failed.can_resume());
    }
}
