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
use crate::error::Result;

/// Maximum number of findings to extract from research output.
///
/// This limit balances comprehensiveness with performance and readability:
/// - Prevents overwhelming downstream agents with excessive findings
/// - Keeps context size manageable for consensus and planning phases
/// - Encourages research output to be concise and focused on key insights
///
/// If research yields more than this limit, the agent should be more selective
/// in what it reports rather than increasing this ceiling.
const MAX_FINDINGS: usize = 20;

/// Timeout for research task execution in seconds.
///
/// Research tasks involve codebase analysis and evidence gathering which can be
/// time-consuming. A 5-minute timeout provides enough time for:
/// - Semantic searches across large codebases
/// - Reference finding and call hierarchy analysis
/// - Pattern discovery and dependency mapping
///
/// This prevents research tasks from running indefinitely while allowing thorough
/// exploration of the codebase.
const RESEARCH_TIMEOUT_SECS: u64 = 300;

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
}

impl ResearchAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::core_research(), task_agent),
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

        if result.success && !result.findings.is_empty() {
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
        // Max possible per finding: critical(150) + confidence(100) + file:line(60) + file(40) + pattern(35) + convention(20) + structured(10) + detailed(15) = 430
        // Use 300 as practical max (unlikely to hit all bonuses)
        let practical_max = findings.len() as f64 * 300.0;
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

        // Take top MAX_FINDINGS after sorting
        findings
            .into_iter()
            .take(MAX_FINDINGS)
            .map(|(finding, _)| finding)
            .collect()
    }

    /// Scores a finding based on relevance indicators.
    /// Higher scores indicate more important findings that should be prioritized.
    fn score_finding(finding: &str) -> u32 {
        let lower = finding.to_lowercase();
        let mut score = 0u32;

        // Highest priority: Critical findings (security, blockers, risks)
        if lower.contains("critical") || lower.contains("blocker") || lower.contains("risk") {
            score += 150;
        }

        // High priority: Explicit confidence scores or markers
        if lower.contains("confidence:") || lower.contains("confidence score") {
            score += 100;
        }

        // Medium-high priority: File references with locations (more specific)
        if finding.contains(".rs:") || finding.contains(".ts:") || finding.contains(".js:") {
            score += 60;
        }

        // Medium priority: General file references
        if lower.contains(".rs")
            || lower.contains(".ts")
            || lower.contains(".js")
            || lower.contains(".py")
            || lower.contains(".go")
        {
            score += 40;
        }

        // Medium priority: Patterns, dependencies, or relationships
        if lower.contains("pattern")
            || lower.contains("dependency")
            || lower.contains("relationship")
        {
            score += 35;
        }

        // Low-medium priority: Implementation details or conventions
        if lower.contains("convention") || lower.contains("implements") || lower.contains("uses") {
            score += 20;
        }

        // Boost for structured findings (bullets, numbers)
        if finding.trim_start().starts_with('-')
            || finding.trim_start().starts_with('*')
            || finding
                .trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        {
            score += 10;
        }

        // Penalize very short findings (likely less informative)
        if finding.len() < 30 {
            score = score.saturating_sub(15);
        }

        // Boost longer, more detailed findings
        if finding.len() > 100 {
            score += 15;
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
        let timeout = Duration::from_secs(RESEARCH_TIMEOUT_SECS);
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
* Discovered pattern in src/lib.rs
The implementation uses src/utils/helper.ts
"#;

        let findings = agent.extract_findings(output);
        assert!(!findings.is_empty());
    }

    #[test]
    fn test_findings_prioritization() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ResearchAgent::new("test".to_string(), task_agent);

        // Create output with varying priority findings
        let output = r#"This is a low priority finding without much detail.

CRITICAL: Security vulnerability found in authentication module at src/auth.rs:45 that could allow unauthorized access.

- Found relevant file: src/utils/helper.rs with utility functions

This finding has confidence: 0.95 and indicates the main entry point is src/main.rs:10

Discovered pattern: Repository pattern used throughout the codebase for data access

Short note

This is a detailed finding that explains the architecture uses a layered approach with clear separation of concerns. The presentation layer communicates with the business logic layer through well-defined interfaces."#;

        let findings = agent.extract_findings(output);

        // Critical finding should be first
        assert!(findings[0].contains("CRITICAL"));

        // Confidence score finding should be high priority
        assert!(findings.iter().take(3).any(|f| f.contains("confidence")));

        // Very short findings should be deprioritized
        let short_finding_pos = findings.iter().position(|f| f == "Short note");
        assert!(short_finding_pos.is_none() || short_finding_pos.unwrap() > 3);
    }

    #[test]
    fn test_score_finding() {
        // Critical findings get highest scores
        let critical_score = ResearchAgent::score_finding("CRITICAL: Issue in src/auth.rs:45");
        let pattern_score = ResearchAgent::score_finding("- Discovered pattern in codebase");
        let short_score = ResearchAgent::score_finding("Short note");

        assert!(critical_score > pattern_score);
        assert!(pattern_score > short_score);

        // Confidence scores should be high priority
        let confidence_score = ResearchAgent::score_finding("Found file with confidence: 0.95");
        assert!(confidence_score > pattern_score);
    }
}
