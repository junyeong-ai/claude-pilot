mod gathering;
mod scoring;
mod types;

pub use types::{
    CodebaseAnalysis, EvidenceCompleteness,
    EvidenceQualitySummary, EvidenceSource, EvidenceSufficiency,
    ModuleTestMapping, QualityAssessment, QualityTier, RelevantFile,
    SourceConfidenceCategory, TestStructure,
};
pub use types::{DependencyAnalysis, PriorKnowledge};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub codebase_analysis: CodebaseAnalysis,
    pub dependency_analysis: DependencyAnalysis,
    pub prior_knowledge: PriorKnowledge,
    pub gathered_at: DateTime<Utc>,
    #[serde(default)]
    pub completeness: EvidenceCompleteness,
}
