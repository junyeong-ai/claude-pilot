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
pub(crate) use multi::AgentExecutionMetrics;
pub use prompt::PromptBuilder;
pub use task_agent::{ResourceLoadingMode, TaskAgent};
