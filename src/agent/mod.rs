//! Task execution agents with structured output support.
//!
//! Provides single-agent execution (`TaskAgent`, `DirectAgent`) and
//! multi-agent architecture (`multi` module) for specialized task execution.

mod direct_agent;
mod executor;
pub mod multi;
mod prompt;
mod task_agent;

pub use direct_agent::{DirectAgent, DirectResult};
pub use executor::LlmExecutor;
pub use multi::{
    AgentPool, AgentPoolBuilder, AgentRole, AgentTask, AgentTaskResult, ArchitectureAgent,
    BoxedAgent, CoderAgent, ConsensusEngine, ConsensusResult, ConsensusTask, Coordinator,
    MetricsSnapshot, MissionResult, ModuleAgent, PlanningAgent, PoolStatistics, ResearchAgent,
    ReviewerAgent, RoleCategory, ScopeValidation, SpecializedAgent, TaskContext, TaskPriority,
    VerifierAgent, create_agent_pool, create_coordinator, create_dynamic_pool,
};
pub use prompt::PromptBuilder;
pub use task_agent::{ResourceLoadingMode, TaskAgent};
