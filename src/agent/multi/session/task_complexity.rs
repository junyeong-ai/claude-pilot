//! Task complexity estimation and automatic task splitting.
//!
//! Analyzes tasks before assignment to estimate their token cost and
//! automatically splits large tasks to prevent agent context overflow.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::task_graph::TaskInfo;

/// Estimated complexity of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityEstimate {
    /// Total estimated tokens.
    pub total_tokens: u32,
    /// Tokens for file context.
    pub file_context_tokens: u32,
    /// Tokens for task description and instructions.
    pub instruction_tokens: u32,
    /// Tokens for dependencies context.
    pub dependency_tokens: u32,
    /// Tokens reserved for agent output.
    pub output_reserve: u32,
    /// Confidence in the estimate (0.0-1.0).
    pub confidence: f32,
    /// Whether the task exceeds budget.
    pub exceeds_budget: bool,
    /// Recommended split if exceeds budget.
    pub recommended_splits: Option<Vec<SplitSuggestion>>,
}

impl ComplexityEstimate {
    pub fn simple(tokens: u32) -> Self {
        Self {
            total_tokens: tokens,
            file_context_tokens: tokens / 2,
            instruction_tokens: tokens / 4,
            dependency_tokens: tokens / 8,
            output_reserve: tokens / 8,
            confidence: 0.5,
            exceeds_budget: false,
            recommended_splits: None,
        }
    }

    pub fn with_budget_check(mut self, budget: u32) -> Self {
        self.exceeds_budget = self.total_tokens > budget;
        self
    }
}

/// Suggestion for how to split a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitSuggestion {
    /// Description of the subtask.
    pub description: String,
    /// Files involved.
    pub files: Vec<String>,
    /// Estimated tokens.
    pub estimated_tokens: u32,
    /// Dependencies on other splits.
    pub depends_on: Vec<usize>,
}

/// Configuration for the complexity estimator.
#[derive(Debug, Clone)]
pub struct ComplexityConfig {
    /// Default budget per agent.
    pub default_budget: u32,
    /// Tokens per line of code.
    pub tokens_per_line: f32,
    /// Tokens per file (metadata overhead).
    pub tokens_per_file: u32,
    /// Base tokens for task instructions.
    pub base_instruction_tokens: u32,
    /// Tokens per dependency.
    pub tokens_per_dependency: u32,
    /// Output reserve ratio.
    pub output_reserve_ratio: f32,
    /// Safety margin.
    pub safety_margin: f32,
}

impl Default for ComplexityConfig {
    fn default() -> Self {
        Self {
            default_budget: 100000,       // 100k tokens
            tokens_per_line: 1.5,         // Avg tokens per line of code
            tokens_per_file: 200,         // Metadata per file
            base_instruction_tokens: 500, // Base task instructions
            tokens_per_dependency: 300,   // Context per dependency
            output_reserve_ratio: 0.2,    // Reserve 20% for output
            safety_margin: 0.1,           // 10% safety margin
        }
    }
}

/// Estimator for task complexity.
pub struct TaskComplexityEstimator {
    config: ComplexityConfig,
    file_line_cache: HashMap<String, u32>,
}

