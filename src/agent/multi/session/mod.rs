//! Orchestration session management for multi-agent coordination.
//!
//! Provides the central orchestration unit managing the entire lifecycle
//! of a multi-agent mission: participant tracking, task dependencies,
//! notification-based communication, and state management.

mod agent_context;
mod checkpoint;
mod continuation;
mod health_adapter;
mod invocation;
mod notification;
mod orchestration;
mod participant;
mod task_graph;

// --- Primary API: used by coordinator and other agent::multi siblings ---
pub use orchestration::{OrchestrationConfig, OrchestrationSession, SessionPhase, SharedSession};
pub use agent_context::AgentContext;
pub use notification::NotificationType;
pub use continuation::ContinuationManager;
pub use health_adapter::SessionHealthTracker;
pub use invocation::InvocationTracker;
pub use participant::AgentStatus;
