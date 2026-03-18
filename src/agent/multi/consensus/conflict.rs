//! Conflict detection and resolution types for consensus.

use serde::{Deserialize, Serialize};

use crate::domain::Severity;

/// Conflict between agent proposals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub agents: Vec<String>,
    pub topic: String,
    pub positions: Vec<String>,
    pub severity: Severity,
    pub resolution: Option<ConflictResolution>,
}

impl Conflict {
    /// Create a conflict from structured parameters.
    pub fn new(
        id: impl Into<String>,
        topic: impl Into<String>,
        severity: Severity,
    ) -> Self {
        Self {
            id: id.into(),
            agents: Vec::new(),
            topic: topic.into(),
            positions: Vec::new(),
            severity,
            resolution: None,
        }
    }

    pub fn with_agents(mut self, agents: Vec<String>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_positions(mut self, positions: Vec<String>) -> Self {
        self.positions = positions;
        self
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

/// How a conflict was resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub strategy: ResolutionStrategy,
    pub chosen_position: String,
    pub rationale: String,
}

/// Strategy for resolving conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStrategy {
    HighestScore,
    ModuleExpertPriority,
    Compromise,
    Escalate,
}

impl ResolutionStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HighestScore => "highest_score",
            Self::ModuleExpertPriority => "module_expert_priority",
            Self::Compromise => "compromise",
            Self::Escalate => "escalate",
        }
    }
}
