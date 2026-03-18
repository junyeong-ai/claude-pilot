mod passes;
pub(crate) mod rules;
mod types;

#[cfg(test)]
mod tests;

pub use types::{
    QualityTier, ValidationResult,
};

use std::path::{Path, PathBuf};

use super::plan_agent::PlanAgent;
use crate::config::{AgentConfig, QualityConfig};

pub struct PlanValidator {
    executor: PlanAgent,
    quality_config: QualityConfig,
    working_dir: PathBuf,
}

impl PlanValidator {
    pub fn with_config(
        working_dir: &Path,
        agent_config: &AgentConfig,
        quality_config: QualityConfig,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            executor: PlanAgent::new(working_dir, agent_config)?,
            quality_config,
            working_dir: working_dir.to_path_buf(),
        })
    }
}
