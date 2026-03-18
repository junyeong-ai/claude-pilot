//! Agent selection strategies and scoring.

use std::sync::atomic::Ordering;

use tracing::debug;

use super::AgentPool;
use super::super::identity::AgentIdentifier;
use super::super::shared::AgentId;
use super::super::traits::{AgentExecutionMetrics, AgentRole, BoxedAgent};
use crate::config::SelectionPolicy;

/// Stride value for pseudo-random agent selection.
///
/// This prime number (7) is used to create pseudo-random distribution when selecting
/// agents from a pool without the overhead of a proper RNG. By incrementing a counter
/// by this stride and taking modulo of the pool size, we get good distribution across
/// agents while avoiding the patterns that would emerge from sequential (stride=1)
/// selection.
///
/// A prime number works well because it's coprime to most pool sizes, ensuring we cycle
/// through all agents before repeating.
const RANDOM_SELECTION_STRIDE: usize = 7;

impl AgentPool {
    pub fn select(&self, role: &AgentRole) -> Option<BoxedAgent> {
        self.select_by_id(&role.id)
    }

    pub fn select_by_id(&self, role_id: impl AsRef<str>) -> Option<BoxedAgent> {
        let role_id = role_id.as_ref();
        let agents = self.agents.read();
        let pool = agents.get(role_id)?;

        let pool_len = pool.len();
        if pool_len == 0 {
            return None;
        }

        match self.config.selection_policy {
            SelectionPolicy::RoundRobin => {
                let counters = self.counters.read();
                let idx = counters
                    .get(role_id)
                    .map_or(0, |c| c.fetch_add(1, Ordering::Relaxed));
                pool.get(idx % pool_len).cloned()
            }
            SelectionPolicy::LeastLoaded => pool.iter().min_by_key(|a| a.current_load()).cloned(),
            SelectionPolicy::Random => {
                let counters = self.counters.read();
                let idx = counters.get(role_id).map_or(0, |c| {
                    c.fetch_add(RANDOM_SELECTION_STRIDE, Ordering::Relaxed)
                });
                pool.get(idx % pool_len).cloned()
            }
            SelectionPolicy::Scored => {
                let metrics = self.metrics.snapshot();
                let max_load = self.config.max_agent_load;
                let weights = &self.config.selection_weights;
                pool.iter()
                    .map(|agent| {
                        let score = AgentScore::from_agent(agent, &metrics, max_load, weights);
                        (agent, score.total_with_weights(weights))
                    })
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .map(|(agent, _)| agent.clone())
            }
        }
    }

    /// Select a specific agent instance by its qualified ID with fallback.
    ///
    /// Qualified ID format: "workspace:module:role-instance" or "role-instance"
    /// This is used for consensus protocols that need specific agent instances.
    ///
    /// The lookup follows this order:
    /// 1. Exact match on the qualified ID
    /// 2. Role-only match (strips workspace/module prefix for cross-workspace scenarios)
    pub fn select_by_qualified_id(&self, qualified_id: &str) -> Option<BoxedAgent> {
        if qualified_id.is_empty() {
            return None;
        }

        let by_qid = self.by_qualified_id.read();

        // 1. Exact match
        if let Some(agent) = by_qid.get(qualified_id) {
            return Some(agent.clone());
        }

        // 2. Fallback: try role-only match (last segment after ':')
        // e.g., "ws-a:auth:coder-0" -> "coder-0"
        let role_part = qualified_id.rsplit(':').next()?;
        if role_part.is_empty() {
            return None;
        }

        // Collect and sort candidates for deterministic selection
        // Priority: exact role match > shorter ID > alphabetically first
        let mut candidates: Vec<_> = by_qid
            .iter()
            .filter(|(id, _)| *id == role_part || id.ends_with(&format!(":{}", role_part)))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by: exact match first, then by ID length, then alphabetically
        candidates.sort_by(|(id_a, _), (id_b, _)| {
            let exact_a = *id_a == role_part;
            let exact_b = *id_b == role_part;
            match (exact_a, exact_b) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => id_a.len().cmp(&id_b.len()).then_with(|| id_a.cmp(id_b)),
            }
        });

