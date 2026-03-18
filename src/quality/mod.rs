pub(crate) mod coherence;
pub(crate) mod context;
pub(crate) mod evidence;

mod config;
mod types;

pub use config::{
    CoherenceConfig, CoherenceThresholds, ConfidenceScores, EvidenceGatheringConfig,
    EvidenceSufficiencyConfig,
};

pub use types::{
    AggregatedCoherence, CodeLocation, CoherenceCheckType, CoherenceIssue, CoherenceResult,
    QualityFileChange, QualityFileChangeType,
};

pub use coherence::{CoherenceChecker, CoherenceReport, CoherenceTaskResult};

pub use evidence::{
    EnhancedEvidenceGatherer, EvidenceGatheringLoop, EvidenceRequirement,
    EvidenceSufficiencyChecker, GatheringResult, RequirementCategory,
};
