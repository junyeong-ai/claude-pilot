use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use claude_agent::{Agent as SdkAgent, PermissionMode, ToolAccess};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tracing::{debug, info, warn};

use crate::agent::LlmExecutor;
use crate::config::{AgentConfig, ModelAgentType};
use crate::context::{ContextEstimator, LoadingRecommendation};
use crate::error::{PilotError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceStrategy {
    Full,
    Minimal,
}

pub struct PlanAgent {
    working_dir: std::path::PathBuf,
    model: String,
    timeout_secs: u64,
    default_timeout_secs: u64,
    max_iterations: u32,
    context_window: u64,
    extended_context: bool,
    plugin_dirs: Vec<std::path::PathBuf>,
}

impl PlanAgent {
    pub fn new(working_dir: &Path, config: &AgentConfig) -> crate::error::Result<Self> {
        let model_config = config.model_config_for(ModelAgentType::Planning)?;

        if model_config.has_extended_context() {
            info!(
                model = %model_config.model_id(),
                context_window = model_config.context_window(),
                "Extended context enabled for planning"
            );
        }

        Ok(Self {
            working_dir: working_dir.to_path_buf(),
            model: model_config.model_id().to_string(),
            timeout_secs: config.planning_timeout_secs,
            default_timeout_secs: config.timeout_secs,
            max_iterations: config.max_sdk_iterations,
            context_window: model_config.context_window(),
            extended_context: model_config.has_extended_context(),
            plugin_dirs: config
                .plugin_dirs
                .iter()
                .map(|p| working_dir.join(p))
                .collect(),
        })
    }

    pub fn context_window(&self) -> u64 {
        self.context_window
    }

    pub fn has_extended_context(&self) -> bool {
        self.extended_context
    }

    pub async fn estimate_strategy(&self) -> ResourceStrategy {
        let estimator = ContextEstimator::new(self.context_window as usize);
        let estimate = estimator.estimate(&self.working_dir).await;

        info!("{}", estimate.summary());

        match estimate.recommendation {
            LoadingRecommendation::LoadAll => ResourceStrategy::Full,
            _ => {
                warn!(
                    mcp_tokens = estimate.mcp_resources_total(),
                    available = estimate.available_budget,
                    "MCP resources too large, using minimal strategy"
                );
                ResourceStrategy::Minimal
            }
        }
    }

    pub async fn run(&self, prompt: &str) -> Result<String> {
        let strategy = self.estimate_strategy().await;

        match self.run_with_strategy(prompt, strategy).await {
            Ok(result) => Ok(result),
            Err(e) if is_token_overflow_error(&e) && strategy == ResourceStrategy::Full => {
                warn!("Token overflow detected, retrying with minimal strategy");
                self.run_with_strategy(prompt, ResourceStrategy::Minimal)
                    .await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn run_structured<T>(&self, prompt: &str) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let strategy = self.estimate_strategy().await;

        match self
            .run_structured_with_strategy::<T>(prompt, strategy)
            .await
        {
            Ok(result) => Ok(result),
            Err(e) if is_token_overflow_error(&e) && strategy == ResourceStrategy::Full => {
                warn!("Token overflow detected, retrying with minimal strategy");
                self.run_structured_with_strategy::<T>(prompt, ResourceStrategy::Minimal)
                    .await
            }
            Err(e) => Err(e),
        }
    }

    async fn run_structured_with_strategy<T>(
        &self,
        prompt: &str,
        strategy: ResourceStrategy,
    ) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let timeout = self.effective_timeout();

        debug!(
            timeout_secs = timeout.as_secs(),
            working_dir = %self.working_dir.display(),
            strategy = ?strategy,
            type_name = std::any::type_name::<T>(),
            prompt_length = prompt.len(),
            "Running structured output prompt"
        );
        // Prompt content at trace level to avoid sensitive data in production logs
        tracing::trace!(prompt_preview = %prompt.chars().take(200).collect::<String>());

        let planning_tools =
            ToolAccess::except(["Write", "Edit", "Plan", "TodoWrite", "KillShell"]);

        let base_builder = SdkAgent::builder()
            .from_claude_code(&self.working_dir)
            .await
            .map_err(|e| PilotError::Planning(format!("Claude Code init failed: {}", e)))?
            .plugin_dirs(self.plugin_dirs.clone())
            .model(&self.model)
            .extended_context(self.extended_context)
            .structured_output::<T>();

        let builder = match strategy {
            ResourceStrategy::Full => base_builder
                .with_user_resources()
                .with_project_resources()
                .with_local_resources(),
            ResourceStrategy::Minimal => base_builder,
        };

        let agent = builder
            .tools(planning_tools)
            .timeout(timeout)
            .max_iterations(self.max_iterations as usize)
            .permission_mode(PermissionMode::AcceptEdits)
            .build()
            .await
            .map_err(|e| PilotError::Planning(format!("Failed to build agent: {}", e)))?;

        let result = agent
            .execute(prompt)
            .await
            .map_err(|e| PilotError::Planning(format!("Execution failed: {}", e)))?;

        debug!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            strategy = ?strategy,
            has_structured_output = result.structured_output.is_some(),
            text_length = result.text.len(),
            iterations = result.iterations,
            tool_calls = result.tool_calls,
            stop_reason = ?result.stop_reason,
            "Structured output completed"
        );

        result.extract::<T>().map_err(|e| {
            PilotError::Planning(format!(
                "Failed to extract structured output: {}. Text: {}",
                e,
                result.text.chars().take(500).collect::<String>()
            ))
        })
    }

    async fn run_with_strategy(&self, prompt: &str, strategy: ResourceStrategy) -> Result<String> {
        let timeout = self.effective_timeout();

        debug!(
            timeout_secs = timeout.as_secs(),
            working_dir = %self.working_dir.display(),
            strategy = ?strategy,
            extended_context = self.extended_context,
            context_window = self.context_window,
            "Running planning prompt"
        );

        let planning_tools =
            ToolAccess::except(["Write", "Edit", "Plan", "TodoWrite", "KillShell"]);

        let base_builder = SdkAgent::builder()
            .from_claude_code(&self.working_dir)
            .await
            .map_err(|e| PilotError::Planning(format!("Claude Code init failed: {}", e)))?
            .plugin_dirs(self.plugin_dirs.clone())
            .model(&self.model)
            .extended_context(self.extended_context);

        let builder = match strategy {
            ResourceStrategy::Full => base_builder
                .with_user_resources()
                .with_project_resources()
                .with_local_resources(),
            ResourceStrategy::Minimal => base_builder,
        };

        let agent = builder
            .tools(planning_tools)
            .timeout(timeout)
            .max_iterations(self.max_iterations as usize)
            .permission_mode(PermissionMode::AcceptEdits)
            .build()
            .await
            .map_err(|e| PilotError::Planning(format!("Failed to build agent: {}", e)))?;

        let result = agent
            .execute(prompt)
            .await
            .map_err(|e| PilotError::Planning(format!("Execution failed: {}", e)))?;

        debug!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            strategy = ?strategy,
            "Planning completed"
        );

        Ok(result.text)
    }

    fn effective_timeout(&self) -> Duration {
        if self.timeout_secs > 0 {
            Duration::from_secs(self.timeout_secs)
        } else {
            Duration::from_secs(self.default_timeout_secs)
        }
    }
}

fn is_token_overflow_error(error: &PilotError) -> bool {
    match error {
        PilotError::Planning(msg) => {
            msg.contains("prompt is too long") || (msg.contains("400") && msg.contains("token"))
        }
        _ => false,
    }
}

/// LlmExecutor implementation for PlanAgent.
/// Uses the Planning model for higher quality semantic judgments.
impl LlmExecutor for PlanAgent {
    fn execute<'a>(
        &'a self,
        prompt: &'a str,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move { self.run(prompt).await })
    }

    fn execute_structured<'a, T>(
        &'a self,
        prompt: &'a str,
        _working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        Box::pin(async move { self.run_structured(prompt).await })
    }
}
