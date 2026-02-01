pub(crate) mod coherence;
pub(crate) mod context;
pub(crate) mod evidence;

mod config;
mod types;

pub use config::{
    CoherenceConfig, CoherenceThresholds, ConfidenceScores, EvidenceGatheringConfig,
    EvidenceSourcesConfig, EvidenceSufficiencyConfig, EvidenceVerificationConfig,
};

pub use types::{
    AggregatedCoherence, CodeLocation, CoherenceCheckType, CoherenceIssue, CoherenceResult,
    FileChange, FileChangeType, Severity,
};

pub use coherence::{CoherenceChecker, CoherenceReport, CoherenceTaskResult};

pub use evidence::{
    EnhancedEvidenceGatherer, EvidenceGatheringLoop, EvidenceRequirement,
    EvidenceSufficiencyChecker, GatheringResult, RequirementCategory, RequirementPriority,
};
