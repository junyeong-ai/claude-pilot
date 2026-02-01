//! Unified code reviewer agent for systematic quality assurance.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use modmap::{Convention, KnownIssue};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::traits::{
    AgentCore, AgentRole, AgentTask, AgentTaskResult, ArtifactType, SpecializedAgent, TaskArtifact,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::error::Result;
use crate::workspace::Workspace;

const REVIEWER_SYSTEM_PROMPT: &str = r"You are a Code Reviewer Agent responsible for systematic quality assurance.

## Role
- Review code changes for correctness, security, and maintainability
- Provide actionable feedback with specific file:line references
- Do NOT modify files - only analyze and report

## Output Format
Output one of:
- `PASS` - No issues found
- `ISSUES` - List each issue with format: `[SEVERITY] file:line - description`

Severity levels: CRITICAL, HIGH, MEDIUM, LOW

## Focus Areas
1. Logic correctness and edge cases
2. Security vulnerabilities (injection, secrets, validation)
3. Error handling completeness
4. Code clarity and maintainability
5. Project convention adherence";

/// Issue severity for review findings.
/// Re-exported from modmap but with local parsing support.
pub use modmap::IssueSeverity;

/// Review issue found during code review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewIssue {
    pub severity: IssueSeverity,
    pub file: String,
    pub line: Option<u32>,
    pub description: String,
}

/// Context for project-level review including conventions and known issues.
#[derive(Debug, Clone, Default)]
pub struct ProjectReviewContext {
    pub conventions: Vec<ReviewConvention>,
    pub known_issues: Vec<ReviewKnownIssue>,
}

/// Convention with module association for review context.
#[derive(Debug, Clone)]
pub struct ReviewConvention {
    pub name: String,
    pub pattern: String,
    pub module: Option<String>,
}

impl From<&Convention> for ReviewConvention {
    fn from(conv: &Convention) -> Self {
        Self {
            name: conv.name.clone(),
            pattern: conv.pattern.clone(),
            module: None,
        }
    }
}

/// Known issue with module association for review context.
#[derive(Debug, Clone)]
pub struct ReviewKnownIssue {
    pub id: String,
    pub description: String,
    pub module: Option<String>,
}

impl From<&KnownIssue> for ReviewKnownIssue {
    fn from(issue: &KnownIssue) -> Self {
        Self {
            id: issue.id.clone(),
            description: issue.description.clone(),
            module: None,
        }
    }
}

fn parse_issue_severity(s: &str) -> Option<IssueSeverity> {
    match s.to_uppercase().as_str() {
        "CRITICAL" => Some(IssueSeverity::Critical),
        "HIGH" => Some(IssueSeverity::High),
        "MEDIUM" => Some(IssueSeverity::Medium),
        "LOW" => Some(IssueSeverity::Low),
        _ => None,
    }
}

impl ProjectReviewContext {
    pub fn from_workspace(workspace: &Workspace) -> Self {
        let mut conventions = Vec::new();
        let mut known_issues = Vec::new();

        for module in workspace.modules() {
            for conv in &module.conventions {
                conventions.push(ReviewConvention {
                    name: conv.name.clone(),
                    pattern: conv.pattern.clone(),
                    module: Some(module.name.clone()),
                });
            }

            for issue in &module.known_issues {
                known_issues.push(ReviewKnownIssue {
                    id: issue.id.clone(),
                    description: issue.description.clone(),
                    module: Some(module.name.clone()),
                });
            }
        }

        Self {
            conventions,
            known_issues,
        }
    }

    fn build_context_prompt(&self) -> String {
        if self.conventions.is_empty() && self.known_issues.is_empty() {
            return String::new();
        }

        let mut prompt = String::new();

        if !self.conventions.is_empty() {
            prompt.push_str("\n## Project Conventions\n");
            for conv in &self.conventions {
                if let Some(module) = &conv.module {
                    prompt.push_str(&format!("- [{}] {}: {}\n", module, conv.name, conv.pattern));
                } else {
                    prompt.push_str(&format!("- {}: {}\n", conv.name, conv.pattern));
                }
            }
        }

        if !self.known_issues.is_empty() {
            prompt.push_str("\n## Known Issues (Check for recurrence)\n");
            for issue in &self.known_issues {
                if let Some(module) = &issue.module {
                    prompt.push_str(&format!("- [{}] {}\n", module, issue.description));
                } else {
                    prompt.push_str(&format!("- {}\n", issue.description));
                }
            }
        }

        prompt
    }
}