impl TaskComplexityEstimator {
    pub fn new(config: ComplexityConfig) -> Self {
        Self {
            config,
            file_line_cache: HashMap::new(),
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(ComplexityConfig::default())
    }

    /// Estimate complexity of a task.
    pub fn estimate(&mut self, task: &TaskInfo, dependency_count: usize) -> ComplexityEstimate {
        // Estimate file context
        let file_tokens = self.estimate_file_tokens(&task.affected_files);

        // Estimate instruction tokens
        let instruction_tokens =
            self.config.base_instruction_tokens + (task.description.len() as f32 / 4.0) as u32; // ~4 chars per token

        // Estimate dependency tokens
        let dependency_tokens = (dependency_count as u32) * self.config.tokens_per_dependency;

        // Calculate subtotal
        let subtotal = file_tokens + instruction_tokens + dependency_tokens;

        // Add output reserve
        let output_reserve = (subtotal as f32 * self.config.output_reserve_ratio) as u32;

        // Add safety margin
        let with_margin =
            ((subtotal + output_reserve) as f32 * (1.0 + self.config.safety_margin)) as u32;

        let total_tokens = with_margin;

        // Calculate effective budget
        let effective_budget = (self.config.default_budget as f32
            * (1.0 - self.config.output_reserve_ratio)
            * (1.0 - self.config.safety_margin)) as u32;

        let exceeds_budget = total_tokens > effective_budget;

        // Generate split suggestions if needed
        let recommended_splits = if exceeds_budget {
            Some(self.suggest_splits(task, effective_budget))
        } else {
            None
        };

        // Calculate confidence based on factors
        let confidence = self.calculate_confidence(task, &file_tokens);

        ComplexityEstimate {
            total_tokens,
            file_context_tokens: file_tokens,
            instruction_tokens,
            dependency_tokens,
            output_reserve,
            confidence,
            exceeds_budget,
            recommended_splits,
        }
    }

    /// Estimate tokens for a specific budget.
    pub fn estimate_with_budget(
        &mut self,
        task: &TaskInfo,
        dependency_count: usize,
        budget: u32,
    ) -> ComplexityEstimate {
        let mut estimate = self.estimate(task, dependency_count);
        let effective_budget = (budget as f32
            * (1.0 - self.config.output_reserve_ratio)
            * (1.0 - self.config.safety_margin)) as u32;

        estimate.exceeds_budget = estimate.total_tokens > effective_budget;
        if estimate.exceeds_budget && estimate.recommended_splits.is_none() {
            estimate.recommended_splits = Some(self.suggest_splits(task, effective_budget));
        }

        estimate
    }

    fn estimate_file_tokens(&mut self, files: &[String]) -> u32 {
        let mut total = 0u32;

        for file in files {
            let lines = self.get_file_lines(file);
            let file_tokens =
                self.config.tokens_per_file + (lines as f32 * self.config.tokens_per_line) as u32;
            total = total.saturating_add(file_tokens);
        }

        total
    }

    fn get_file_lines(&mut self, file: &str) -> u32 {
        if let Some(&lines) = self.file_line_cache.get(file) {
            return lines;
        }

        // Try to count lines
        let lines = count_file_lines(file).unwrap_or(100); // Default to 100 lines
        self.file_line_cache.insert(file.to_string(), lines);
        lines
    }

    fn calculate_confidence(&self, task: &TaskInfo, file_tokens: &u32) -> f32 {
        let mut confidence: f32 = 0.7; // Base confidence

        // Higher confidence if we have actual file sizes
        if task
            .affected_files
            .iter()
            .all(|f| self.file_line_cache.contains_key(f))
        {
            confidence += 0.1;
        }

        // Lower confidence for very large tasks
        if *file_tokens > 50000 {
            confidence -= 0.1;
        }

        // Lower confidence if no files specified
        if task.affected_files.is_empty() {
            confidence -= 0.2;
        }

        confidence.clamp(0.3, 0.95)
    }

    fn suggest_splits(&self, task: &TaskInfo, target_budget: u32) -> Vec<SplitSuggestion> {
        let mut splits = Vec::new();

        if task.affected_files.is_empty() {
            // Can't split by files, suggest generic split
            splits.push(SplitSuggestion {
                description: format!("{} - Part 1", task.description),
                files: Vec::new(),
                estimated_tokens: target_budget / 2,
                depends_on: Vec::new(),
            });
            splits.push(SplitSuggestion {
                description: format!("{} - Part 2", task.description),
                files: Vec::new(),
                estimated_tokens: target_budget / 2,
                depends_on: vec![0],
            });
            return splits;
        }

        // Group files by estimated tokens to fit within budget
        let mut current_split_files = Vec::new();
        let mut current_tokens = self.config.base_instruction_tokens;
        let file_budget = (target_budget as f32 * 0.8) as u32; // Leave room for instructions

        for file in &task.affected_files {
            let file_lines = self.file_line_cache.get(file).copied().unwrap_or(100);
            let file_tokens = self.config.tokens_per_file
                + (file_lines as f32 * self.config.tokens_per_line) as u32;

            if current_tokens + file_tokens > file_budget && !current_split_files.is_empty() {
                // Start a new split
                let split_idx = splits.len();
                splits.push(SplitSuggestion {
                    description: format!(
                        "{} - Files: {}",
                        task.description,
                        current_split_files.join(", ")
                    ),
                    files: current_split_files.clone(),
                    estimated_tokens: current_tokens,
                    depends_on: if split_idx > 0 {
                        vec![split_idx - 1]
                    } else {
                        Vec::new()
                    },
                });
                current_split_files.clear();
                current_tokens = self.config.base_instruction_tokens;
            }

            current_split_files.push(file.clone());
            current_tokens += file_tokens;
        }

        // Add remaining files
        if !current_split_files.is_empty() {
            let split_idx = splits.len();
            splits.push(SplitSuggestion {
                description: format!(
                    "{} - Files: {}",
                    task.description,
                    current_split_files.join(", ")
                ),
                files: current_split_files,
                estimated_tokens: current_tokens,
                depends_on: if split_idx > 0 {
                    vec![split_idx - 1]
                } else {
                    Vec::new()
                },
            });
        }

        // If we ended up with just one split, it means the task is actually manageable
        if splits.len() == 1 {
            splits.clear();
        }

        splits
    }

    /// Apply splits to create new task infos.
    pub fn apply_splits(&self, task: &TaskInfo, splits: &[SplitSuggestion]) -> Vec<TaskInfo> {
        splits
            .iter()
            .enumerate()
            .map(|(i, split)| TaskInfo {
                id: format!("{}-part-{}", task.id, i + 1),
                description: split.description.clone(),
                module: task.module.clone(),
                required_role: task.required_role.clone(),
                estimated_complexity: split.estimated_tokens,
                priority: task.priority,
                affected_files: split.files.clone(),
            })
            .collect()
    }

    /// Clear the file line cache.
    pub fn clear_cache(&mut self) {
        self.file_line_cache.clear();
    }

    /// Pre-populate cache with file information.
    pub fn cache_file_lines(&mut self, file: &str, lines: u32) {
        self.file_line_cache.insert(file.to_string(), lines);
    }
}

fn count_file_lines(path: &str) -> Option<u32> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = File::open(Path::new(path)).ok()?;
    let reader = BufReader::new(file);
    Some(reader.lines().count() as u32)
}

