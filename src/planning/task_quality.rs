use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::TaskDecompositionConfig;

use super::artifacts::PlannedTask;
use super::graph::detect_cycle;

#[derive(Debug)]
pub struct TaskQualityResult {
    pub passed: bool,
    pub score: f32,
    pub issues: Vec<TaskQualityIssue>,
}

impl TaskQualityResult {
    pub fn format_feedback(&self) -> String {
        let mut feedback = format!(
            "Previous attempt scored {:.2}. Issues to fix:\n",
            self.score
        );
        for issue in &self.issues {
            feedback.push_str(&format!("- {} → {}\n", issue, issue.suggested_fix()));
        }
        feedback
    }
}

#[derive(Debug)]
pub enum TaskQualityIssue {
    TooFewTasks {
        story_id: String,
        count: usize,
        min: usize,
    },
    TooManyTasks {
        story_id: String,
        count: usize,
        max: usize,
    },
    MissingStoryCoverage {
        story_id: String,
    },
    EmptyAffectedFiles {
        task_id: String,
    },
    InvalidDirectoryPath {
        task_id: String,
        path: String,
    },
    DuplicateTaskId {
        task_id: String,
    },
    VagueDescription {
        task_id: String,
        description: String,
    },
    MissingDependency {
        task_id: String,
        missing: String,
    },
    CircularDependency {
        task_ids: Vec<String>,
    },
}

impl std::fmt::Display for TaskQualityIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewTasks {
                story_id,
                count,
                min,
            } => {
                write!(
                    f,
                    "Story '{}' has {} tasks (minimum: {})",
                    story_id, count, min
                )
            }
            Self::TooManyTasks {
                story_id,
                count,
                max,
            } => {
                write!(
                    f,
                    "Story '{}' has {} tasks (maximum: {})",
                    story_id, count, max
                )
            }
            Self::MissingStoryCoverage { story_id } => {
                write!(
                    f,
                    "User story '{}' has no tasks linked (set user_story field)",
                    story_id
                )
            }
            Self::EmptyAffectedFiles { task_id } => {
                write!(f, "Task '{}' has empty affected_files", task_id)
            }
            Self::InvalidDirectoryPath { task_id, path } => {
                write!(
                    f,
                    "Task '{}' has directory '{}' in affected_files (should be files only)",
                    task_id, path
                )
            }
            Self::DuplicateTaskId { task_id } => {
                write!(f, "Duplicate task ID: '{}'", task_id)
            }
            Self::VagueDescription {
                task_id,
                description,
            } => {
                write!(
                    f,
                    "Task '{}' has vague description: '{}'",
                    task_id, description
                )
            }
            Self::MissingDependency { task_id, missing } => {
                write!(
                    f,
                    "Task '{}' depends on non-existent task '{}'",
                    task_id, missing
                )
            }
            Self::CircularDependency { task_ids } => {
                write!(f, "Circular dependency detected: {:?}", task_ids)
            }
        }
    }
}

impl TaskQualityIssue {
    pub fn suggested_fix(&self) -> &'static str {
        match self {
            Self::TooFewTasks { .. } => "Add more granular tasks to cover the story scope",
            Self::TooManyTasks { .. } => "Consolidate related tasks or split into separate stories",
            Self::MissingStoryCoverage { .. } => "Add user_story field to at least one task",
            Self::EmptyAffectedFiles { .. } => "Specify expected files to modify or create",
            Self::InvalidDirectoryPath { .. } => "Replace directory with specific file paths",
            Self::DuplicateTaskId { .. } => "Rename to use unique task IDs",
            Self::VagueDescription { .. } => "Add specific acceptance criteria",
            Self::MissingDependency { .. } => {
                "Either add the missing task or remove the dependency"
            }
            Self::CircularDependency { .. } => "Remove one dependency to break the cycle",
        }
    }
}

pub struct TaskQualityValidator {
    config: TaskDecompositionConfig,
    working_dir: Option<PathBuf>,
}

impl TaskQualityValidator {
    pub fn new(config: &TaskDecompositionConfig) -> Self {
        Self {
            config: config.clone(),
            working_dir: None,
        }
    }

    pub fn with_working_dir(mut self, working_dir: &Path) -> Self {
        self.working_dir = Some(working_dir.to_path_buf());
        self
    }

    pub fn validate(&self, tasks: &[PlannedTask], story_ids: &[String]) -> TaskQualityResult {
        let mut issues = Vec::new();
        let mut scores = Vec::new();

        let story_count = story_ids.len().max(1);
        self.check_task_count(tasks, story_count, &mut issues, &mut scores);
        self.check_story_coverage(tasks, story_ids, &mut issues, &mut scores);
        self.check_affected_files(tasks, &mut issues, &mut scores);
        self.check_duplicate_ids(tasks, &mut issues, &mut scores);
        self.check_descriptions(tasks, &mut issues, &mut scores);
        self.check_dependencies(tasks, &mut issues, &mut scores);
        self.check_cycles(tasks, &mut issues, &mut scores);

        let avg_score = scores.iter().sum::<f32>() / scores.len().max(1) as f32;
        let passed = avg_score >= self.config.quality_threshold && issues.is_empty();

        TaskQualityResult {
            passed,
            score: avg_score,
            issues,
        }
    }

