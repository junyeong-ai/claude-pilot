//! Adaptive consensus executor with hierarchical support.
//!
//! This module provides the main executor for adaptive consensus, automatically
//! choosing between direct, flat, or hierarchical consensus based on participant count.

mod execution;
mod recovery;
mod strategy;
mod synthesis;
#[cfg(test)]
mod tests;
pub(crate) mod types;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::warn;
use uuid::Uuid;

use super::consensus::{ConsensusEngine, ConsensusResult, ConsensusTask};
use super::hierarchy::StrategySelector;
use super::metrics::MetricsObserver;
pub use super::shared::{ConsensusOutcome, ConsensusSessionStatus, TierResult};
use super::traits::TaskContext;
use crate::config::ConsensusConfig;
use crate::state::{DomainEvent, EventStore};

/// Mission context for consensus.
pub(crate) struct ConsensusMission {
    pub id: String,
    pub description: String,
    pub affected_files: Vec<PathBuf>,
    pub context: TaskContext,
}

impl ConsensusMission {
    pub fn new(description: impl Into<String>, files: Vec<PathBuf>, context: TaskContext) -> Self {
        Self {
            id: format!("mission-{}", Uuid::new_v4()),
            description: description.into(),
            affected_files: files,
            context,
        }
    }
}

/// Result of hierarchical consensus execution.
#[derive(Debug)]
pub(crate) struct HierarchicalConsensusResult {
    pub session_id: String,
    pub outcome: ConsensusOutcome,
    pub plan: Option<String>,
    pub tasks: Vec<ConsensusTask>,
    pub tier_results: HashMap<String, TierResult>,
    pub total_rounds: usize,
    pub duration: Duration,
}

/// Adaptive consensus executor.
pub struct AdaptiveConsensusExecutor {
    strategy_selector: StrategySelector,
    consensus_engine: ConsensusEngine,
    event_store: Option<Arc<EventStore>>,
    metrics: Option<Arc<dyn MetricsObserver>>,
    config: ConsensusConfig,
}

impl AdaptiveConsensusExecutor {
    pub fn new(consensus_engine: ConsensusEngine, config: ConsensusConfig) -> Self {
        Self {
            strategy_selector: StrategySelector::new(config.clone()),
            consensus_engine,
            event_store: None,
            metrics: None,
            config,
        }
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_metrics(mut self, metrics: Arc<dyn MetricsObserver>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Unpack consensus result into components: (outcome, plan, tasks, rounds).
    pub(super) fn unpack_consensus_result(
        &self,
        result: ConsensusResult,
    ) -> (ConsensusOutcome, Option<String>, Vec<ConsensusTask>, usize) {
        match result {
            ConsensusResult::Agreed {
                plan,
                tasks,
                rounds,
                ..
            } => (ConsensusOutcome::Converged, Some(plan), tasks, rounds),
            ConsensusResult::PartialAgreement { plan, tasks, .. } => (
                ConsensusOutcome::PartialConvergence,
                Some(plan),
                tasks,
                self.config.max_rounds,
            ),
            ConsensusResult::NoConsensus { summary, .. } => (
                ConsensusOutcome::Failed,
                Some(summary),
                vec![],
                self.config.max_rounds,
            ),
        }
    }

    /// Emit an event to the event store.
    pub(super) async fn emit_event(&self, event: DomainEvent) {
        let Some(store) = &self.event_store else {
            return;
        };
        if let Err(e) = store.append(event).await {
            warn!(error = %e, "Failed to emit consensus event");
        }
    }
}

/// Compute SHA256 hash of content.
pub(super) fn sha256_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
