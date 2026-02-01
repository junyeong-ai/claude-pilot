pub mod agent;
pub mod cli;
pub mod config;
pub mod context;
pub mod domain;
pub mod error;
pub mod git;
pub mod isolation;
pub mod learning;
pub mod mission;
pub mod notification;
pub mod orchestration;
pub mod orchestrator;
pub mod output;
pub mod planning;
pub mod quality;
pub mod recovery;
pub mod search;
pub mod session;
pub mod state;
pub mod strategy;
pub mod summarization;
pub mod symora;
pub mod utils;
pub mod verification;
pub mod workspace;

pub use context::{ContextManager, MissionContext};
pub use domain::{EscalationContext, HumanAction, HumanResponse, ProgressTracker};
pub use error::{PilotError, Result};
pub use git::{GhRunner, GitRunner};
pub use orchestration::{AgentScope, Mission, MissionOrchestrator, MissionResult, MissionStatus};
pub use planning::{
    ChunkedPlanner, ComplexityEstimator, Evidence, PlanningOrchestrator, PlanningResult,
};
pub use recovery::{FailureAnalyzer, RecoveryExecutor};
pub use search::{FixSearcher, SearchQuery};
pub use session::SessionBridge;
pub use state::MissionState;
pub use strategy::{
    ConvergenceLevel, EvidenceEscalator, EvidenceResult, GatheringContext,
    MetaConvergenceOrchestrator, MetaConvergenceResult, MetaConvergenceState,
    PartialConvergencePolicy,
};
pub use summarization::TextCompressor;
pub use symora::SymoraClient;
pub use verification::ConvergentVerifier;
pub use workspace::{Workspace, WorkspaceSet};
