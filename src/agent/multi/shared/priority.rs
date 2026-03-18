use serde::{Deserialize, Serialize};

use crate::mission::MissionPriority;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl TaskPriority {
    pub fn weight(&self) -> f64 {
        match self {
            Self::Low => 0.25,
            Self::Normal => 0.5,
            Self::High => 0.75,
            Self::Critical => 1.0,
        }
    }
}

impl From<MissionPriority> for TaskPriority {
    fn from(mp: MissionPriority) -> Self {
        match mp {
            MissionPriority::P1 => Self::Critical,
            MissionPriority::P2 => Self::High,
            MissionPriority::P3 => Self::Normal,
            MissionPriority::P4 => Self::Low,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Trivial,
    #[default]
    Low,
    Medium,
    High,
    Complex,
}

impl TaskComplexity {
    pub fn weight(&self) -> f64 {
        match self {
            Self::Trivial => 0.2,
            Self::Low => 0.4,
            Self::Medium => 0.6,
            Self::High => 0.8,
            Self::Complex => 1.0,
        }
    }
}
