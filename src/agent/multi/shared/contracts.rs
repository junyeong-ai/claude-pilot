//! Layer contracts for message-event conversion.
//!
//! Design principles:
//! - Messages are Commands (transient, trigger state changes)
//! - Events are Facts (permanent, enable state reconstruction)
//! - Explicit conversion preserves all semantic information

use crate::state::DomainEvent;

/// Context for event creation.
#[derive(Debug, Clone, Default)]
pub struct EventContext {
    pub mission_id: String,
    pub correlation_id: Option<String>,
    pub causation_id: Option<String>,
    pub agent_id: Option<String>,
}

impl EventContext {
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            correlation_id: None,
            causation_id: None,
            agent_id: None,
        }
    }

    pub fn with_correlation(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }

    pub fn with_causation(mut self, id: impl Into<String>) -> Self {
        self.causation_id = Some(id.into());
        self
    }

    pub fn with_agent(mut self, id: impl Into<String>) -> Self {
        self.agent_id = Some(id.into());
        self
    }
}

/// Message to Event conversion trait.
pub trait ToEvent {
    fn to_event(&self, ctx: &EventContext) -> DomainEvent;
}

/// Event to State projection trait.
pub trait ApplyEvent<S> {
    fn apply(&self, state: &mut S);
}

/// Message validation trait.
pub trait ValidateMessage {
    type Error;
    fn validate(&self) -> Result<(), Self::Error>;
}
