//! Consensus engine for multi-agent planning through evidence-weighted voting.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use super::escalation::EscalationEngine;
use super::messaging::AgentMessageBus;
use crate::agent::TaskAgent;
use crate::config::ConsensusConfig;
use crate::state::EventStore;

// Submodules
pub(crate) mod conflict;
mod engine;
mod proposals;
mod rounds;
pub(crate) mod scoring;
#[cfg(test)]
mod tests;
pub mod types;

// Re-export conflict types
pub(crate) use conflict::Conflict;
pub use conflict::ResolutionStrategy;
pub use super::shared::TaskComplexity;
pub use types::{
    AgentPerformanceHistory, AgentProposal, AgentRequest, ConsensusResult, ScoredProposalRound,
    ConsensusSynthesisOutput, ConsensusTask, EnhancedProposal, EvidenceQuality,
    AgentProposalScore, ProposedApiChange, ProposedTypeChange, ScoredProposal,
};

/// Consensus engine with evidence-weighted voting and conflict resolution.
pub struct ConsensusEngine {
    aggregator: Arc<TaskAgent>,
    config: ConsensusConfig,
    agent_history: RwLock<HashMap<String, AgentPerformanceHistory>>,
    event_store: Option<Arc<EventStore>>,
    escalation_engine: Option<EscalationEngine>,
    message_bus: Option<Arc<AgentMessageBus>>,
}

impl ConsensusEngine {
    pub fn new(aggregator: Arc<TaskAgent>, config: ConsensusConfig) -> Self {
        Self {
            aggregator,
            config,
            agent_history: RwLock::new(HashMap::new()),
            event_store: None,
            escalation_engine: None,
            message_bus: None,
        }
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_escalation(mut self, engine: EscalationEngine) -> Self {
        self.escalation_engine = Some(engine);
        self
    }

    pub fn with_message_bus(mut self, bus: Arc<AgentMessageBus>) -> Self {
        self.message_bus = Some(bus);
        self
    }
}
