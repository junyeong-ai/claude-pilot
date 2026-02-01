//! Convergence strategies with meta-level orchestration.

use async_trait::async_trait;

use crate::domain::{EscalationContext, ProgressTracker};
use crate::error::Result;
use crate::verification::Issue;

mod meta;
mod partial;

pub use meta::{MetaConvergenceConfig, MetaConvergenceOrchestrator};
pub use partial::{PartialConvergencePolicy, PartialEvaluation};

/// Level of convergence strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum ConvergenceLevel {
    /// Direct fix with standard context.
    DirectFix = 0,
    /// Expanded context for better understanding.
    ContextualFix = 1,
    /// Accept non-critical issues.
    ReducedScope = 2,
    /// Escalate to human.
    HumanEscalation = 3,
}

impl ConvergenceLevel {
    pub fn next(self) -> Option<Self> {
        match self {
            Self::DirectFix => Some(Self::ContextualFix),
            Self::ContextualFix => Some(Self::ReducedScope),
            Self::ReducedScope => Some(Self::HumanEscalation),
            Self::HumanEscalation => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::DirectFix => "DirectFix",
            Self::ContextualFix => "ContextualFix",
            Self::ReducedScope => "ReducedScope",
            Self::HumanEscalation => "HumanEscalation",
        }
    }
}

/// State maintained across convergence attempts.
#[derive(Debug, Clone)]
pub struct MetaConvergenceState {
    pub current_level: ConvergenceLevel,
    pub total_rounds: u32,
    pub consecutive_clean: u32,
    pub issues: Vec<Issue>,
    pub progress: ProgressTracker,
    pub level_transitions: Vec<LevelTransition>,
}

impl MetaConvergenceState {
    pub fn new(stagnation_threshold: u32) -> Self {
        Self {
            current_level: ConvergenceLevel::DirectFix,
            total_rounds: 0,
            consecutive_clean: 0,
            issues: Vec::new(),
            progress: ProgressTracker::new(stagnation_threshold),
            level_transitions: Vec::new(),
        }
    }

    pub fn record_round(&mut self, issues: Vec<Issue>) {
        self.total_rounds += 1;

        if issues.is_empty() {
            self.consecutive_clean += 1;
        } else {
            self.consecutive_clean = 0;
        }

        let critical_count = issues
            .iter()
            .filter(|i| i.severity == crate::verification::IssueSeverity::Critical)
            .count();

        self.progress.record(crate::domain::ProgressSnapshot::new(
            self.total_rounds,
            issues.len(),
            critical_count,
        ));

        self.issues = issues;
    }

    pub fn escalate_level(&mut self) -> Option<ConvergenceLevel> {
        if let Some(next) = self.current_level.next() {
            self.level_transitions.push(LevelTransition {
                from: self.current_level,
                to: next,
                at_round: self.total_rounds,
            });
            self.current_level = next;
            Some(next)
        } else {
            None
        }
    }

    pub fn is_converged(&self, required_clean_rounds: u32) -> bool {
        self.consecutive_clean >= required_clean_rounds
    }
}

#[derive(Debug, Clone)]
pub struct LevelTransition {
    pub from: ConvergenceLevel,
    pub to: ConvergenceLevel,
    pub at_round: u32,
}

/// Result of meta-convergence attempt.
#[derive(Debug)]
pub enum MetaConvergenceResult {
    /// Successfully converged.
    Converged {
        rounds: u32,
        final_level: ConvergenceLevel,
    },

    /// Partially converged (non-critical issues accepted).
    PartiallyConverged {
        rounds: u32,
        accepted_issues: Vec<Issue>,
    },

    /// Human intervention required.
    NeedsHuman {
        state: MetaConvergenceState,
        context: Box<EscalationContext>,
    },

    /// Timed out.
    Timeout {
        state: MetaConvergenceState,
        best_round: Option<u32>,
    },
}

impl MetaConvergenceResult {
    pub fn is_success(&self) -> bool {
        matches!(
            self,
            Self::Converged { .. } | Self::PartiallyConverged { .. }
        )
    }

    pub fn rounds(&self) -> u32 {
        match self {
            Self::Converged { rounds, .. } | Self::PartiallyConverged { rounds, .. } => *rounds,
            Self::NeedsHuman { state, .. } | Self::Timeout { state, .. } => state.total_rounds,
        }
    }
}

/// Strategy for attempting convergence at a specific level.
#[async_trait]
pub trait ConvergenceStrategy: Send + Sync {
    /// Strategy level.
    fn level(&self) -> ConvergenceLevel;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Attempt convergence using this strategy.
    async fn attempt(
        &self,
        state: &MetaConvergenceState,
        context: &ConvergenceContext,
    ) -> Result<ConvergenceAttempt>;
}

/// Context for convergence attempt.
#[derive(Debug, Clone)]
pub struct ConvergenceContext {
    pub mission_id: String,
    pub working_dir: std::path::PathBuf,
    pub max_rounds_per_level: u32,
    pub required_clean_rounds: u32,
}

/// Result of a single convergence attempt.
#[derive(Debug)]
pub struct ConvergenceAttempt {
    pub result: AttemptResult,
    pub new_issues: Vec<Issue>,
    pub resolved_issues: Vec<Issue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptResult {
    /// Achieved required clean rounds.
    Converged,
    /// Making progress (issues decreasing).
    Progress,
    /// No change in issues.
    Stagnating,
    /// Issues increasing.
    Degrading,
}
