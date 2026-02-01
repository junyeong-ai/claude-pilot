//! Orchestration session management for multi-agent coordination.
//!
//! This module provides the central orchestration unit that manages the entire
//! lifecycle of a multi-agent mission, including participant tracking, task
//! dependencies, and state management.
//!
//! # Overview
//!
//! The session module implements the core patterns for stateless agent coordination:
//!
//! - **Notification Log Pattern**: Notifications stored and queried instead of broadcast
//! - **Checkpoint Pattern**: Lightweight checkpoints with references to external state
//! - **Task DAG Pattern**: Dependency-aware task scheduling
//! - **Participant Registry Pattern**: Full agent awareness for context injection
//!
//! # Components
//!
//! - [`OrchestrationSession`]: Central coordination unit managing the entire mission
//! - [`ParticipantRegistry`]: Tracks all agents with indexing by module/group/workspace/role
//! - [`TaskDAG`]: Manages task dependencies and execution ordering
//! - [`NotificationLog`]: Stores notifications for query-based retrieval
//! - [`CheckpointManager`]: Creates and manages recovery checkpoints
//!
//! # Usage
//!
//! ```ignore
//! use claude_pilot::agent::multi::session::{
//!     OrchestrationSession, SessionConfig, Phase,
//! };
//!
//! // Create session
//! let mut session = OrchestrationSession::new("Implement feature X", working_dir)
//!     .with_config(SessionConfig::default())
//!     .with_context_budget(200000, 10000);
//!
//! // Register participants
//! session.register_agent("auth-planning-0", "auth", "project-a", capabilities);
//!
//! // Add tasks
//! session.add_task(task_info);
//!
//! // Start session
//! session.start();
//!
//! // Execute workflow...
//! ```

mod agent_context;
mod checkpoint;
mod comm_tools;
mod consensus_state;
mod continuation;
mod health_adapter;
mod invocation;
mod notification;
mod orchestration;
mod participant;
mod priority_bus;
mod shared_state;
mod task_complexity;
mod task_graph;
mod task_scorer;
mod tool_paths;

pub use agent_context::{
    AgentContext, AgentContextBuilder, ConflictSummary, NotificationSummary, TaskAssignment,
    TaskSummary,
};
pub use checkpoint::{
    ArchiveRef, Checkpoint, CheckpointManager, CheckpointReason, CheckpointRef, CompactedContext,
    FileSnapshot, RecoveryContext,
};
pub use comm_tools::{
    AcquireLockInput, AcquireLockOutput, CommunicationTools, CoordinateInput, CoordinateOutput,
    CoordinationAction, CoordinationInfo, LockInfo, LockTypeInput, NotificationTypeInput,
    NotifyInput, NotifyOutput, PriorityInput, ProgressInfo, QueryFilters, QueryInput, QueryOutput,
    QueryResults, QueryType, SaveProgressInput, SaveProgressOutput, ToolDefinition, ToolError,
    ToolResult, get_tool_definitions,
};
pub use consensus_state::{
    ConsensusRound, ConsensusState, ConsensusSummary, Proposal, ProposalScore, RoundStatus,
    TallyResult, TallyStatus, Vote,
};
pub use continuation::{
    ContinuationCheckpoint, ContinuationManager, ContinuationState, ContinuationStats,
    ContinuationStatus, ResumeRequirements, follow_up_to_requirements, generate_continuation_id,
};
pub use health_adapter::{
    HealthRecommendation, ParticipantHealth, ParticipantHealthMetrics, RecommendedAction,
    SessionHealthConfig, SessionHealthReport, SessionHealthStatus, SessionHealthTracker,
};
pub use invocation::{
    ActionType, AgentAction, CoordinationRequest, FollowUp, FollowUpType, InvocationError,
    InvocationInput, InvocationOutcome, InvocationRecord, InvocationResult, InvocationStats,
    InvocationTracker, NotificationRequest, TokenUsage, generate_invocation_id,
};
pub use notification::{
    Notification, NotificationFilter, NotificationLog, NotificationType, NotificationTypeFilter,
};
pub use orchestration::{
    ContextBudget, OrchestrationSession, Phase, SessionConfig, SessionStatus, SessionSummary,
    SharedSession, create_shared_session,
};
pub use participant::{
    AgentCapabilities, AgentStatus, Participant, ParticipantRegistry, ParticipantSummary,
};
pub use priority_bus::{
    BusStats, MessageType as PriorityMessageType, Priority, PriorityMessage, PriorityMessageBus,
    Subscriber,
};
pub use shared_state::{
    CoordinationEntry, LockResult, LockType, ProgressEntry, ResourceLock, SharedState,
    SharedStateSnapshot,
};
pub use task_complexity::{
    ComplexityConfig, ComplexityEstimate, SplitSuggestion, TaskComplexityEstimator,
    should_split_quick,
};
pub use task_graph::{
    DependencyStatus, DependencyType, TaskDAG, TaskDependency, TaskInfo, TaskNode, TaskResult,
    TaskStats, TaskStatus,
};
pub use task_scorer::{
    AgentPerformance, AgentTaskScorer, AssignmentResult, ScoredAgent, ScorerConfig, TaskScore,
};
pub use tool_paths::{
    DeferralContext, DeferralHandler, DeferralReason, DeferredResult, PendingDeferral,
    ToolBehavior, ToolPathInfo, ToolPathResult, communication_tool_paths,
};
