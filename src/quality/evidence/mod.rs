mod adapter;
mod filter;
mod gathering;
mod sufficiency;

pub use adapter::EnhancedEvidenceGatherer;
pub use gathering::{EvidenceGatheringLoop, GatheringResult};
pub use sufficiency::{
    EvidenceRequirement, EvidenceSufficiencyChecker, RequirementCategory, RequirementPriority,
    SufficiencyContext,
};
