//! Research agent with P2P evidence sharing via MessageBus.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::debug;

use super::messaging::{AgentMessage, AgentMessageBus, MessagePayload};
use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::config::ResearchConfig;
use crate::error::Result;

const RESEARCH_SYSTEM_PROMPT: &str = r"# Research Agent

You are a specialized research agent focused on evidence gathering and codebase analysis.

## Primary Responsibilities
- Analyze codebase structure and patterns
- Identify relevant files for tasks
- Discover dependencies and relationships
- Extract patterns and conventions
- Gather evidence for planning decisions

## Tools Preference
Prioritize these tools for efficient research:
- `symora search` for semantic code search
- `symora find refs` for finding usages
- `symora calls` for call hierarchy analysis
- Glob/Grep for pattern matching

## Output Format
Provide structured findings:
1. Relevant files with confidence scores
2. Dependencies and relationships
3. Patterns and conventions observed
4. Recommendations for implementation

## Constraints
- Do not modify any files
- Focus on gathering facts, not opinions
- Cite specific locations for all findings
";

pub struct ResearchAgent {
    core: AgentCore,
    config: ResearchConfig,
}

impl ResearchAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self::with_config(id, task_agent, ResearchConfig::default())
    }

    pub fn with_config(id: String, task_agent: Arc<TaskAgent>, config: ResearchConfig) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::core_research(), task_agent),
            config,
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id.to_string(), task_agent))
    }

    pub async fn execute_with_messaging(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        message_bus: &AgentMessageBus,
    ) -> Result<AgentTaskResult> {
        let result = self.execute(task, working_dir).await?;

        if result.is_success() && !result.findings.is_empty() {
            let quality = self.compute_findings_quality(&result.findings);
            self.broadcast_evidence(message_bus, &result.findings, quality)
                .await;
        }

        Ok(result)
    }

    async fn broadcast_evidence(&self, bus: &AgentMessageBus, findings: &[String], quality: f64) {
        let msg = AgentMessage::broadcast(
            self.id(),
            MessagePayload::EvidenceShare {
                evidence: findings.to_vec(),
                quality_score: quality,
            },
        );

        if let Err(e) = bus.send(msg).await {
            debug!(error = %e, "Failed to broadcast evidence");
        }
    }

    fn compute_findings_quality(&self, findings: &[String]) -> f64 {
        if findings.is_empty() {
            return 0.0;
        }

        let score_sum: u32 = findings.iter().map(|f| Self::score_finding(f)).sum();
        // Max possible per finding: file:line(80) + paths(40) + structured(15) + length(20) + code(10) = 165
        // Use 120 as practical max (unlikely to hit all bonuses simultaneously)
        let practical_max = findings.len() as f64 * 120.0;
        (score_sum as f64 / practical_max).min(1.0)
    }

    fn build_research_prompt(&self, task: &AgentTask) -> String {
        AgentPromptBuilder::new(RESEARCH_SYSTEM_PROMPT, "Research", task)
            .with_context(task)
            .with_related_files(task)
            .with_section(
                "Expected Output",
                &[
                    "Provide comprehensive evidence gathering results:",
                    "1. List all relevant files with confidence scores",
                    "2. Document dependencies and imports",
                    "3. Note patterns and conventions",
                    "4. Identify potential risks or blockers",
                ],
            )
            .build()
    }

    fn extract_findings(&self, output: &str) -> Vec<String> {
        let mut findings: Vec<(String, u32)> = output
            .split("\n\n")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|finding| {
                let score = Self::score_finding(&finding);
                (finding, score)
            })
            .collect();

        // Sort by score (descending) to prioritize more important findings
        findings.sort_by(|a, b| b.1.cmp(&a.1));

        findings
            .into_iter()
            .take(self.config.max_findings)
            .map(|(finding, _)| finding)
            .collect()
    }

    /// Scores a finding using language-agnostic structural signals.
    ///
    /// Avoids English keyword matching (CLAUDE.md: universal system requirement).
    /// Instead, detects structural patterns that indicate actionable, specific findings
    /// regardless of the language the LLM responds in.
    fn score_finding(finding: &str) -> u32 {
        let mut score = 0u32;

        // File:line references (e.g., "src/auth.rs:45", "lib/utils.py:120")
        // Pattern: path-like segment followed by `:` and digits
        let has_file_line_ref = finding.chars().enumerate().any(|(i, c)| {
            if c != ':' || i == 0 {
                return false;
            }
            if !finding[i + 1..].chars().next().is_some_and(|d| d.is_ascii_digit()) {
                return false;
            }
            let seg_start = finding[..i]
                .rfind(|ch: char| ch.is_whitespace() || ch == '(' || ch == '[' || ch == '`')
                .map_or(0, |pos| pos + 1);
            let segment = &finding[seg_start..i];
            (segment.contains('.') || segment.contains('/'))
                && !segment.starts_with("//")
                && !segment.contains("://")
        });
        if has_file_line_ref {
            score += 80;
        }

        // Path references (segments with `/` and `.` — file paths)
        let has_path_ref = finding.split_whitespace().any(|word| {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
            clean.contains('/') && clean.contains('.') && clean.len() > 3
        });
        if has_path_ref {
            score += 40;
        }

        // Structured format (bullets, numbered lists, markdown headers)
        let trimmed = finding.trim_start();
        if trimmed.starts_with('-')
            || trimmed.starts_with('*')
            || trimmed.starts_with('#')
            || trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
                && trimmed.chars().nth(1).is_some_and(|c| c == '.' || c == ')')
        {
            score += 15;
        }

        // Length-based scoring (longer = more detailed = more useful)
        if finding.len() > 200 {
            score += 20;
        } else if finding.len() > 100 {
            score += 10;
        } else if finding.len() < 30 {
            score = score.saturating_sub(20);
        }

        // Code-like content (indented lines with brackets/parentheses)
        let has_code_content = finding.lines().any(|line| {
            let indent = line.len() - line.trim_start().len();
            indent >= 2 && line.contains(['(', ')', '{', '}', '[', ']'])
        });
        if has_code_content {
            score += 10;
        }

        score
    }
}

