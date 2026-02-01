use std::path::PathBuf;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Task, TaskStatus};
use crate::learning::ExtractionResult;
use crate::planning::ComplexityTier;
use crate::recovery::EscalationContext;
use crate::state::{MissionState, StateTransition};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: String,
    pub description: String,
    pub status: MissionState,
    pub isolation: IsolationMode,

    pub branch: Option<String>,
    pub worktree_path: Option<PathBuf>,
    pub base_branch: String,

    #[serde(default)]
    pub tasks: Vec<Task>,

    #[serde(default)]
    pub phases: Vec<Phase>,

    pub priority: Priority,
    pub on_complete: OnComplete,

    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(default)]
    pub learnings: Vec<Learning>,

    #[serde(default)]
    pub iteration: u32,

    #[serde(default)]
    pub max_iterations: u32,

    #[serde(default)]
    pub flags: MissionFlags,

    #[serde(default)]
    pub state_history: Vec<StateTransition>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_candidates: Option<ExtractionResult>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_complexity: Option<ComplexityTier>,

    /// Active escalation context when mission is in Escalated state.
    /// Persisted for durable resume with full human intervention context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<EscalationContext>,
}

impl Mission {
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            status: MissionState::Pending,
            isolation: IsolationMode::Auto,
            branch: None,
            worktree_path: None,
            base_branch: String::from("main"),
            tasks: Vec::new(),
            phases: Vec::new(),
            priority: Priority::P2,
            on_complete: OnComplete::Manual,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            learnings: Vec::new(),
            iteration: 0,
            max_iterations: 100,
            flags: MissionFlags::default(),
            state_history: Vec::new(),
            extracted_candidates: None,
            estimated_complexity: None,
            escalation: None,
        }
    }

    pub fn with_isolation(mut self, isolation: IsolationMode) -> Self {
        self.isolation = isolation;
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_on_complete(mut self, on_complete: OnComplete) -> Self {
        self.on_complete = on_complete;
        self
    }

    pub fn with_base_branch(mut self, branch: impl Into<String>) -> Self {
        self.base_branch = branch.into();
        self
    }

    pub fn is_complete(&self) -> bool {
        self.status == MissionState::Completed
    }

    /// Returns true if all tasks are in a terminal state (Completed, Skipped, or permanently Failed)
    pub fn all_tasks_done(&self) -> bool {
        !self.tasks.is_empty()
            && self.tasks.iter().all(|t| {
                t.status == TaskStatus::Completed
                    || t.status == TaskStatus::Skipped
                    || (t.status == TaskStatus::Failed && !t.can_retry())
            })
    }

    /// Returns failed tasks that cannot be retried.
    pub fn permanently_failed_tasks(&self) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed && !t.can_retry())
            .collect()
    }

    pub fn progress(&self) -> Progress {
        let total = self.tasks.len();
        // Count all terminal states: Completed, Skipped, or permanently Failed
        let done = self
            .tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Completed
                    || t.status == TaskStatus::Skipped
                    || (t.status == TaskStatus::Failed && !t.can_retry())
            })
            .count();

        Progress {
            completed: done,
            total,
            percentage: if total > 0 {
                crate::utils::ratio_to_percent_u8(done as f64 / total as f64)
            } else {
                0
            },
        }
    }

    /// Returns tasks that are ready to start (dependencies satisfied, pending status).
    pub fn next_tasks(&self) -> Vec<&Task> {
        // Include both Completed and Skipped as "satisfied" dependencies
        // Skipped tasks are intentionally not executed, so they shouldn't block dependents
        let satisfied: Vec<&str> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed || t.status == TaskStatus::Skipped)
            .map(|t| t.id.as_str())
            .collect();

        self.tasks
            .iter()
            .filter(|t| t.can_start(&satisfied))
            .collect()
    }

    /// Returns pending tasks that have at least one permanently failed dependency.
    /// These tasks can never start because their dependencies will never be satisfied.
    pub fn tasks_blocked_by_failed_deps(&self) -> Vec<(&Task, Vec<&str>)> {
        let permanently_failed: Vec<&str> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed && !t.can_retry())
            .map(|t| t.id.as_str())
            .collect();

        if permanently_failed.is_empty() {
            return Vec::new();
        }

        self.tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .filter_map(|t| {
                let failed_deps: Vec<&str> = t
                    .dependencies
                    .iter()
                    .filter(|dep| permanently_failed.contains(&dep.as_str()))
                    .map(|s| s.as_str())
                    .collect();

                if failed_deps.is_empty() {
                    None
                } else {
                    Some((t, failed_deps))
                }
            })
            .collect()
    }

    /// Returns currently active (in-progress) tasks.
    pub fn active_tasks(&self) -> Vec<&Task> {
        self.tasks.iter().filter(|t| t.status.is_active()).collect()
    }

    pub fn add_task(&mut self, task: Task) {
        self.tasks.push(task);
    }

    pub fn task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == task_id)
    }

    pub fn task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
    }

    pub fn add_learning(&mut self, learning: Learning) {
        self.learnings.push(learning);
    }

    pub fn branch_name(&self) -> String {
        format!("pilot/{}", self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    #[default]
    Auto,
    None,
    Branch,
    Worktree,
}

impl IsolationMode {
    /// Escalate to a more isolated mode.
    ///
    /// None -> Branch -> Worktree
    /// Auto treated as Branch for escalation.
    pub fn escalate(self) -> Self {
        match self {
            Self::None | Self::Auto => Self::Branch,
            Self::Branch => Self::Worktree,
            Self::Worktree => Self::Worktree,
        }
    }
}

impl std::fmt::Display for IsolationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::None => write!(f, "none"),
            Self::Branch => write!(f, "branch"),
            Self::Worktree => write!(f, "worktree"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P1,
    #[default]
    P2,
    P3,
    P4,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::P1 => write!(f, "P1"),
            Self::P2 => write!(f, "P2"),
            Self::P3 => write!(f, "P3"),
            Self::P4 => write!(f, "P4"),
        }
    }
}