/// Check if a task should be split based on quick heuristics.
pub fn should_split_quick(task: &TaskInfo, budget: u32) -> bool {
    // Quick heuristic: more than 10 files or estimated complexity > 60% of budget
    let file_count_threshold = 10;
    let complexity_threshold = (budget as f32 * 0.6) as u32;

    task.affected_files.len() > file_count_threshold
        || task.estimated_complexity > complexity_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_task(id: &str, files: Vec<String>, complexity: u32) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            description: "Test task".to_string(),
            module: "test".to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: complexity,
            priority: 1,
            affected_files: files,
        }
    }

    #[test]
    fn test_simple_estimate() {
        let mut estimator = TaskComplexityEstimator::with_default_config();

        // Pre-populate cache
        estimator.cache_file_lines("auth.rs", 200);
        estimator.cache_file_lines("api.rs", 300);

        let task = create_test_task(
            "task-1",
            vec!["auth.rs".to_string(), "api.rs".to_string()],
            1000,
        );

        let estimate = estimator.estimate(&task, 2);

        assert!(estimate.total_tokens > 0);
        assert!(estimate.confidence > 0.5);
        assert!(!estimate.exceeds_budget); // Default 100k budget should be enough
    }

    #[test]
    fn test_exceeds_budget() {
        let mut estimator = TaskComplexityEstimator::new(ComplexityConfig {
            default_budget: 5000, // Very small budget
            ..Default::default()
        });

        // Many files with lots of lines
        let files: Vec<String> = (0..20).map(|i| format!("file{}.rs", i)).collect();
        for file in &files {
            estimator.cache_file_lines(file, 500); // 500 lines each
        }

        let task = create_test_task("task-1", files, 10000);

        let estimate = estimator.estimate(&task, 5);

        assert!(estimate.exceeds_budget);
        assert!(estimate.recommended_splits.is_some());
        assert!(estimate.recommended_splits.as_ref().unwrap().len() > 1);
    }

    #[test]
    fn test_split_suggestions() {
        let mut estimator = TaskComplexityEstimator::new(ComplexityConfig {
            default_budget: 1000, // Very small budget to force splits
            ..Default::default()
        });

        let files: Vec<String> = (0..10).map(|i| format!("file{}.rs", i)).collect();
        for file in &files {
            estimator.cache_file_lines(file, 200);
        }

        let task = create_test_task("task-1", files, 5000);

        let estimate = estimator.estimate_with_budget(&task, 0, 1000);

        assert!(estimate.exceeds_budget, "Task should exceed budget");
        assert!(
            estimate.recommended_splits.is_some(),
            "Should have split suggestions"
        );

        let splits = estimate.recommended_splits.as_ref().unwrap();
        assert!(
            splits.len() >= 2,
            "Should have at least 2 splits, got {}",
            splits.len()
        );

        // Each split should have files
        for split in splits {
            assert!(!split.files.is_empty());
        }
    }

    #[test]
    fn test_apply_splits() {
        let estimator = TaskComplexityEstimator::with_default_config();

        let task = create_test_task("task-1", vec!["a.rs".to_string(), "b.rs".to_string()], 1000);

        let splits = vec![
            SplitSuggestion {
                description: "Part 1".to_string(),
                files: vec!["a.rs".to_string()],
                estimated_tokens: 500,
                depends_on: Vec::new(),
            },
            SplitSuggestion {
                description: "Part 2".to_string(),
                files: vec!["b.rs".to_string()],
                estimated_tokens: 500,
                depends_on: vec![0],
            },
        ];

        let new_tasks = estimator.apply_splits(&task, &splits);

        assert_eq!(new_tasks.len(), 2);
        assert_eq!(new_tasks[0].id, "task-1-part-1");
        assert_eq!(new_tasks[1].id, "task-1-part-2");
    }

    #[test]
    fn test_quick_split_check() {
        let small_task = create_test_task("task-1", vec!["a.rs".to_string()], 1000);
        assert!(!should_split_quick(&small_task, 100000));

        let many_files_task = create_test_task(
            "task-2",
            (0..15).map(|i| format!("f{}.rs", i)).collect(),
            1000,
        );
        assert!(should_split_quick(&many_files_task, 100000));

        let high_complexity_task = create_test_task("task-3", vec!["a.rs".to_string()], 70000);
        assert!(should_split_quick(&high_complexity_task, 100000));
    }
}
