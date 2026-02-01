//! Task execution agent with context-aware resource loading.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use claude_agent::{Agent as SdkAgent, AgentEvent, PermissionMode, ToolAccess};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tracing::{debug, info, warn};

use super::PromptBuilder;
use super::executor::LlmExecutor;
use super::multi::PermissionProfile;
use crate::config::{
    AgentConfig, BuildSystem, DisplayConfig, ModelAgentType, TaskBudgetConfig, VerificationConfig,
};
use crate::error::{PilotError, Result};
use crate::mission::{Mission, Task, TaskResult};

/// Resource loading strategy for agent execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceLoadingMode {
    /// Load all SDK resources (CLAUDE.md, skills, rules).
    /// Use for: Architect agents, standalone execution, mission tasks.
    #[default]
    Full,
    /// Skip SDK resource loading, use Coordinator-provided context only.
    /// Use for: Module agents in multi-agent orchestration.
    Composed,
}

pub struct TaskAgent {
    config: AgentConfig,
    prompt_builder: PromptBuilder,
    allowed_bash_prefixes: Option<Vec<String>>,
}

impl TaskAgent {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            prompt_builder: PromptBuilder::default(),
            allowed_bash_prefixes: None,
        }
    }

    pub fn with_verification(
        config: AgentConfig,
        task_budget: &TaskBudgetConfig,
        display_config: &DisplayConfig,
        verification: &VerificationConfig,
        working_dir: &Path,
    ) -> Self {
        Self {
            config,
            prompt_builder: PromptBuilder::new(task_budget, display_config),
            allowed_bash_prefixes: Some(verification.resolve_allowed_bash_prefixes(working_dir)),
        }
    }

    fn calculate_timeout(&self, task: &Task) -> Duration {
        let base = Duration::from_secs(self.config.timeout_secs);
        let adjusted = if task.dependencies.len() > 5 {
            Duration::from_secs_f32(
                base.as_secs_f32() * self.config.complex_task_timeout_multiplier,
            )
        } else {
            base
        };
        Duration::from_secs_f32(adjusted.as_secs_f32() * task.scope_factor)
    }

    fn resolve_plugin_dirs(&self, working_dir: &Path) -> Vec<std::path::PathBuf> {
        self.config
            .plugin_dirs
            .iter()
            .map(|p| working_dir.join(p))
            .collect()
    }

    fn default_timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs)
    }

    fn chunk_timeout(&self) -> Duration {
        Duration::from_secs(self.config.chunk_timeout_secs)
    }

    fn resolve_bash_prefixes(&self, working_dir: &Path) -> Vec<String> {
        if let Some(ref prefixes) = self.allowed_bash_prefixes {
            return prefixes.clone();
        }
        // Fallback: auto-detect from build system
        BuildSystem::detect(working_dir)
            .map(|bs| {
                bs.allowed_bash_prefixes()
                    .into_iter()
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_else(|| {
                BuildSystem::default_common_prefixes()
                    .into_iter()
                    .map(String::from)
                    .collect()
            })
    }

    async fn build_agent(
        &self,
        working_dir: &Path,
        timeout: Duration,
        profile: PermissionProfile,
        system_prompt: Option<&str>,
        loading_mode: ResourceLoadingMode,
    ) -> Result<SdkAgent> {
        let model_config = self.config.model_config_for(ModelAgentType::Task)?;

        let (tools, permission_mode) = match profile {
            PermissionProfile::ReadOnly => (
                ToolAccess::only(["Read", "Glob", "Grep", "WebSearch", "WebFetch"]),
                PermissionMode::Plan,
            ),
            PermissionProfile::FileModify => (
                ToolAccess::except(["Task", "TaskOutput", "Plan"]),
                PermissionMode::AcceptEdits,
            ),
            PermissionProfile::VerifyExecute => (
                ToolAccess::only(["Read", "Glob", "Grep", "Bash"]),
                PermissionMode::Default,
            ),
        };

        let base_builder = SdkAgent::builder()
            .from_claude_code(working_dir)
            .await
            .map_err(|e| PilotError::Config(format!("Claude Code init failed: {}", e)))?;

        let mut builder = match loading_mode {
            ResourceLoadingMode::Full => base_builder.with_project_resources(),
            ResourceLoadingMode::Composed => base_builder,
        };

        builder = builder
            .plugin_dirs(self.resolve_plugin_dirs(working_dir))
            .model(model_config.model_id())
            .extended_context(model_config.has_extended_context())
            .tools(tools)
            .timeout(timeout)
            .chunk_timeout(self.chunk_timeout())
            .max_iterations(self.config.max_sdk_iterations as usize)
            .permission_mode(permission_mode);

        if profile == PermissionProfile::VerifyExecute {
            for prefix in self.resolve_bash_prefixes(working_dir) {
                builder = builder.allow_tool(format!("Bash({}:*)", prefix));
            }
            builder = builder
                .allow_tool("Read")
                .allow_tool("Glob")
                .allow_tool("Grep");
        }

        if let Some(prompt) = system_prompt {
            builder = builder.append_system_prompt(prompt);
        }

        builder
            .build()
            .await
            .map_err(|e| PilotError::Config(format!("Failed to build agent: {}", e)))
    }

    async fn build_agent_structured<T>(
        &self,
        working_dir: &Path,
        timeout: Duration,
        loading_mode: ResourceLoadingMode,
    ) -> Result<SdkAgent>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let model_config = self.config.model_config_for(ModelAgentType::Task)?;

        let base_builder = SdkAgent::builder()
            .from_claude_code(working_dir)
            .await
            .map_err(|e| PilotError::Config(format!("Claude Code init failed: {}", e)))?;

        let builder = match loading_mode {
            ResourceLoadingMode::Full => base_builder.with_project_resources(),
            ResourceLoadingMode::Composed => base_builder,
        };

        builder
            .plugin_dirs(self.resolve_plugin_dirs(working_dir))
            .model(model_config.model_id())
            .extended_context(model_config.has_extended_context())
            .tools(ToolAccess::none())
            .structured_output::<T>()
            .timeout(timeout)
            .chunk_timeout(self.chunk_timeout())
            .max_iterations(1)
            .permission_mode(PermissionMode::AcceptEdits)
            .build()
            .await
            .map_err(|e| PilotError::Config(format!("Failed to build agent: {}", e)))
    }

    async fn build_agent_review<T>(
        &self,
        working_dir: &Path,
        timeout: Duration,
        loading_mode: ResourceLoadingMode,
    ) -> Result<SdkAgent>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let model_config = self.config.model_config_for(ModelAgentType::Validation)?;

        let base_builder = SdkAgent::builder()
            .from_claude_code(working_dir)
            .await
            .map_err(|e| PilotError::Config(format!("Claude Code init failed: {}", e)))?;

        let builder = match loading_mode {
            ResourceLoadingMode::Full => base_builder.with_project_resources(),
            ResourceLoadingMode::Composed => base_builder,
        };

        builder
            .plugin_dirs(self.resolve_plugin_dirs(working_dir))
            .model(model_config.model_id())
            .extended_context(model_config.has_extended_context())
            .tools(ToolAccess::only(["Read", "Glob", "Grep"]))
            .structured_output::<T>()
            .timeout(timeout)
            .chunk_timeout(self.chunk_timeout())
            .max_iterations(self.config.max_sdk_iterations as usize)
            .permission_mode(PermissionMode::Plan)
            .build()
            .await
            .map_err(|e| PilotError::Config(format!("Failed to build review agent: {}", e)))
    }

    async fn execute_agent(&self, agent: SdkAgent, prompt: &str) -> Result<TaskResult> {
        let stream = agent
            .execute_stream(prompt)
            .await
            .map_err(|e| PilotError::AgentExecution(format!("Stream error: {}", e)))?;

        let mut stream = std::pin::pin!(stream);
        let mut final_result = None;
        let mut collected_text = String::new();

        while let Some(event) = stream.next().await {
            match event {
                Ok(AgentEvent::Text(text)) => collected_text.push_str(&text),
                Ok(AgentEvent::ToolComplete { name, is_error, .. }) => {
                    debug!(tool = %name, is_error, "Tool completed");
                }
                Ok(AgentEvent::Complete(result)) => final_result = Some(*result),
                Ok(AgentEvent::ToolBlocked { name, reason, .. }) => {
                    warn!(tool = %name, reason = %reason, "Tool blocked");
                }
                Ok(AgentEvent::Thinking(_) | AgentEvent::ContextUpdate { .. }) => {}
                Err(e) => {
                    warn!(error = %e, "Stream error");
                    return Err(PilotError::AgentExecution(format!("Stream error: {}", e)));
                }
            }
        }

        let result = final_result
            .ok_or_else(|| PilotError::AgentExecution("No completion result".into()))?;

        let output = if collected_text.is_empty() {
            result.text.clone()
        } else {
            collected_text
        };

        debug!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            "Execution completed"
        );

        Ok(TaskResult {
            success: true,
            output,
            files_modified: Vec::new(),
            files_created: Vec::new(),
            files_deleted: Vec::new(),
            input_tokens: Some(result.usage.input_tokens as u64),
            output_tokens: Some(result.usage.output_tokens as u64),
        })
    }

    async fn execute_agent_structured<T>(&self, agent: SdkAgent, prompt: &str) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let result = agent
            .execute(prompt)
            .await
            .map_err(|e| PilotError::AgentExecution(format!("Execution failed: {}", e)))?;

        debug!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            has_structured_output = result.structured_output.is_some(),
            "Structured execution completed"
        );

        result.extract::<T>().map_err(|e| {
            PilotError::AgentExecution(format!(
                "Failed to extract structured output: {}. Text: {}",
                e,
                result.text.chars().take(500).collect::<String>()
            ))
        })
    }

    /// Execute with Full resource loading (SDK loads all project resources).
    pub async fn run_with_profile(
        &self,
        prompt: &str,
        working_dir: &Path,
        profile: PermissionProfile,
    ) -> Result<String> {
        self.run_internal(prompt, working_dir, profile, ResourceLoadingMode::Full)
            .await
    }

    /// Execute with Composed context only (no SDK resource loading).
    /// Use for module agents where Coordinator provides the context.
    pub async fn run_with_composed_context(
        &self,
        prompt: &str,
        working_dir: &Path,
        profile: PermissionProfile,
    ) -> Result<String> {
        self.run_composed_internal(prompt, working_dir, profile, self.default_timeout())
            .await
    }

    /// Execute with Composed context and custom timeout.
    pub async fn run_with_composed_context_timeout(
        &self,
        prompt: &str,
        working_dir: &Path,
        profile: PermissionProfile,
        timeout: Duration,
    ) -> Result<String> {
        self.run_composed_internal(prompt, working_dir, profile, timeout)
            .await
    }

    async fn run_composed_internal(
        &self,
        prompt: &str,
        working_dir: &Path,
        profile: PermissionProfile,
        timeout: Duration,
    ) -> Result<String> {
        let agent = self
            .build_agent(
                working_dir,
                timeout,
                profile,
                None,
                ResourceLoadingMode::Composed,
            )
            .await?;
        let result = self.execute_agent(agent, prompt).await?;
        Ok(result.output)
    }

    async fn run_internal(
        &self,
        prompt: &str,
        working_dir: &Path,
        profile: PermissionProfile,
        loading_mode: ResourceLoadingMode,
    ) -> Result<String> {
        let agent = self
            .build_agent(
                working_dir,
                self.default_timeout(),
                profile,
                None,
                loading_mode,
            )
            .await?;
        let result = self.execute_agent(agent, prompt).await?;
        Ok(result.output)
    }

    pub async fn run_prompt(&self, prompt: &str, working_dir: &Path) -> Result<String> {
        self.run_with_profile(prompt, working_dir, PermissionProfile::FileModify)
            .await
    }

    pub async fn run_prompt_structured<T>(&self, prompt: &str, working_dir: &Path) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let agent = self
            .build_agent_structured::<T>(
                working_dir,
                self.default_timeout(),
                ResourceLoadingMode::Composed,
            )
            .await?;
        self.execute_agent_structured(agent, prompt).await
    }

    pub async fn run_prompt_review<T>(&self, prompt: &str, working_dir: &Path) -> Result<T>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        let agent = self
            .build_agent_review::<T>(
                working_dir,
                self.default_timeout(),
                ResourceLoadingMode::Composed,
            )
            .await?;
        self.execute_agent_structured(agent, prompt).await
    }

    pub async fn execute_task(
        &self,
        mission: &Mission,
        task: &Task,
        working_dir: &Path,
    ) -> Result<TaskResult> {
        info!(mission_id = %mission.id, task_id = %task.id, "Executing task");

        let timeout = self.calculate_timeout(task);
        let system_prompt = self.prompt_builder.build_mission_system_prompt(mission);
        let task_message = self.prompt_builder.build_task_message(mission, task, None);

        let agent = self
            .build_agent(
                working_dir,
                timeout,
                PermissionProfile::FileModify,
                Some(&system_prompt),
                ResourceLoadingMode::Full,
            )
            .await?;

        self.execute_agent(agent, &task_message).await
    }

    pub async fn run_qa(&self, mission: &Mission, working_dir: &Path) -> Result<TaskResult> {
        info!(mission_id = %mission.id, "Running QA");

        let system_prompt = self.prompt_builder.build_mission_system_prompt(mission);
        let qa_prompt = self.prompt_builder.build_qa_prompt(mission);

        let agent = self
            .build_agent(
                working_dir,
                self.default_timeout(),
                PermissionProfile::FileModify,
                Some(&system_prompt),
                ResourceLoadingMode::Full,
            )
            .await?;

        self.execute_agent(agent, &qa_prompt).await
    }
}

impl LlmExecutor for TaskAgent {
    fn execute<'a>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.run_prompt(prompt, working_dir))
    }

    fn execute_structured<'a, T>(
        &'a self,
        prompt: &'a str,
        working_dir: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>
    where
        T: JsonSchema + DeserializeOwned + Send + 'static,
    {
        Box::pin(self.run_prompt_structured(prompt, working_dir))
    }
}