        let (matched_id, agent) = candidates[0];
        debug!(
            qualified_id = %qualified_id,
            matched_id = %matched_id,
            candidates_count = candidates.len(),
            "Fallback match for cross-workspace agent selection"
        );
        Some(agent.clone())
    }

    /// Select a specific agent instance by its AgentIdentifier.
    pub fn select_by_identifier(&self, identifier: &AgentIdentifier) -> Option<BoxedAgent> {
        self.select_by_qualified_id(&identifier.qualified_id())
    }

    /// Get all agent instances for a given pool key.
    ///
    /// Pool key format: "workspace:module:role" or "role"
    pub fn select_instances(&self, pool_key: &str) -> Vec<BoxedAgent> {
        self.agents
            .read()
            .get(pool_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Get all agent instances for a given AgentIdentifier's pool key.
    pub fn select_instances_for(&self, identifier: &AgentIdentifier) -> Vec<BoxedAgent> {
        self.select_instances(&identifier.pool_key())
    }

    /// Get all agent identifiers for a given pool key.
    pub fn identifiers_for_pool(&self, pool_key: &str) -> Vec<AgentIdentifier> {
        self.agents
            .read()
            .get(pool_key)
            .map(|agents| agents.iter().map(|a| a.identifier()).collect())
            .unwrap_or_default()
    }

    /// Get all registered role IDs.
    pub fn role_ids(&self) -> Vec<String> {
        self.agents.read().keys().cloned().collect()
    }

    /// Get role IDs for planning-relevant agents only (excludes QA, verifier, coder).
    pub fn planning_role_ids(&self) -> Vec<String> {
        self.agents
            .read()
            .iter()
            .filter(|(_, agents)| {
                agents
                    .first()
                    .is_some_and(|a| a.role().is_planning_relevant())
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Get all agents for a given role ID.
    pub fn select_all_by_id(&self, role_id: &str) -> Vec<BoxedAgent> {
        self.agents.read().get(role_id).cloned().unwrap_or_default()
    }

    /// Get all agents for a given role.
    pub fn select_all(&self, role: &AgentRole) -> Vec<BoxedAgent> {
        self.select_all_by_id(&role.id)
    }

    /// Get all agent IDs for a given role.
    pub fn agent_ids_for(&self, role: &AgentRole) -> Vec<AgentId> {
        self.agent_ids_for_key(&role.id)
    }

    /// Get all agent IDs for a given pool key (e.g., "module-auth", "planning").
    pub fn agent_ids_for_key(&self, pool_key: &str) -> Vec<AgentId> {
        self.agents
            .read()
            .get(pool_key)
            .map(|agents| agents.iter().map(|a| AgentId::new(a.id())).collect())
            .unwrap_or_default()
    }
}

/// Weighted score for agent selection.
#[derive(Debug, Clone, Copy)]
pub struct AgentScore {
    pub capability: f64,
    pub load: f64,
    pub performance: f64,
    pub health: f64,
    pub availability: f64,
}

impl AgentScore {
    pub fn total_with_weights(&self, weights: &crate::config::SelectionWeightsConfig) -> f64 {
        self.capability * weights.capability
            + self.load * weights.load
            + self.performance * weights.performance
            + self.health * weights.health
            + self.availability * weights.availability
    }

    pub fn from_agent(
        agent: &BoxedAgent,
        metrics: &AgentExecutionMetrics,
        max_load: u32,
        weights: &crate::config::SelectionWeightsConfig,
    ) -> Self {
        let current_load = agent.current_load();

        let load_score = 1.0 - (current_load as f64 / max_load as f64).min(1.0);
        let availability_score = if current_load < max_load { 1.0 } else { 0.0 };

        let performance_score = if metrics.total_executions > 0 {
            metrics.success_rate.min(1.0)
        } else {
            weights.default_new_agent_score
        };

        let health_score = if metrics.avg_duration_ms < weights.health_good_latency_ms {
            1.0
        } else if metrics.avg_duration_ms < weights.health_degraded_latency_ms {
            0.7
        } else {
            0.4
        };

        Self {
            capability: 1.0,
            load: load_score,
            performance: performance_score,
            health: health_score,
            availability: availability_score,
        }
    }
}