    fn check_story_coverage(
        &self,
        tasks: &[PlannedTask],
        story_ids: &[String],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        for story_id in story_ids {
            let has_task = tasks
                .iter()
                .any(|t| t.user_story.as_ref() == Some(story_id));

            if has_task {
                scores.push(1.0);
            } else {
                issues.push(TaskQualityIssue::MissingStoryCoverage {
                    story_id: story_id.clone(),
                });
                scores.push(0.0);
            }
        }
    }

    fn check_task_count(
        &self,
        tasks: &[PlannedTask],
        story_count: usize,
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        let avg_tasks = tasks.len() as f32 / story_count.max(1) as f32;

        if avg_tasks < self.config.min_tasks_per_story as f32 {
            issues.push(TaskQualityIssue::TooFewTasks {
                story_id: "overall".to_string(),
                count: tasks.len(),
                min: self.config.min_tasks_per_story * story_count.max(1),
            });
            scores.push(0.5);
        } else if avg_tasks > self.config.max_tasks_per_story as f32 {
            issues.push(TaskQualityIssue::TooManyTasks {
                story_id: "overall".to_string(),
                count: tasks.len(),
                max: self.config.max_tasks_per_story * story_count.max(1),
            });
            scores.push(0.7);
        } else {
            scores.push(1.0);
        }
    }

    fn check_affected_files(
        &self,
        tasks: &[PlannedTask],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        if !self.config.require_affected_files {
            return;
        }

        for task in tasks {
            if task.affected_files.is_empty() {
                issues.push(TaskQualityIssue::EmptyAffectedFiles {
                    task_id: task.id.clone(),
                });
                scores.push(0.3);
                continue;
            }

            let mut task_passed = true;
            for file in &task.affected_files {
                // Check 1: Path ending with '/' is explicitly a directory
                if file.ends_with('/') {
                    issues.push(TaskQualityIssue::InvalidDirectoryPath {
                        task_id: task.id.clone(),
                        path: file.clone(),
                    });
                    task_passed = false;
                    continue;
                }

                // Check 2: If working_dir is set, verify on filesystem
                if let Some(ref working_dir) = self.working_dir {
                    let full_path = working_dir.join(file);
                    if full_path.exists() && full_path.is_dir() {
                        issues.push(TaskQualityIssue::InvalidDirectoryPath {
                            task_id: task.id.clone(),
                            path: file.clone(),
                        });
                        task_passed = false;
                    }
                }
            }

            scores.push(if task_passed { 1.0 } else { 0.3 });
        }
    }

    fn check_duplicate_ids(
        &self,
        tasks: &[PlannedTask],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        let mut seen = HashSet::new();
        for task in tasks {
            if !seen.insert(&task.id) {
                issues.push(TaskQualityIssue::DuplicateTaskId {
                    task_id: task.id.clone(),
                });
                scores.push(0.0);
            }
        }
    }

    /// Check task descriptions for quality.
    fn check_descriptions(
        &self,
        tasks: &[PlannedTask],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        for task in tasks {
            // Only flag truly empty or extremely short descriptions
            // Use char count for proper Unicode handling (CJK characters, etc.)
            let char_count = task.description.trim().chars().count();
            if char_count < 3 {
                issues.push(TaskQualityIssue::VagueDescription {
                    task_id: task.id.clone(),
                    description: task.description.clone(),
                });
                scores.push(0.4);
            } else {
                scores.push(1.0);
            }
        }
    }

    fn check_dependencies(
        &self,
        tasks: &[PlannedTask],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        let all_ids: HashSet<_> = tasks.iter().map(|t| &t.id).collect();

        for task in tasks {
            for dep in &task.dependencies {
                if !all_ids.contains(dep) {
                    issues.push(TaskQualityIssue::MissingDependency {
                        task_id: task.id.clone(),
                        missing: dep.clone(),
                    });
                    scores.push(0.2);
                }
            }
        }
    }

