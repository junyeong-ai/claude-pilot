use serde::{Deserialize, Serialize};

use crate::agent::multi::core::ExecutionOutcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Blocked,
    InProgress,
    Verifying,
    Completed,
    Failed,
    Skipped,
    Deferred,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::InProgress | Self::Verifying)
    }
}

impl From<ExecutionOutcome> for TaskStatus {
    fn from(outcome: ExecutionOutcome) -> Self {
        match outcome {
            ExecutionOutcome::Completed => Self::Completed,
            ExecutionOutcome::Failed => Self::Failed,
            ExecutionOutcome::Deferred => Self::Deferred,
        }
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Pending => "Pending",
            Self::Blocked => "Blocked",
            Self::InProgress => "In Progress",
            Self::Verifying => "Verifying",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Deferred => "Deferred",
        };
        write!(f, "{}", s)
    }
}