#[async_trait]
impl SpecializedAgent for ResearchAgent {
    fn role(&self) -> &AgentRole {
        self.core.role()
    }

    fn id(&self) -> &str {
        self.core.id()
    }

    fn identifier(&self) -> AgentIdentifier {
        self.core.identifier().clone()
    }

    fn system_prompt(&self) -> &str {
        RESEARCH_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Research agent executing");

        let prompt = self.build_research_prompt(task);
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let output = self
            .core
            .task_agent
            .run_with_composed_context_timeout(
                &prompt,
                working_dir,
                self.role().permission_profile(),
                timeout,
            )
            .await;

        match output {
            Ok(output) => {
                let findings = self.extract_findings(&output);
                Ok(AgentTaskResult::success(&task.id, output.clone())
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("research-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Evidence,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_findings() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ResearchAgent::new("test".to_string(), task_agent);

        let output = r#"## Findings
- Found relevant file: src/auth.rs
* Discovered something in src/lib.rs
The code uses src/utils/helper.ts
"#;

        let findings = agent.extract_findings(output);
        assert!(!findings.is_empty());
    }

    #[test]
    fn test_findings_prioritization() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ResearchAgent::new("test".to_string(), task_agent);

        // Findings with varying structural signals
        let output = "This is a low priority finding without much detail.\n\n\
            The main entry point is src/auth.rs:45 with authentication logic that handles user sessions and token validation for the API layer.\n\n\
            - Found relevant file: src/utils/helper.rs with utility functions\n\n\
            src/main.rs:10 references src/config/settings.rs:25 and both files contain important initialization code for the application startup sequence.\n\n\
            Short\n\n\
            This is a detailed finding that explains the architecture.\n  fn handle_request(req: Request) {\n    validate(req.token)\n  }\nThe layered approach with clear separation of concerns ensures maintainability across the entire codebase including all modules.";

        let findings = agent.extract_findings(output);

        // Finding with file:line refs + long text should be first (80+40+10+20=150 or similar)
        assert!(
            findings[0].contains(":10") || findings[0].contains(":25") || findings[0].contains(":45"),
            "Top finding should contain file:line references"
        );

        // Very short finding should be deprioritized
        let short_pos = findings.iter().position(|f| f == "Short");
        assert!(short_pos.is_none() || short_pos.unwrap() > 3);
    }

    #[test]
    fn test_score_finding() {
        // File:line reference scores highest
        let file_line_score = ResearchAgent::score_finding(
            "Found issue at src/auth.rs:45 in the authentication module"
        );
        // Bullet with path reference
        let bullet_path_score = ResearchAgent::score_finding(
            "- Located in src/utils/helper.rs within the project"
        );
        // Short finding with no structural signals
        let short_score = ResearchAgent::score_finding("Short note");

        assert!(file_line_score > bullet_path_score, "file:line should score higher than path-only");
        assert!(bullet_path_score > short_score, "structured + path should score higher than short");

        // Code-like content with indentation
        let code_score = ResearchAgent::score_finding(
            "The function signature is:\n  fn process(items: Vec<String>) -> Result<()>\nThis handles the main processing pipeline and returns errors for invalid input."
        );
        assert!(code_score > short_score, "code content should score higher than short note");

        // Long detailed finding without file refs
        let long_score = ResearchAgent::score_finding(
            &"A".repeat(250)
        );
        assert_eq!(long_score, 20, "long text without structural signals gets length bonus only");

        // Comment-like paths must NOT score as file:line
        let comment_score = ResearchAgent::score_finding("//config.rs:10 is a comment");
        assert!(comment_score < 80, "comment-like path should not score as file:line, got {comment_score}");

        // URL ports must NOT score as file:line
        let url_score = ResearchAgent::score_finding(
            "Visit http://example.com:8080 for API details and more information about the endpoint"
        );
        assert!(url_score < 80, "URL:port should not score as file:line, got {url_score}");

        // https URLs must NOT score as file:line
        let https_score = ResearchAgent::score_finding(
            "Documentation at https://docs.rs:443/crate/tokio provides comprehensive API reference"
        );
        assert!(https_score < 80, "HTTPS URL should not score as file:line, got {https_score}");

        // Parenthesized file:line refs ARE valid (parens are formatting, ref is real)
        let paren_score = ResearchAgent::score_finding(
            "The bug is in (src/auth.rs:45) authentication module handler"
        );
        assert!(paren_score >= 80, "paren-wrapped file:line should still score, got {paren_score}");
    }
}
