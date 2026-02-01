use std::path::Path;
use std::time::Duration;

use claude_agent::{Agent, PermissionMode, ToolAccess};
use tracing::{debug, info, warn};

use super::{ClaudeCodeWriter, ExtractionCandidate, ExtractionResult};
use crate::config::{AgentConfig, ModelAgentType, ProjectPaths};
use crate::error::{PilotError, Result};
use crate::git::GitRunner;
use crate::mission::Mission;

pub struct LearningExtractor {
    writer: ClaudeCodeWriter,
    agent_config: AgentConfig,
}

impl LearningExtractor {
    pub fn new(paths: ProjectPaths, agent_config: &AgentConfig) -> Self {
        Self {
            writer: ClaudeCodeWriter::new(paths),
            agent_config: agent_config.clone(),
        }
    }

    pub async fn extract(&self, mission: &Mission, working_dir: &Path) -> Result<ExtractionResult> {
        info!(mission_id = %mission.id, "Extracting learnings from mission");

        let mission_data = self.collect_mission_data(mission, working_dir).await?;
        let prompt = self.build_extraction_prompt(&mission_data);

        debug!(prompt_length = prompt.len(), "Built extraction prompt");

        // Use per-agent model configuration for learning extraction
        let model_config = self
            .agent_config
            .model_config_for(ModelAgentType::Learning)?;

        // Use structured output for type-safe extraction
        // This eliminates YAML parsing and provides API-level validation
        let plugin_dirs: Vec<std::path::PathBuf> = self
            .agent_config
            .plugin_dirs
            .iter()
            .map(|p| working_dir.join(p))
            .collect();

        let agent = Agent::builder()
            .from_claude_code(working_dir)
            .await
            .map_err(|e| PilotError::Learning(format!("Claude Code init failed: {}", e)))?
            .plugin_dirs(plugin_dirs)
            .model(model_config.model_id())
            .extended_context(model_config.has_extended_context())
            .with_user_resources()
            .with_project_resources()
            .tools(ToolAccess::none()) // No tools needed for extraction
            .timeout(Duration::from_secs(self.agent_config.timeout_secs))
            .max_iterations(1) // Single pass extraction
            .permission_mode(PermissionMode::AcceptEdits)
            .structured_output::<ExtractionResponse>()
            .build()
            .await
            .map_err(|e| PilotError::Learning(format!("Failed to build agent: {}", e)))?;

        let result = agent
            .execute(&prompt)
            .await
            .map_err(|e| PilotError::Learning(format!("SDK execution failed: {}", e)))?;

        debug!(
            input_tokens = result.usage.input_tokens,
            output_tokens = result.usage.output_tokens,
            "Extraction prompt completed"
        );

        let response: ExtractionResponse = result.extract().map_err(|e| {
            warn!(error = %e, "Failed to extract structured response, returning empty result");
            PilotError::Learning(format!("Failed to extract response: {}", e))
        })?;

        let candidates = self.build_result(response, &mission.id);

        info!(
            skills = candidates.skills.len(),
            rules = candidates.rules.len(),
            agents = candidates.agents.len(),
            "Extraction complete"
        );

        Ok(candidates)
    }

    pub async fn apply(
        &self,
        result: &ExtractionResult,
        approved: &[String],
    ) -> Result<Vec<String>> {
        let mut written = Vec::new();

        for candidate in result
            .skills
            .iter()
            .chain(result.rules.iter())
            .chain(result.agents.iter())
        {
            if approved.contains(&candidate.name) {
                let path = self.writer.write(candidate).await?;
                info!(
                    name = %candidate.name,
                    candidate_type = %candidate.candidate_type,
                    path = %path,
                    "Wrote extraction"
                );
                written.push(path);
            }
        }

        Ok(written)
    }

    async fn collect_mission_data(
        &self,
        mission: &Mission,
        working_dir: &Path,
    ) -> Result<MissionData> {
        debug!(mission_id = %mission.id, "Collecting mission data");

        let git = GitRunner::new(working_dir);
        let log = git.log(&mission.base_branch, 50).await?;
        let commits: Vec<String> = log.lines().map(String::from).collect();
        let diff = git.diff_stat(&mission.base_branch).await?;

        let learnings = mission
            .learnings
            .iter()
            .map(|l| format!("[{}] {}", l.category, l.content))
            .collect();

        let task_summaries = mission
            .tasks
            .iter()
            .filter(|t| t.result.is_some())
            .map(|t| {
                format!(
                    "Task {}: {} -> {}",
                    t.id,
                    t.description,
                    t.result
                        .as_ref()
                        .map(|r| &r.output)
                        .unwrap_or(&String::new())
                )
            })
            .collect();

        let affected_files: Vec<String> = mission
            .tasks
            .iter()
            .flat_map(|t| t.affected_files.iter().cloned())
            .collect();

        Ok(MissionData {
            description: mission.description.clone(),
            commits,
            diff,
            learnings,
            task_summaries,
            affected_files,
        })
    }