pub struct ReviewerAgent {
    core: AgentCore,
    context: ProjectReviewContext,
}

impl ReviewerAgent {
    pub fn new(id: impl Into<String>, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::reviewer(), task_agent),
            context: ProjectReviewContext::default(),
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id, task_agent))
    }

    pub fn with_context(mut self, context: ProjectReviewContext) -> Self {
        self.context = context;
        self
    }

    pub fn set_context(&mut self, context: ProjectReviewContext) {
        self.context = context;
    }

    fn full_system_prompt(&self) -> String {
        let context_prompt = self.context.build_context_prompt();
        if context_prompt.is_empty() {
            REVIEWER_SYSTEM_PROMPT.to_string()
        } else {
            format!("{}\n{}", REVIEWER_SYSTEM_PROMPT, context_prompt)
        }
    }

    fn parse_review_output(&self, output: &str, task_id: &str) -> AgentTaskResult {
        // Find the verdict line - prefer explicit "VERDICT:" marker, fallback to first non-empty line
        let mut first_line = None;
        let mut explicit_verdict = None;
        let mut in_code_block = false;

        for line in output.lines() {
            let trimmed = line.trim();

            // Track code blocks to ignore their content
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }

            // Skip empty lines and code block content
            if trimmed.is_empty() || in_code_block {
                continue;
            }

            // Look for explicit verdict marker anywhere in output
            if trimmed.to_uppercase().starts_with("VERDICT:") {
                explicit_verdict = Some(trimmed[8..].trim().to_uppercase());
                break; // Explicit marker takes precedence
            } else if first_line.is_none() {
                // Capture first non-empty line as fallback
                first_line = Some(trimmed.to_uppercase());
            }
        }

        // Prefer explicit verdict, fallback to first line
        let verdict_line = explicit_verdict.or(first_line);

        let verdict = verdict_line.unwrap_or_default();
        let explicit_pass = verdict.starts_with("PASS");
        let _explicit_issues = verdict.starts_with("ISSUES");

        // Parse issues - only lines that start with [SEVERITY] pattern (not in code blocks)
        let mut findings = Vec::new();
        let mut in_code_block = false;

        for line in output.lines() {
            let trimmed = line.trim();

            // Track code blocks
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
                continue;
            }

            if !in_code_block && let Some(issue) = self.parse_issue_line(line) {
                findings.push(format!(
                    "[{}] {}:{} - {}",
                    issue.severity,
                    issue.file,
                    issue.line.map(|l| l.to_string()).unwrap_or_default(),
                    issue.description
                ));
            }
        }

        // Determine success:
        // - Must have explicit PASS verdict AND no findings = success
        // - ISSUES verdict or any findings present = failure
        // - No explicit verdict = failure (require explicit PASS)
        let passed = explicit_pass && findings.is_empty();
        let result = if passed {
            AgentTaskResult::success(task_id, output)
        } else {
            AgentTaskResult::failure(task_id, output)
        };
        result
            .with_findings(findings)
            .with_artifacts(vec![TaskArtifact {
                name: format!("review-{}.md", task_id),
                content: output.to_string(),
                artifact_type: ArtifactType::Report,
            }])
    }

    fn parse_issue_line(&self, line: &str) -> Option<ReviewIssue> {
        let trimmed = line.trim();
        if !trimmed.starts_with('[') {
            return None;
        }

        let close_bracket = trimmed.find(']')?;
        let severity_str = &trimmed[1..close_bracket];
        let severity = parse_issue_severity(severity_str)?;

        let rest = trimmed[close_bracket + 1..].trim();
        let dash_pos = rest.find('-')?;
        let location = rest[..dash_pos].trim();
        let description = rest[dash_pos + 1..].trim();

        // Parse file:line, handling Windows paths (e.g., C:/file.rs:10)
        // Find the last ':' that is followed by digits (line number)
        let (file, line_num) = {
            let mut file_part = location;
            let mut line_num = None;

            // Search backwards for ':' followed by digits
            if let Some(last_colon) = location.rfind(':') {
                let after_colon = &location[last_colon + 1..];
                if !after_colon.is_empty() && after_colon.chars().all(|c| c.is_ascii_digit()) {
                    file_part = &location[..last_colon];
                    line_num = after_colon.parse().ok();
                }
            }

            (file_part.to_string(), line_num)
        };

        Some(ReviewIssue {
            severity,
            file,
            line: line_num,
            description: description.to_string(),
        })
    }
}

