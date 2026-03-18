use std::collections::HashMap;

use super::super::shared::{ConsensusSessionStatus, TierLevel, TierResult};

/// State for checkpoint/recovery.
#[derive(Debug, Clone)]
pub struct AdaptiveConsensusState {
    pub session_id: String,
    pub current_round: usize,
    pub current_tier: Option<TierLevel>,
    pub completed_tiers: Vec<String>,
    pub tier_results: HashMap<String, TierResult>,
    pub status: ConsensusSessionStatus,
}

impl AdaptiveConsensusState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            current_round: 0,
            current_tier: None,
            completed_tiers: Vec::new(),
            tier_results: HashMap::new(),
            status: ConsensusSessionStatus::Running,
        }
    }

    pub fn content_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.session_id.as_bytes());
        hasher.update(self.current_round.to_le_bytes());

        if let Some(ref tier) = self.current_tier {
            let tier_str = tier.as_str();
            hasher.update([1u8]);
            hasher.update((tier_str.len() as u32).to_le_bytes());
            hasher.update(tier_str.as_bytes());
        } else {
            hasher.update([0u8]);
        }

        hasher.update((self.completed_tiers.len() as u32).to_le_bytes());
        for tier in &self.completed_tiers {
            hasher.update((tier.len() as u32).to_le_bytes());
            hasher.update(tier.as_bytes());
        }

        let mut result_keys: Vec<_> = self.tier_results.keys().collect();
        result_keys.sort();
        hasher.update((result_keys.len() as u32).to_le_bytes());
        for key in result_keys {
            hasher.update((key.len() as u32).to_le_bytes());
            hasher.update(key.as_bytes());
            if let Some(r) = self.tier_results.get(key) {
                hasher.update(r.tier_level.as_str().as_bytes());
                hasher.update([r.converged as u8]);
                hasher.update([r.timed_out as u8]);
                hasher.update((r.respondent_count as u32).to_le_bytes());
                hasher.update((r.rounds as u32).to_le_bytes());
                hasher.update((r.synthesis.len() as u32).to_le_bytes());
                hasher.update(r.synthesis.as_bytes());
                hasher.update((r.conflicts.len() as u32).to_le_bytes());
                for conflict in &r.conflicts {
                    hasher.update((conflict.len() as u32).to_le_bytes());
                    hasher.update(conflict.as_bytes());
                }
                hasher.update((r.tasks.len() as u32).to_le_bytes());
                for task in &r.tasks {
                    hasher.update((task.id.len() as u32).to_le_bytes());
                    hasher.update(task.id.as_bytes());
                }
            }
        }

        hasher.update(self.status.as_str().as_bytes());

        format!("{:x}", hasher.finalize())
    }
}