    fn build_extraction_prompt(&self, data: &MissionData) -> String {
        format!(
            r#"## Learning Extraction

Analyze this completed mission and extract reusable knowledge for Claude Code.

**Mission:** {}

### Commits
{}

### Changes
{}

### Affected Files
{}

### Learnings Recorded
{}

### Task Summaries
{}

---

Extract reusable knowledge following Claude Code's format:

1. **Skills**: Reusable patterns (stored in `.claude/skills/<name>/SKILL.md`)
   - Required: name, description
   - Optional: allowed-tools (e.g., "Read, Grep, Glob")

2. **Rules**: Coding conventions (stored in `.claude/rules/<name>.md`)
   - Include `paths` for path-specific rules (glob patterns like "src/**/*.ts")
   - Rules without paths apply globally

3. **Agents**: Specialized roles (stored in `.claude/agents/<name>.md`)
   - Required: name, description, tools
   - Optional: model (sonnet/opus/haiku/inherit), permissionMode

Output in YAML:
```yaml
skills:
  - name: skill-name
    description: "What it does and when to use it"
    content: |
      Detailed instructions...
    tools: ["Read", "Grep", "Glob"]
    confidence: 0.8

rules:
  - name: rule-name
    description: "Brief description"
    content: |
      Rule details...
    paths: ["src/**/*.ts", "lib/**/*.ts"]
    confidence: 0.7

agents:
  - name: agent-name
    description: "When Claude should delegate to this agent"
    content: |
      Agent instructions...
    tools: ["Read", "Grep", "Glob", "Bash"]
    model: inherit
    confidence: 0.6
```

Only include high-confidence (>0.6) extractions that would be genuinely useful.
Follow progressive disclosure: keep main content concise, link to details if needed.
"#,
            data.description,
            data.commits.join("\n"),
            data.diff,
            data.affected_files.join("\n"),
            data.learnings.join("\n"),
            data.task_summaries.join("\n")
        )
    }

    /// Convert structured response to extraction result.
    fn build_result(&self, response: ExtractionResponse, mission_id: &str) -> ExtractionResult {
        let skills = response
            .skills
            .into_iter()
            .map(|s| {
                ExtractionCandidate::skill(&s.name, &s.description)
                    .with_content(&s.content)
                    .with_confidence(s.confidence)
                    .with_source_tasks(vec![mission_id.to_string()])
                    .with_tools(s.tools)
            })
            .collect();

        let rules = response
            .rules
            .into_iter()
            .map(|r| {
                ExtractionCandidate::rule(&r.name, &r.description)
                    .with_content(&r.content)
                    .with_confidence(r.confidence)
                    .with_source_tasks(vec![mission_id.to_string()])
                    .with_paths(r.paths)
            })
            .collect();

        let agents = response
            .agents
            .into_iter()
            .map(|a| {
                let mut candidate = ExtractionCandidate::agent(&a.name, &a.description)
                    .with_content(&a.content)
                    .with_confidence(a.confidence)
                    .with_source_tasks(vec![mission_id.to_string()])
                    .with_tools(a.tools);

                if let Some(model) = a.model {
                    candidate = candidate.with_model(model);
                }

                candidate
            })
            .collect();

        ExtractionResult {
            skills,
            rules,
            agents,
        }
    }
}

struct MissionData {
    description: String,
    commits: Vec<String>,
    diff: String,
    learnings: Vec<String>,
    task_summaries: Vec<String>,
    affected_files: Vec<String>,
}

/// Structured response for learning extraction.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractionResponse {
    /// Extracted skills (reusable patterns)
    #[serde(default)]
    pub skills: Vec<ExtractedSkill>,
    /// Extracted rules (coding conventions)
    #[serde(default)]
    pub rules: Vec<ExtractedRule>,
    /// Extracted agents (specialized roles)
    #[serde(default)]
    pub agents: Vec<ExtractedAgent>,
}

/// Extracted skill definition.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractedSkill {
    /// Unique skill name (kebab-case)
    pub name: String,
    /// What this skill does and when to use it
    pub description: String,
    /// Detailed skill instructions
    #[serde(default)]
    pub content: String,
    /// Allowed tools for this skill
    #[serde(default)]
    pub tools: Vec<String>,
    /// Confidence score (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// Extracted rule definition.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractedRule {
    /// Unique rule name (kebab-case)
    pub name: String,
    /// Brief description of the rule
    pub description: String,
    /// Rule details and examples
    #[serde(default)]
    pub content: String,
    /// Path patterns this rule applies to (glob patterns)
    #[serde(default)]
    pub paths: Vec<String>,
    /// Confidence score (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// Extracted agent definition.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractedAgent {
    /// Unique agent name (kebab-case)
    pub name: String,
    /// When Claude should delegate to this agent
    pub description: String,
    /// Agent instructions
    #[serde(default)]
    pub content: String,
    /// Tools available to this agent
    #[serde(default)]
    pub tools: Vec<String>,
    /// Model to use (sonnet/opus/haiku/inherit)
    #[serde(default)]
    pub model: Option<String>,
    /// Confidence score (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    0.5
}
