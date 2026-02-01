use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::TaskStatus;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub phase: Option<String>,

    #[serde(default)]
    pub dependencies: Vec<String>,

    #[serde(default)]
    pub agent_type: AgentType,

    #[serde(default)]
    pub retry_count: u32,

    #[serde(default)]
    pub max_retries: u32,

    #[serde(default)]
    pub affected_files: Vec<String>,

    #[serde(default)]
    pub test_patterns: Vec<String>,

    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub result: Option<TaskResult>,

    #[serde(default)]
    pub learnings: Vec<String>,

    #[serde(default)]
    pub module: Option<String>,

    /// Scope reduction factor for retry with reduced chunks (1.0 = full scope, 0.5 = half scope).
    /// Applied to context budget during execution.
    #[serde(default = "default_scope_factor")]
    pub scope_factor: f32,
}

fn default_scope_factor() -> f32 {
    1.0
}

impl Task {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            status: TaskStatus::Pending,
            phase: None,
            dependencies: Vec::new(),
            agent_type: AgentType::General,
            retry_count: 0,
            max_retries: 3,
            affected_files: Vec::new(),
            test_patterns: Vec::new(),
            started_at: None,
            completed_at: None,
            result: None,
            learnings: Vec::new(),
            module: None,
            scope_factor: 1.0,
        }
    }

    pub fn with_phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = Some(phase.into());
        self
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn with_agent_type(mut self, agent_type: AgentType) -> Self {
        self.agent_type = agent_type;
        self
    }

    pub fn with_affected_files(mut self, files: Vec<String>) -> Self {
        self.affected_files = files;
        self
    }

    pub fn with_test_patterns(mut self, patterns: Vec<String>) -> Self {
        self.test_patterns = patterns;
        self
    }

    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Checks if task can start. Dependencies must be in terminal success states (Completed or Skipped).
    pub fn can_start(&self, satisfied_deps: &[&str]) -> bool {
        self.status == TaskStatus::Pending
            && self
                .dependencies
                .iter()
                .all(|dep| satisfied_deps.contains(&dep.as_str()))
    }

    pub fn start(&mut self) {
        self.status = TaskStatus::InProgress;
        self.started_at = Some(Utc::now());
    }

    pub fn complete(&mut self, result: TaskResult) {
        self.status = TaskStatus::Completed;
        self.completed_at = Some(Utc::now());
        self.result = Some(result);
    }

    pub fn fail(&mut self, error: String) {
        self.retry_count += 1;
        if self.retry_count > self.max_retries {
            self.status = TaskStatus::Failed;
        } else {
            self.status = TaskStatus::Pending;
        }

        // Preserve existing file information on failure
        if let Some(ref mut result) = self.result {
            result.success = false;
            result.output = error;
            // Keep files_modified, files_created, and token counts intact
        } else {
            self.result = Some(TaskResult {
                success: false,
                output: error,
                files_modified: Vec::new(),
                files_created: Vec::new(),
                files_deleted: Vec::new(),
                input_tokens: None,
                output_tokens: None,
            });
        }
    }

    /// Marks task as skipped with a reason.
    /// Unlike fail(), this sets success=true since skipping is an intentional decision.
    pub fn skip(&mut self, reason: String) {
        self.status = TaskStatus::Skipped;
        self.completed_at = Some(Utc::now());
        self.result = Some(TaskResult {
            success: true, // Skipping is intentional, not a failure
            output: format!("Skipped: {}", reason),
            files_modified: Vec::new(),
            files_created: Vec::new(),
            files_deleted: Vec::new(),
            input_tokens: None,
            output_tokens: None,
        });
    }

    /// Updates file change information without affecting other result fields.
    pub fn update_file_changes(&mut self, created: Vec<String>, modified: Vec<String>) {
        if let Some(ref mut result) = self.result {
            result.files_created = created;
            result.files_modified = modified;
        } else {
            self.result = Some(TaskResult {
                success: false,
                output: String::new(),
                files_modified: modified,
                files_created: created,
                files_deleted: Vec::new(),
                input_tokens: None,
                output_tokens: None,
            });
        }
    }

    pub fn can_retry(&self) -> bool {
        self.retry_count <= self.max_retries
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    #[default]
    General,
    Coder,
    Reviewer,
    Tester,
    Debugger,
    Architect,
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::General => write!(f, "general"),
            Self::Coder => write!(f, "coder"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Tester => write!(f, "tester"),
            Self::Debugger => write!(f, "debugger"),
            Self::Architect => write!(f, "architect"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskResult {
    pub success: bool,
    pub output: String,
    pub files_modified: Vec<String>,
    pub files_created: Vec<String>,
    /// Files deleted during task execution (from FileTracker).
    #[serde(default)]
    pub files_deleted: Vec<String>,
    /// Input tokens used (from SDK execution)
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens used (from SDK execution)
    #[serde(default)]
    pub output_tokens: Option<u64>,
}
