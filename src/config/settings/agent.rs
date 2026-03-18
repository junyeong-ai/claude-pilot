use serde::{Deserialize, Serialize};

use crate::config::model::{DEFAULT_MODEL, ModelConfig};

/// Authentication mode for Claude API access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// OAuth-based authentication via Claude Code CLI.
    /// NOTE: Extended context (1M tokens) is NOT available in OAuth mode.
    /// Context window is limited to 200k tokens regardless of extended_context setting.
    #[default]
    Oauth,
    /// Direct API key authentication.
    /// Extended context (1M tokens) is available for supported models.
    ApiKey,
}

/// Model selection agent type for per-agent model configuration.
/// Note: This is distinct from mission::AgentType which classifies task types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAgentType {
    /// Task execution agent (default workhorse)
    Task,
    /// Planning and specification agent (complex reasoning)
    Planning,
    /// Validation and review agent (quality checks)
    Validation,
    /// Evidence gathering agent (codebase analysis)
    Evidence,
    /// Retry analysis agent (failure diagnosis)
    Retry,
    /// Learning extraction agent (post-execution analysis)
    Learning,
}

/// Per-agent model configuration.
/// Allows different models for different agent types based on their requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentModelsConfig {
    /// Default model used when no specific model is configured for an agent type.
    pub default: String,
    /// Model for task execution (balanced speed/quality).
    pub task: Option<String>,
    /// Model for planning/specification (complex reasoning, may use more powerful model).
    pub planning: Option<String>,
    /// Model for validation/review (can use faster model for simple checks).
    pub validation: Option<String>,
    /// Model for evidence gathering (codebase analysis).
    pub evidence: Option<String>,
    /// Model for retry analysis (failure diagnosis).
    pub retry: Option<String>,
    /// Model for learning extraction (post-execution analysis).
    pub learning: Option<String>,
}

impl Default for AgentModelsConfig {
    fn default() -> Self {
        Self {
            default: String::from(DEFAULT_MODEL),
            task: None,       // Uses default
            planning: None,   // Uses default (consider opus for complex projects)
            validation: None, // Uses default (consider haiku for simple checks)
            evidence: None,   // Uses default
            retry: None,      // Uses default
            learning: None,   // Uses default
        }
    }
}

impl AgentModelsConfig {
    /// Get the model name for a specific agent type.
    /// Falls back to default if no specific model is configured.
    pub fn model_for(&self, agent_type: ModelAgentType) -> &str {
        let specific = match agent_type {
            ModelAgentType::Task => &self.task,
            ModelAgentType::Planning => &self.planning,
            ModelAgentType::Validation => &self.validation,
            ModelAgentType::Evidence => &self.evidence,
            ModelAgentType::Retry => &self.retry,
            ModelAgentType::Learning => &self.learning,
        };
        specific.as_deref().unwrap_or(&self.default)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Default model. Use `models` for per-agent configuration.
    pub model: String,
    /// Per-agent model configuration. Overrides `model` when specific agent model is set.
    pub models: AgentModelsConfig,
    /// Authentication mode: oauth (default) or api_key.
    /// IMPORTANT: In OAuth mode, extended_context is ignored (200k limit applies).
    pub auth_mode: AuthMode,
    /// Enable extended context window (1M tokens) for supported models.
    /// NOTE: Only effective when auth_mode = api_key. Ignored in OAuth mode.
    pub extended_context: bool,
    pub max_retries: u32,
    pub timeout_secs: u64,
    pub planning_timeout_secs: u64,
    pub chunk_timeout_secs: u64,
    pub subprocess_timeout_secs: u64,
    pub complex_task_timeout_multiplier: f32,
    pub max_sdk_iterations: u32,
    #[serde(default)]
    pub plugin_dirs: Vec<String>,
}

impl AgentConfig {
    /// Returns the effective ModelConfig for the default model based on auth_mode.
    /// In OAuth mode, extended_context is always disabled (200k limit).
    /// For per-agent model config, use `model_config_for(agent_type)`.
    pub fn model_config(&self) -> crate::error::Result<ModelConfig> {
        self.model_config_for_name(self.effective_default_model())
    }

