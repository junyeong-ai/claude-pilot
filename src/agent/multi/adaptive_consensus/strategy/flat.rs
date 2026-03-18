use std::collections::HashMap;
use std::time::Duration;

use tracing::info;

use super::super::super::shared::AgentId;
use super::super::super::pool::AgentPool;
use super::super::{AdaptiveConsensusExecutor, ConsensusMission, HierarchicalConsensusResult};
use crate::error::Result;

impl AdaptiveConsensusExecutor {
    /// Execute flat consensus.
    pub(in crate::agent::multi::adaptive_consensus) async fn execute_flat(
        &self,
        session_id: &str,
        participants: &[AgentId],
        mission: &ConsensusMission,
        pool: &AgentPool,
    ) -> Result<HierarchicalConsensusResult> {
        info!(
            session_id = %session_id,
            participant_count = participants.len(),
            "Executing flat consensus"
        );

        let result = self
            .consensus_engine
            .run_with_dynamic_joining(
                &mission.description,
                &mission.context,
                participants,
                pool,
                |requests| Self::resolve_agent_requests(requests, pool),
            )
            .await?;

        let (outcome, plan, tasks, rounds) = self.unpack_consensus_result(result);

        Ok(HierarchicalConsensusResult {
            session_id: session_id.to_string(),
            outcome,
            plan,
            tasks,
            tier_results: HashMap::new(),
            total_rounds: rounds,
            duration: Duration::ZERO,
        })
    }
}
