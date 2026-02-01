//! Evidence gathering strategies with multi-level escalation.

use std::path::Path;

use async_trait::async_trait;

use crate::error::Result;
use crate::planning::Evidence;

mod context;
mod escalator;

pub use context::{GatheringContext, SearchHint, SearchHintType};
pub use escalator::EvidenceEscalator;

/// Result of evidence gathering attempt.
#[derive(Debug)]
pub enum EvidenceResult {
    /// Evidence gathering completed successfully.
    Complete(Evidence),

    /// Evidence is sufficient but has known gaps.
    Sufficient {
        evidence: Evidence,
        gaps: Vec<EvidenceGap>,
    },

    /// Evidence gathering requires human input.
    NeedsHuman {
        partial: Evidence,
        context: Box<GatheringContext>,
    },
}

impl EvidenceResult {
    pub fn evidence(&self) -> &Evidence {
        match self {
            Self::Complete(e) | Self::Sufficient { evidence: e, .. } => e,
            Self::NeedsHuman { partial, .. } => partial,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }

    pub fn is_sufficient(&self) -> bool {
        matches!(self, Self::Complete(_) | Self::Sufficient { .. })
    }
}

/// Gap identified in evidence.
#[derive(Debug, Clone)]
pub struct EvidenceGap {
    pub area: String,
    pub importance: GapImportance,
    pub description: String,
}

impl EvidenceGap {
    pub fn new(area: impl Into<String>, importance: GapImportance) -> Self {
        Self {
            area: area.into(),
            importance,
            description: String::new(),
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapImportance {
    Low,
    Medium,
    High,
    Critical,
}

/// Strategy for gathering evidence at a specific level.
#[async_trait]
pub trait EvidenceStrategy: Send + Sync {
    /// Strategy level (0 = Pattern, 1 = LSP, 2 = LLM, 3 = Human).
    fn level(&self) -> u8;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Check if this strategy can address a specific gap.
    fn can_address_gap(&self, gap: &EvidenceGap) -> bool;

    /// Gather evidence using this strategy.
    async fn gather(
        &self,
        context: &GatheringContext,
        working_dir: &Path,
    ) -> Result<GatheringOutcome>;
}

/// Outcome of a single gathering attempt.
#[derive(Debug)]
pub struct GatheringOutcome {
    pub evidence: Evidence,
    pub remaining_gaps: Vec<EvidenceGap>,
    pub should_escalate: bool,
}

impl GatheringOutcome {
    pub fn complete(evidence: Evidence) -> Self {
        Self {
            evidence,
            remaining_gaps: Vec::new(),
            should_escalate: false,
        }
    }

    pub fn partial(evidence: Evidence, gaps: Vec<EvidenceGap>) -> Self {
        let should_escalate = !gaps.is_empty();
        Self {
            evidence,
            remaining_gaps: gaps,
            should_escalate,
        }
    }

    pub fn needs_escalation(evidence: Evidence, gaps: Vec<EvidenceGap>) -> Self {
        Self {
            evidence,
            remaining_gaps: gaps,
            should_escalate: true,
        }
    }
}