    fn check_cycles(
        &self,
        tasks: &[PlannedTask],
        issues: &mut Vec<TaskQualityIssue>,
        scores: &mut Vec<f32>,
    ) {
        let deps = tasks
            .iter()
            .map(|t| (t.id.clone(), t.dependencies.clone()))
            .collect();

        if let Some(cycle) = detect_cycle(&deps) {
            issues.push(TaskQualityIssue::CircularDependency { task_ids: cycle });
            scores.push(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_task(id: &str, affected: &[&str], deps: &[&str]) -> PlannedTask {
        PlannedTask {
            id: id.to_string(),
            description: format!("Implement {}", id),
            user_story: None,
            parallel: false,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            affected_files: affected.iter().map(|s| s.to_string()).collect(),
            test_patterns: Vec::new(),
        }
    }

    #[test]
    fn valid_tasks() {
        let config = TaskDecompositionConfig::default();
        let validator = TaskQualityValidator::new(&config);

        let tasks = vec![
            build_task("T001", &["src/foo.rs"], &[]),
            build_task("T002", &["src/bar.rs"], &["T001"]),
            build_task("T003", &["src/baz.rs"], &["T001"]),
        ];

        // No story IDs means no story coverage check
        let result = validator.validate(&tasks, &[]);
        assert!(result.passed);
        assert!(result.issues.is_empty());
    }

    #[test]
    fn empty_affected_files() {
        let config = TaskDecompositionConfig::default();
        let validator = TaskQualityValidator::new(&config);

        let tasks = vec![
            build_task("T001", &[], &[]),
            build_task("T002", &["src/bar.rs"], &["T001"]),
        ];

        let result = validator.validate(&tasks, &[]);
        assert!(!result.passed);
        assert!(
            result
                .issues
                .iter()
                .any(|i| matches!(i, TaskQualityIssue::EmptyAffectedFiles { .. }))
        );
    }

    #[test]
    fn duplicate_task_id() {
        let config = TaskDecompositionConfig {
            require_affected_files: false,
            ..Default::default()
        };
        let validator = TaskQualityValidator::new(&config);

        let tasks = vec![build_task("T001", &[], &[]), build_task("T001", &[], &[])];

        let result = validator.validate(&tasks, &[]);
        assert!(!result.passed);
        assert!(
            result
                .issues
                .iter()
                .any(|i| matches!(i, TaskQualityIssue::DuplicateTaskId { .. }))
        );
    }

    #[test]
    fn circular_dependency() {
        let config = TaskDecompositionConfig {
            require_affected_files: false,
            ..Default::default()
        };
        let validator = TaskQualityValidator::new(&config);

        let tasks = vec![
            build_task("T001", &[], &["T003"]),
            build_task("T002", &[], &["T001"]),
            build_task("T003", &[], &["T002"]),
        ];

        let result = validator.validate(&tasks, &[]);
        assert!(!result.passed);
        assert!(
            result
                .issues
                .iter()
                .any(|i| matches!(i, TaskQualityIssue::CircularDependency { .. }))
        );
    }

    #[test]
    fn missing_story_coverage() {
        let config = TaskDecompositionConfig {
            require_affected_files: false,
            ..Default::default()
        };
        let validator = TaskQualityValidator::new(&config);

        let mut task = build_task("T001", &[], &[]);
        task.user_story = Some("us_001".to_string());

        let tasks = vec![task];
        let story_ids = vec!["us_001".to_string(), "us_002".to_string()];

        let result = validator.validate(&tasks, &story_ids);
        assert!(!result.passed);
        assert!(result.issues.iter().any(|i| matches!(
            i,
            TaskQualityIssue::MissingStoryCoverage { story_id } if story_id == "us_002"
        )));
    }

    #[test]
    fn directory_path_with_trailing_slash() {
        let config = TaskDecompositionConfig::default();
        let validator = TaskQualityValidator::new(&config);

        let tasks = vec![build_task("T001", &["src/"], &[])];

        let result = validator.validate(&tasks, &[]);
        assert!(!result.passed);
        assert!(result.issues.iter().any(|i| matches!(
            i,
            TaskQualityIssue::InvalidDirectoryPath { task_id, path }
                if task_id == "T001" && path == "src/"
        )));
    }

    #[test]
    fn directory_detection_with_working_dir() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();

        let config = TaskDecompositionConfig::default();
        let validator = TaskQualityValidator::new(&config).with_working_dir(dir.path());

        // Task with actual directory path (not file)
        let tasks = vec![build_task("T001", &["src"], &[])];

        let result = validator.validate(&tasks, &[]);
        assert!(!result.passed);
        assert!(result.issues.iter().any(|i| matches!(
            i,
            TaskQualityIssue::InvalidDirectoryPath { task_id, .. }
                if task_id == "T001"
        )));
    }

    #[test]
    fn valid_file_paths_pass() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src_dir.join("lib.rs"), "pub fn foo() {}").unwrap();

        let config = TaskDecompositionConfig {
            min_tasks_per_story: 1, // Allow single task for this test
            ..Default::default()
        };
        let validator = TaskQualityValidator::new(&config).with_working_dir(dir.path());

        // Task with actual file path
        let tasks = vec![build_task("T001", &["src/main.rs", "src/lib.rs"], &[])];

        let result = validator.validate(&tasks, &[]);
        assert!(result.passed, "Unexpected issues: {:?}", result.issues);
        assert!(result.issues.is_empty());
    }
}
