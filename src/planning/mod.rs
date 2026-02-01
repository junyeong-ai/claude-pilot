//! Evidence-based planning and task decomposition.
//!
//! This module implements fact-based development (NON-NEGOTIABLE) by:
//! - Requiring pre-gathered evidence for all planning (enforced by type system)
//! - Estimating complexity to choose appropriate strategies
//! - Validating plans against gathered evidence
//!
//! Architecture:
//! - Evidence gathering: `quality/evidence/` module (`EnhancedEvidenceGatherer`)
//! - Planning: This module (requires `Evidence` parameter)
//! - Separation enables evidence reuse (complexity assessment → planning)
//!
//! Key components:
//! - `ComplexityEstimator`: Determines task complexity using evidence signals
//! - `PlanningOrchestrator`: Coordinates spec/plan/tasks generation
//! - `PlanValidator`: Validates plans against gathered evidence

mod artifacts;
mod chunked;
mod complexity;
mod evidence;
mod graph;
mod orchestrator;
mod plan_agent;
mod spec;
mod task_quality;
mod task_scorer;
mod tasks;
mod validation;

pub use artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
pub use chunked::{ChunkedPlanner, MissionOutline, OutlinePhase};
pub use complexity::{
    ComplexityEstimator, ComplexityExample, ComplexityExampleStore, ComplexityGate,
    ComplexityResult, ComplexityTier, ComplexityTierValue, WorkspaceDetectionConfidence,
    WorkspaceType,
};
pub use evidence::{
    CodebaseAnalysis, DependencyAnalysis, Evidence, EvidenceCompleteness, EvidenceQualitySummary,
    EvidenceSource, EvidenceSufficiency, ModuleTestMapping, PriorKnowledge, QualityAssessment,
    QualityTier, RelevantFile, SourceConfidenceCategory, TestStructure,
};
pub use orchestrator::{PlanningOrchestrator, PlanningResult};
pub use plan_agent::PlanAgent;
pub use spec::SpecificationAgent;
pub use task_quality::{TaskQualityResult, TaskQualityValidator};
pub use task_scorer::{HistoricalTask, SimilarityCalculator, TaskScore, TaskScorer};
pub use tasks::TaskDecomposer;
pub use validation::{PlanValidator, ValidationResult};
