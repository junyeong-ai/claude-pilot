//! Mission orchestration for manifest-based multi-agent workflows.

mod mission;
mod scope;

pub use mission::{Mission, MissionOrchestrator, MissionResult, MissionStatus};
pub use scope::AgentScope;
