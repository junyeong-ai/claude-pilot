mod assessment;
mod build_system;
mod lint_detector;
mod quality_gates;
mod search;
mod task;
mod verification;

pub use assessment::{ComplexityAssessmentConfig, EvidenceBudgetConfig, PatternBankConfig};
pub use build_system::BuildSystem;
pub use quality_gates::QualityConfig;
pub use search::{FocusConfig, SearchConfig};
pub use task::{TaskDecompositionConfig, TaskScoringConfig, TaskScoringWeights};
pub use verification::{ModuleConfig, VerificationCommands, VerificationConfig};

#[cfg(test)]
mod tests;