#[async_trait]
impl SpecializedAgent for ReviewerAgent {
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
        REVIEWER_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Reviewer executing");

        let system = task
            .context
            .composed_prompt
            .clone()
            .unwrap_or_else(|| self.full_system_prompt());

        let prompt = format!(
            "{}\n\n---\n\n## Review Task: {}\n\n{}",
            system, task.id, task.description
        );

        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => Ok(self.parse_review_output(&output, &task.id)),
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
    use modmap::IssueCategory;

    #[test]
    fn test_reviewer_role() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        assert_eq!(agent.role(), &AgentRole::reviewer());
        assert_eq!(agent.id(), "reviewer-test");
    }

    #[test]
    fn test_issue_severity_parsing() {
        assert_eq!(
            parse_issue_severity("CRITICAL"),
            Some(IssueSeverity::Critical)
        );
        assert_eq!(parse_issue_severity("high"), Some(IssueSeverity::High));
        assert_eq!(parse_issue_severity("Medium"), Some(IssueSeverity::Medium));
        assert_eq!(parse_issue_severity("low"), Some(IssueSeverity::Low));
        assert_eq!(parse_issue_severity("invalid"), None);
    }

    #[test]
    fn test_parse_issue_line() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        let issue = agent.parse_issue_line("[HIGH] src/auth.rs:42 - SQL injection vulnerability");
        assert!(issue.is_some());
        let issue = issue.unwrap();
        assert_eq!(issue.severity, IssueSeverity::High);
        assert_eq!(issue.file, "src/auth.rs");
        assert_eq!(issue.line, Some(42));
        assert_eq!(issue.description, "SQL injection vulnerability");
    }

    #[test]
    fn test_parse_issue_line_no_line_number() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        let issue = agent.parse_issue_line("[MEDIUM] src/config.rs - Missing validation");
        assert!(issue.is_some());
        let issue = issue.unwrap();
        assert_eq!(issue.severity, IssueSeverity::Medium);
        assert_eq!(issue.file, "src/config.rs");
        assert_eq!(issue.line, None);
    }

    #[test]
    fn test_review_output_pass() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        let result = agent.parse_review_output("PASS\n\nNo issues found.", "task-1");
        assert!(result.success);
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_review_output_issues() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        let output =
            "ISSUES\n[HIGH] src/auth.rs:42 - SQL injection\n[LOW] src/utils.rs:10 - Magic number";
        let result = agent.parse_review_output(output, "task-1");
        assert!(!result.success);
        assert_eq!(result.findings.len(), 2);
    }

    #[test]
    fn test_project_context_prompt() {
        let context = ProjectReviewContext {
            conventions: vec![ReviewConvention {
                name: "error-handling".into(),
                pattern: "Use Result<T, E> for fallible operations".into(),
                module: Some("core".into()),
            }],
            known_issues: vec![ReviewKnownIssue {
                id: "race-condition".into(),
                description: "Race condition in session handling".into(),
                module: Some("auth".into()),
            }],
        };

        let prompt = context.build_context_prompt();
        assert!(prompt.contains("Project Conventions"));
        assert!(prompt.contains("[core]"));
        assert!(prompt.contains("Known Issues"));
        assert!(prompt.contains("[auth]"));
    }

    #[test]
    fn test_full_system_prompt_with_context() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let context = ProjectReviewContext {
            conventions: vec![ReviewConvention {
                name: "naming".into(),
                pattern: "Use snake_case".into(),
                module: None,
            }],
            known_issues: vec![],
        };
        let agent = ReviewerAgent::new("reviewer-test", task_agent).with_context(context);

        let prompt = agent.full_system_prompt();
        assert!(prompt.contains("Code Reviewer Agent"));
        assert!(prompt.contains("Project Conventions"));
        assert!(prompt.contains("Use snake_case"));
    }

    #[test]
    fn test_review_convention_from_modmap() {
        let conv = Convention::new("test", "pattern");
        let review_conv = ReviewConvention::from(&conv);
        assert_eq!(review_conv.name, "test");
        assert_eq!(review_conv.pattern, "pattern");
    }

    #[test]
    fn test_review_known_issue_from_modmap() {
        let issue = KnownIssue::new(
            "test-id",
            "Test description",
            IssueSeverity::High,
            IssueCategory::Security,
        );
        let review_issue = ReviewKnownIssue::from(&issue);
        assert_eq!(review_issue.id, "test-id");
        assert_eq!(review_issue.description, "Test description");
    }

    #[test]
    fn test_review_output_pass_with_issues_elsewhere() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Edge case: starts with PASS but has severity markers later
        let output = "PASS\n\nHowever, I noticed:\n[MEDIUM] src/lib.rs:10 - Consider refactoring";
        let result = agent.parse_review_output(output, "task-1");
        assert!(
            !result.success,
            "Should fail when PASS is followed by severity markers"
        );
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn test_review_output_no_explicit_verdict() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Edge case: no explicit PASS or ISSUES, just narrative
        let output = "Everything looks good! The code is well-structured.";
        let result = agent.parse_review_output(output, "task-1");
        assert!(!result.success, "Should fail without explicit PASS");
    }

    #[test]
    fn test_review_output_issues_header() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Standard ISSUES format
        let output = "ISSUES\n\n[HIGH] src/auth.rs:42 - Security issue";
        let result = agent.parse_review_output(output, "task-1");
        assert!(!result.success);
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn test_review_output_with_code_example() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Code example with severity markers in code blocks should not trigger false positive
        let output = r#"PASS

