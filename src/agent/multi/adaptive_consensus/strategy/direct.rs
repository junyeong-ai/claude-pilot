use std::collections::HashMap;
use std::time::Duration;

use tracing::debug;

use super::super::super::shared::AgentId;
use super::super::super::pool::AgentPool;
use super::super::{AdaptiveConsensusExecutor, ConsensusMission, HierarchicalConsensusResult};
use crate::error::Result;

impl AdaptiveConsensusExecutor {
    /// Execute direct (single participant, no consensus).
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_direct(
        &self,
        session_id: &str,
        participant: &AgentId,
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        debug!(
            session_id = %session_id,
            participant = %participant,
            "Executing direct (no consensus needed)"
        );

        let result = self
            .consensus_engine
            .run_with_dynamic_joining(
                &mission.description,
                &mission.context,
                std::slice::from_ref(participant),
                pool,
                |_| vec![],
            )
            .await?;

        let (outcome, plan, tasks, _) = self.unpack_consensus_result(result);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: HashMap::new(),
            total_rounds: 1,
            duration: Duration::ZERO,
        })
    }
}
