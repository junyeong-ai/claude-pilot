use crate::config::{DisplayConfig, TaskBudgetConfig};
use crate::context::{MissionContext, TaskContextBudget};
use crate::mission::{Mission, Task};
use crate::utils::estimate_tokens;

/// Prompt builder with token budget awareness.
///
/// Builds prompts with natural information ordering:
/// 1. Mission context (what we're doing)
/// 2. Current task (what to do now)
/// 3. Dependencies (what came before)
/// 4. Learnings (what to remember)
pub struct PromptBuilder {
    budget: TaskContextBudget,
    max_recent_items: usize,
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self {
            budget: TaskContextBudget::default(),
            max_recent_items: DisplayConfig::default().max_recent_items,
        }
    }
}

impl PromptBuilder {
    pub fn new(task_config: &TaskBudgetConfig, display_config: &DisplayConfig) -> Self {
        Self {
            budget: TaskContextBudget::from_config(task_config),
            max_recent_items: display_config.max_recent_items,
        }
    }

    pub fn build_mission_system_prompt(&self, mission: &Mission) -> String {
        format!(
            r#"# Mission: {mission_id}

{description}

## Environment

Branch: {branch}
Working directory: {workdir}

## Code Analysis

Use symora for semantic code analysis:
- `symora search "query" --json` - Semantic search
- `symora find refs file:line:col --json` - Find usages
- `symora calls incoming/outgoing file:line:col --json` - Call hierarchy

## Constraints

- Follow existing patterns and conventions
- Make minimal changes
- Do not modify unrelated code"#,
            mission_id = mission.id,
            description = truncate_to_budget(&mission.description, self.budget.mission_summary),
            branch = mission.branch.as_deref().unwrap_or("(current)"),
            workdir = mission
                .worktree_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(main repo)".to_string())
        )
    }

    /// Build task execution message.
    /// When context is provided, uses context learnings; otherwise uses mission learnings.
    pub fn build_task_message(
        &self,
        mission: &Mission,
        task: &Task,
        context: Option<&MissionContext>,
    ) -> String {
        let progress = mission.progress();
        let mut parts = Vec::new();

        // Task header
        parts.push(format!(
            "## Task {} ({}/{})",
            task.id, progress.completed, progress.total
        ));

        // Task description
        parts.push(truncate_to_budget(
            &task.description,
            self.budget.current_task,
        ));

        // Affected files
        if !task.affected_files.is_empty() {
            parts.push(format!("Files: {}", task.affected_files.join(", ")));
        }

        // Test patterns
        if !task.test_patterns.is_empty() {
            parts.push(format!("Verify: {}", task.test_patterns.join(", ")));
        }

        // Dependencies
        if !task.dependencies.is_empty() {
            let deps = self.format_dependencies(mission, task);
            if !deps.is_empty() {
                parts.push(format!("## Dependencies\n{}", deps));
            }
        }

        // Retry context - provide structured information with fallback for silent failures
        if task.retry_count > 0 {
            let mut retry_info = format!(
                "## Retry Context (attempt {}/{})",
                task.retry_count + 1,
                task.max_retries + 1
            );

            if let Some(ref result) = task.result
                && !result.success
            {
                if !result.output.is_empty() {
                    // Primary: use previous output
                    let hint = extract_first_line(&result.output);
                    if !hint.is_empty() {
                        retry_info.push_str(&format!("\nPrevious result: {}", hint));
                    }
                } else {
                    // Fallback for silent failures: use file change context
                    let has_file_context =
                        !result.files_modified.is_empty() || !result.files_created.is_empty();
                    if has_file_context {
                        retry_info
                            .push_str("\nPrevious attempt completed silently. Files touched:");
                        for f in result.files_modified.iter().take(5) {
                            retry_info.push_str(&format!("\n  - {} (modified)", f));
                        }
                        for f in result.files_created.iter().take(5) {
                            retry_info.push_str(&format!("\n  - {} (created)", f));
                        }
                        retry_info.push_str("\nReview these files for partial/incorrect changes.");
                    } else {
                        // No output, no file changes: provide diagnostic guidance
                        retry_info.push_str(
                            "\nPrevious attempt produced no output or file changes. \
                             Verify build setup and file paths before implementing.",
                        );
                    }
                }
            }

            retry_info.push_str("\nTry a different approach from the previous attempt.");
            parts.push(retry_info);
        }

        // Scope reduction - provide semantic guidance instead of just a percentage
        if context.is_none() && task.scope_factor < 1.0 {
            let scope_guidance = format!(
                "## Scope Constraint ({:.0}%)\n\
                 Resource constraints require reduced scope. Prioritize:\n\
                 - Core functionality that addresses the main requirement\n\
                 - Minimal viable implementation over comprehensive coverage",
                task.scope_factor * 100.0
            );
            parts.push(scope_guidance);
        }

        // Learnings: use context if available, otherwise use mission learnings
        let learnings = if let Some(ctx) = context {
            self.format_context_learnings(ctx, task)
        } else {
            self.format_learnings(mission, task)
        };
        if !learnings.is_empty() {
            parts.push(format!("## Learnings\n{}", learnings));
        }

        parts.join("\n\n")
    }

    pub fn build_qa_prompt(&self, mission: &Mission) -> String {
        format!(
            "## Final QA Review\n\n\
             Mission: {}\n\n\
             Verify:\n\
             1. Functionality matches objective\n\
             2. Code quality and tests pass\n\
             3. No security issues",
            truncate_to_budget(&mission.description, self.budget.mission_summary)
        )
    }