The code looks good. Here's an example of what NOT to do:

```rust
// Bad: Don't use this pattern
fn check_auth() -> Result<(), String> {
    // [CRITICAL] This would be a security issue
    return Ok(());
}
```

No actual issues found."#;
        let result = agent.parse_review_output(output, "task-1");
        assert!(
            result.success,
            "Should pass - severity markers in code blocks should be ignored"
        );
        assert!(
            result.findings.is_empty(),
            "Should have no findings from code block"
        );
    }

    #[test]
    fn test_review_output_with_quoted_text() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Quoted text mentioning severity should not trigger false positive
        let output = r#"PASS

The previous review mentioned "[HIGH] src/auth.rs:42 - SQL injection" 
but this has now been fixed. No current issues."#;
        let result = agent.parse_review_output(output, "task-1");
        assert!(
            result.success,
            "Should pass - quoted severity markers without proper format should be ignored"
        );
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_review_output_mixed_content() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Mixed content with code blocks and actual issues
        let output = r#"ISSUES

Found the following problems:

[HIGH] src/auth.rs:42 - SQL injection vulnerability

For context, here's the vulnerable code:

```rust
let query = format!("SELECT * FROM users WHERE id = {}", user_id);
// [CRITICAL] This is injectable
```

[MEDIUM] src/utils.rs:10 - Magic number

Please fix these issues."#;
        let result = agent.parse_review_output(output, "task-1");
        assert!(!result.success);
        assert_eq!(
            result.findings.len(),
            2,
            "Should only count actual issues, not code block comments"
        );
    }

    #[test]
    fn test_review_output_verdict_marker() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Test explicit VERDICT: marker
        let output = r#"After reviewing the code, here's my assessment:

VERDICT: PASS

Everything looks good!"#;
        let result = agent.parse_review_output(output, "task-1");
        assert!(result.success, "Should recognize VERDICT: marker");
        assert!(result.findings.is_empty());
    }

    #[test]
    fn test_review_output_malformed_issues() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ReviewerAgent::new("reviewer-test", task_agent);

        // Issues that don't match the exact format should be ignored
        let output = r#"PASS

Note: There's a [MEDIUM] issue in the logging, but it's not urgent.
Also, consider [HIGH] priority refactoring later."#;
        let result = agent.parse_review_output(output, "task-1");
        assert!(
            result.success,
            "Malformed issues without file:line format should be ignored"
        );
        assert!(result.findings.is_empty());
    }
}
