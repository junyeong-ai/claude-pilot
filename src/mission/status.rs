use serde::{Deserialize, Serialize};

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
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Skipped)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::InProgress | Self::Verifying)
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
        };
        write!(f, "{}", s)
    }
}