    pub fn build_direct_prompt(&self, description: &str) -> String {
        format!(
            "## Task\n\n{}\n\n\
             Complete in a single pass. Ensure code compiles and tests pass.",
            truncate_to_budget(description, self.budget.current_task)
        )
    }

    fn format_dependencies(&self, mission: &Mission, task: &Task) -> String {
        let mut result = String::new();
        let mut used_tokens = 0;

        for dep_id in &task.dependencies {
            if used_tokens >= self.budget.direct_dependencies {
                break;
            }

            if let Some(dep_task) = mission.task(dep_id) {
                let summary = dep_task
                    .result
                    .as_ref()
                    .map(|r| extract_first_line(&r.output))
                    .unwrap_or_else(|| "(completed)".to_string());
                let line = format!("- {}: {}\n", dep_id, summary);
                used_tokens += estimate_tokens(&line);
                result.push_str(&line);
            }
        }

        result
    }

    fn format_learnings(&self, mission: &Mission, task: &Task) -> String {
        if mission.learnings.is_empty() {
            return String::new();
        }

        let mut result = String::new();
        let mut used_tokens = 0;

        let relevant: Vec<_> = mission
            .learnings
            .iter()
            .filter(|l| is_relevant_to_task(&l.content, task))
            .take(self.max_recent_items)
            .collect();

        let learnings = if relevant.is_empty() {
            mission
                .learnings
                .iter()
                .rev()
                .take(self.max_recent_items)
                .collect()
        } else {
            relevant
        };

        for learning in learnings {
            let line = format!("- [{}] {}\n", learning.category, learning.content);
            let tokens = estimate_tokens(&line);
            if used_tokens + tokens > self.budget.learnings {
                break;
            }
            result.push_str(&line);
            used_tokens += tokens;
        }

        result
    }

    fn format_context_learnings(&self, context: &MissionContext, task: &Task) -> String {
        if context.summary.critical_learnings.is_empty() {
            return String::new();
        }

        let mut result = String::new();
        let mut used_tokens = 0;

        let relevant: Vec<_> = context
            .summary
            .critical_learnings
            .iter()
            .filter(|l| is_relevant_to_task(l, task))
            .take(self.max_recent_items)
            .collect();

        let learnings = if relevant.is_empty() {
            context
                .summary
                .critical_learnings
                .iter()
                .rev()
                .take(self.max_recent_items)
                .collect()
        } else {
            relevant
        };

        for learning in learnings {
            let line = format!("- {}\n", learning);
            let tokens = estimate_tokens(&line);
            if used_tokens + tokens > self.budget.learnings {
                break;
            }
            result.push_str(&line);
            used_tokens += tokens;
        }

        result
    }
}

fn truncate_to_budget(text: &str, budget: usize) -> String {
    let tokens = estimate_tokens(text);
    if tokens <= budget {
        return text.to_string();
    }

    let target_chars = budget * 4;
    if text.len() <= target_chars {
        return text.to_string();
    }

    let truncated: String = text.chars().take(target_chars).collect();
    truncated
        .rfind(' ')
        .map(|i| format!("{}...", &truncated[..i]))
        .unwrap_or_else(|| format!("{}...", truncated))
}

fn extract_first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(100)
        .collect()
}

/// Check if a learning is potentially relevant to a task.
///
/// Uses conservative, language-agnostic heuristics:
/// - File path overlap (universal signal)
/// - Substring containment (works for all scripts including CJK)
///
/// NOTE: Semantic relevance is determined by LLM, not this function.
/// This filter only removes obviously unrelated learnings to save context.
/// When in doubt, include the learning - LLM can ignore irrelevant context.
fn is_relevant_to_task(learning: &str, task: &Task) -> bool {
    let task_lower = task.description.to_lowercase();
    let learning_lower = learning.to_lowercase();

    // 1. File path overlap - extract paths from both and check intersection
    let task_paths: Vec<&str> = task_lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| s.contains('/') || s.contains('.'))
        .collect();

    let learning_paths: Vec<&str> = learning_lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|s| s.contains('/') || s.contains('.'))
        .collect();

    // If both mention file paths, check overlap
    if !task_paths.is_empty() && !learning_paths.is_empty() {
        for tp in &task_paths {
            for lp in &learning_paths {
                // Either path contains the other (handles partial paths)
                if tp.contains(lp) || lp.contains(tp) {
                    return true;
                }
            }
        }
    }

    // 2. Substring containment - works for all scripts (CJK, Cyrillic, Arabic, etc.)
    // Check if any significant portion of task description appears in learning
    // Use character-based chunking, not word-based (language-agnostic)
    let task_chars: Vec<char> = task_lower.chars().filter(|c| c.is_alphanumeric()).collect();
    if task_chars.len() >= 4 {
        // Extract 4+ character substrings and check containment
        let task_substr: String = task_chars.iter().collect();
        if task_substr.len() >= 4
            && learning_lower.contains(&task_substr[..4.min(task_substr.len())])
        {
            return true;
        }
    }

    // 3. Conservative default: include if task is very short (hard to filter accurately)
    // LLM can ignore irrelevant context, but missing relevant context hurts
    task_lower.chars().count() <= 20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_within_limit() {
        let text = "Short text";
        assert_eq!(truncate_to_budget(text, 100), text);
    }

    #[test]
    fn test_truncate_exceeds_limit() {
        let text = "This is a much longer text that should be truncated";
        let result = truncate_to_budget(text, 5);
        assert!(result.len() < text.len());
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_extract_first_line() {
        assert_eq!(extract_first_line("First\nSecond"), "First");
    }

    #[test]
    fn test_extract_first_line_truncates() {
        let text = "A".repeat(200);
        assert_eq!(extract_first_line(&text).len(), 100);
    }
}
