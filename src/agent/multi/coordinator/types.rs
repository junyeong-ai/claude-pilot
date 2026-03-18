//! Internal type definitions for the Coordinator module.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::super::consensus::{Conflict, ConsensusTask, TaskComplexity};
use super::super::escalation::EscalationOption;
use super::super::traits::{AgentTaskResult, AgentExecutionMetrics};

/// Checkpoint information for session recovery.
#[derive(Debug, Clone)]
pub(super) struct CheckpointInfo {
    pub(super) id: String,
    pub(super) session_id: String,
    pub(super) round: usize,
}

pub(super) enum HealthAdaptation {
    Continue,
    Throttle { delay_ms: u64 },
    ForceSequential,
    Escalate { reason: String },
}

/// Complexity distribution metrics for mission tasks.
#[derive(Debug, Clone, Default)]
pub struct ComplexityMetrics {
    /// Number of tasks by complexity level.
    pub by_level: HashMap<TaskComplexity, usize>,
    /// Total weighted complexity score.
    pub total_score: f64,
    /// Completed weighted complexity score.
    pub completed_score: f64,
    /// Completion percentage by complexity weight.
    pub completion_pct: f64,
}

impl ComplexityMetrics {
    pub(super) fn from_tasks(tasks: &[ConsensusTask], completed_ids: &[String]) -> Self {
        let mut by_level: HashMap<TaskComplexity, usize> = HashMap::new();
        let mut total_score = 0.0;
        let mut completed_score = 0.0;

        for task in tasks {
            *by_level.entry(task.estimated_complexity).or_default() += 1;
            let weight = task.estimated_complexity.weight();
            total_score += weight;
            if completed_ids.contains(&task.id) {
                completed_score += weight;
            }
        }

        let completion_pct = if total_score > 0.0 {
            (completed_score / total_score) * 100.0
        } else {
            100.0
        };

        Self {
            by_level,
            total_score,
            completed_score,
            completion_pct,
        }
    }
}

/// Request for human decision when escalation is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanDecisionRequest {
    /// The question to present to the user.
    pub question: String,
    /// Available options for the user to choose from.
    pub options: Vec<EscalationOption>,
    /// Summary of the conflict that led to escalation.
    pub conflict_summary: String,
    /// Files affected by the conflict.
    pub affected_files: Vec<String>,
    /// Positions of different agents on the issue.
    pub agent_positions: HashMap<String, String>,
    /// Resolutions that were attempted before escalating.
    pub attempted_resolutions: Vec<String>,
}

#[derive(Debug)]
pub struct CoordinatorMissionReport {
    pub success: bool,
    pub results: Vec<AgentTaskResult>,
    pub summary: String,
    pub metrics: AgentExecutionMetrics,
    pub conflicts: Vec<Conflict>,
    /// Complexity distribution metrics for the mission.
    pub complexity_metrics: ComplexityMetrics,
    /// Human decision request when escalation is required.
    /// If Some, the mission cannot proceed without user input.
    pub needs_human_decision: Option<HumanDecisionRequest>,
}
