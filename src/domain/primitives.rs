//! Domain primitive types shared across module boundaries.
//!
//! These types are fundamental domain concepts used by both the event
//! sourcing infrastructure (`state/`) and the multi-agent system (`agent/multi/`).
//! Placing them here breaks the circular dependency.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteDecision {
    Approve,
    ApproveWithChanges,
    Reject,
    Abstain,
}

impl VoteDecision {
    pub fn is_positive(&self) -> bool {
        matches!(self, Self::Approve | Self::ApproveWithChanges)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::ApproveWithChanges => "approve_with_changes",
            Self::Reject => "reject",
            Self::Abstain => "abstain",
        }
    }
}

impl fmt::Display for VoteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Canonical severity level used across the entire codebase.
///
/// Ordered from least to most severe: Info < Warning < Error < Critical.
/// Derives `PartialOrd`/`Ord` so comparisons like `severity >= Severity::Error` work.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }

    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Critical)
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warning => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

impl From<crate::symora::DiagnosticSeverity> for Severity {
    fn from(ds: crate::symora::DiagnosticSeverity) -> Self {
        match ds {
            crate::symora::DiagnosticSeverity::Error => Self::Error,
            crate::symora::DiagnosticSeverity::Warning => Self::Warning,
            crate::symora::DiagnosticSeverity::Information => Self::Info,
            crate::symora::DiagnosticSeverity::Hint => Self::Info,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundOutcome {
    Approved,
    NeedsMoreRounds,
    Failed,
    Escalated,
}

impl RoundOutcome {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Approved | Self::Failed | Self::Escalated)
    }
}

/// Escalation levels for conflict resolution, ordered by severity.
///
/// The escalation process follows this progression:
/// 1. **Retry** (0): Simple retry with different agents
/// 2. **ScopeReduction** (1): Reduce scope by executing non-conflicting tasks only
/// 3. **ArchitecturalMediation** (2): Request architect agent to mediate
/// 4. **SplitAndConquer** (3): Split task into smaller, independent units
/// 5. **HumanEscalation** (4): Request human intervention (terminal level)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationLevel {
    Retry = 0,
    ScopeReduction = 1,
    ArchitecturalMediation = 2,
    SplitAndConquer = 3,
    HumanEscalation = 4,
}

impl EscalationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::ScopeReduction => "scope_reduction",
            Self::ArchitecturalMediation => "architectural_mediation",
            Self::SplitAndConquer => "split_and_conquer",
            Self::HumanEscalation => "human_escalation",
        }
    }

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Retry => Some(Self::ScopeReduction),
            Self::ScopeReduction => Some(Self::ArchitecturalMediation),
            Self::ArchitecturalMediation => Some(Self::SplitAndConquer),
            Self::SplitAndConquer => Some(Self::HumanEscalation),
            Self::HumanEscalation => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quorum {
    #[default]
    Majority,
    Supermajority,
    Unanimous,
    UnanimousWithFallback,
}

/// Strategy kind for consensus execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusStrategyKind {
    Direct,
    Flat,
    Hierarchical,
}

impl fmt::Display for ConsensusStrategyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => write!(f, "direct"),
            Self::Flat => write!(f, "flat"),
            Self::Hierarchical => write!(f, "hierarchical"),
        }
    }
}

impl Quorum {
    pub fn threshold(&self, total: usize) -> usize {
        match self {
            Self::Majority => total / 2 + 1,
            Self::Supermajority => (total * 2).div_ceil(3),
            Self::Unanimous | Self::UnanimousWithFallback => total,
        }
    }

    pub fn is_met(&self, votes: usize, total: usize) -> bool {
        votes >= self.threshold(total)
    }
}
