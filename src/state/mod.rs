//! State management with event sourcing.

mod event_store;
mod events;
mod machine;
mod projections;
mod replay;
mod snapshots;
mod writer;

use crate::error::PilotError;

fn state_err(msg: impl std::fmt::Display) -> PilotError {
    PilotError::State(msg.to_string())
}

fn state_err_with<E: std::fmt::Display>(context: &str, err: E) -> PilotError {
    PilotError::State(format!("{}: {}", context, err))
}

pub use event_store::EventStore;
pub use events::{
    AggregateId, DomainEvent, EventFilter, EventId, EventPayload, RoundOutcome, VoteDecision,
};
pub use machine::{MissionState, StateTransition};
pub use projections::{
    ConsensusProjection, PatternStats, PatternStatsProjection, PhaseResult, ProgressProjection,
    Projection, ProjectionSnapshot, TaskState, TaskStatusProjection, TimelineEntry,
    TimelineProjection,
};
pub use replay::EventReplayer;
pub use snapshots::{DurableSnapshot, ProjectionSnapshotStore};