impl std::str::FromStr for Priority {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "p1" | "high" | "1" => Ok(Self::P1),
            "p2" | "normal" | "2" => Ok(Self::P2),
            "p3" | "low" | "3" => Ok(Self::P3),
            "p4" | "4" => Ok(Self::P4),
            _ => Err(format!("Invalid priority: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnComplete {
    Direct,
    #[default]
    Manual,
    PullRequest {
        auto_merge: bool,
        reviewers: Vec<String>,
    },
}

impl std::fmt::Display for OnComplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct => write!(f, "direct"),
            Self::Manual => write!(f, "manual"),
            Self::PullRequest { auto_merge, .. } => {
                if *auto_merge {
                    write!(f, "pr (auto-merge)")
                } else {
                    write!(f, "pr")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub description: String,
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Progress {
    pub completed: usize,
    pub total: usize,
    pub percentage: u8,
}

impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}% ({}/{})",
            self.percentage, self.completed, self.total
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learning {
    pub id: String,
    pub category: LearningCategory,
    pub content: String,
    pub source_task: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Learning {
    pub fn new(category: LearningCategory, content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            category,
            content: content.into(),
            source_task: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_source_task(mut self, task_id: impl Into<String>) -> Self {
        self.source_task = Some(task_id.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningCategory {
    Pattern,
    Gotcha,
    BestPractice,
    Workaround,
    Decision,
}

impl std::fmt::Display for LearningCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pattern => write!(f, "Pattern"),
            Self::Gotcha => write!(f, "Gotcha"),
            Self::BestPractice => write!(f, "Best Practice"),
            Self::Workaround => write!(f, "Workaround"),
            Self::Decision => write!(f, "Decision"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MissionFlags {
    pub isolated: bool,
    pub direct: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    #[default]
    Medium,
    High,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "Low"),
            Self::Medium => write!(f, "Medium"),
            Self::High => write!(f, "High"),
        }
    }
}