    /// Returns the effective ModelConfig for a specific agent type.
    /// Uses per-agent model if configured, otherwise falls back to default.
    pub fn model_config_for(
        &self,
        agent_type: ModelAgentType,
    ) -> crate::error::Result<ModelConfig> {
        let model_name = self.models.model_for(agent_type);
        self.model_config_for_name(model_name)
    }

    /// Internal helper to create ModelConfig with auth_mode constraints.
    fn model_config_for_name(&self, model_name: &str) -> crate::error::Result<ModelConfig> {
        let effective_extended = match self.auth_mode {
            AuthMode::Oauth => false, // OAuth doesn't support extended context
            AuthMode::ApiKey => self.extended_context,
        };
        Ok(
            ModelConfig::from_name_or_default(model_name)?
                .with_extended_context(effective_extended),
        )
    }

    /// Returns the effective default model name.
    /// Uses `models.default` if set, otherwise uses `model`.
    pub fn effective_default_model(&self) -> &str {
        if !self.models.default.is_empty() {
            &self.models.default
        } else {
            &self.model
        }
    }

    /// Returns whether extended context is effectively enabled.
    pub fn effective_extended_context(&self) -> bool {
        self.auth_mode == AuthMode::ApiKey && self.extended_context
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::from(DEFAULT_MODEL),
            models: AgentModelsConfig::default(),
            auth_mode: AuthMode::Oauth, // Default: OAuth via Claude Code CLI
            extended_context: false,    // Disabled by default (and ignored in OAuth anyway)
            max_retries: 3,
            timeout_secs: 600,
            planning_timeout_secs: 0,
            chunk_timeout_secs: 180,
            subprocess_timeout_secs: 30,
            complex_task_timeout_multiplier: 1.5,
            max_sdk_iterations: 100,
            // Plugin directories searched in order:
            // 1. Project-level claudegen plugins (standard output location)
            // 2. Project-local plugins directory
            // 3. SDK default (~/.claude/plugins/) is added automatically by SDK
            plugin_dirs: vec![
                ".".to_string(), // Project root for {project}-plugin/ directories
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    /// Enable automatic learning extraction after mission completion
    pub auto_extract: bool,
    /// Extract reusable skills (patterns, techniques)
    pub extract_skills: bool,
    /// Extract coding rules (conventions, standards)
    pub extract_rules: bool,
    /// Extract specialized agents (experimental, requires vetting)
    pub extract_agents: bool,
    /// Minimum confidence threshold for extraction (0.0 - 1.0)
    pub min_confidence: f32,
    /// Retry history configuration for learning from past retry attempts
    pub retry_history: RetryHistoryConfig,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            auto_extract: false,
            extract_skills: true,
            extract_rules: true,
            extract_agents: false,
            min_confidence: 0.6,
            retry_history: RetryHistoryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryHistoryConfig {
    /// Enable retry history tracking and learning
    pub enabled: bool,
    /// Retention period in days for retry history entries
    pub retention_days: i64,
    /// Minimum sample size before using learned strategies
    pub min_samples_for_learning: usize,
    /// Minimum success rate threshold for strategy recommendation
    pub min_success_rate: f32,
}

impl Default for RetryHistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
            min_samples_for_learning: 2,
            min_success_rate: 0.5,
        }
    }
}

impl LearningConfig {
    /// Filter extraction result based on config flags and confidence threshold
    pub fn filter(
        &self,
        result: crate::learning::ExtractionResult,
    ) -> crate::learning::ExtractionResult {
        crate::learning::ExtractionResult {
            skills: if self.extract_skills {
                result
                    .skills
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
            rules: if self.extract_rules {
                result
                    .rules
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
            agents: if self.extract_agents {
                result
                    .agents
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}
