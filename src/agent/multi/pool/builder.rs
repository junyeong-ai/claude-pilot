//! Builder pattern for AgentPool construction.

use std::sync::Arc;

use super::AgentPool;
use super::super::ownership::FileOwnershipManager;
use super::super::traits::SpecializedAgent;
use crate::config::MultiAgentConfig;
use crate::error::Result;

/// Builder for AgentPool with agent registration.
pub struct AgentPoolBuilder {
    config: MultiAgentConfig,
    agents: Vec<Arc<dyn SpecializedAgent>>,
    ownership_manager: Option<Arc<FileOwnershipManager>>,
}

impl AgentPoolBuilder {
    pub fn new(config: MultiAgentConfig) -> Self {
        Self {
            config,
            agents: Vec::new(),
            ownership_manager: None,
        }
    }

    pub fn with_agent(mut self, agent: Arc<dyn SpecializedAgent>) -> Self {
        self.agents.push(agent);
        self
    }

    pub fn with_agents(mut self, agents: Vec<Arc<dyn SpecializedAgent>>) -> Self {
        self.agents.extend(agents);
        self
    }

    pub fn with_ownership_manager(mut self, manager: Arc<FileOwnershipManager>) -> Self {
        self.ownership_manager = Some(manager);
        self
    }

    pub fn build(self) -> Result<AgentPool> {
        let mut pool = AgentPool::new(self.config);
        if let Some(manager) = self.ownership_manager {
            pool = pool.with_ownership_manager(manager);
        }
        for agent in self.agents {
            pool.register(agent)?;
        }
        Ok(pool)
    }
}
