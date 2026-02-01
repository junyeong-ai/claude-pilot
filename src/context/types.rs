use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap, VecDeque};

use crate::config::{ContextConfig, ContextLimitsConfig};
use crate::mission::Progress;
use crate::planning::Evidence;
use crate::recovery::ReasoningContext;
use crate::utils::truncate_chars;
use crate::verification::VerificationArchive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionContext {
    pub mission_id: String,
    pub summary: MissionSummary,
    pub phase_summaries: Vec<PhaseSummary>,
    pub task_registry: TaskRegistry,
    pub token_budget: super::TokenBudget,
    pub compaction_state: CompactionState,
    pub last_updated_at: DateTime<Utc>,
    /// Evidence snapshot for checkpoint persistence.
    /// Stored during planning to enable durable recovery without re-gathering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_snapshot: Option<Evidence>,
    /// Reasoning context for durable execution (hypothesis/decision tracking).
    /// Enables understanding "why" decisions were made after long breaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_context: Option<ReasoningContext>,
    /// Verification archive for learned patterns and fix history.
    /// Enables avoiding repeated failed strategies across checkpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_archive: Option<VerificationArchive>,
}

impl MissionContext {
    pub fn new(mission_id: impl Into<String>) -> Self {
        Self {
            mission_id: mission_id.into(),
            summary: MissionSummary::default(),
            phase_summaries: Vec::new(),
            task_registry: TaskRegistry::default(),
            token_budget: super::TokenBudget::default(),
            compaction_state: CompactionState::default(),
            last_updated_at: Utc::now(),
            evidence_snapshot: None,
            reasoning_context: None,
            verification_archive: None,
        }
    }

    /// Set evidence snapshot for checkpoint persistence.
    pub fn set_evidence(&mut self, evidence: Evidence) {
        self.evidence_snapshot = Some(evidence);
    }

    /// Get reasoning context if present.
    pub fn reasoning_context(&self) -> Option<&ReasoningContext> {
        self.reasoning_context.as_ref()
    }

    /// Set reasoning context for durable execution.
    pub fn set_reasoning_context(&mut self, context: ReasoningContext) {
        self.reasoning_context = Some(context);
    }

    /// Get or initialize reasoning context for the mission.
    pub fn reasoning_context_mut(&mut self) -> &mut ReasoningContext {
        let mission_id = self.mission_id.clone();
        self.reasoning_context
            .get_or_insert_with(|| ReasoningContext::new(&mission_id))
    }

    /// Get verification archive if present.
    pub fn verification_archive(&self) -> Option<&VerificationArchive> {
        self.verification_archive.as_ref()
    }

    /// Set verification archive for learned patterns.
    pub fn set_verification_archive(&mut self, archive: VerificationArchive) {
        self.verification_archive = Some(archive);
    }

    /// Get or initialize verification archive for the mission.
    pub fn verification_archive_mut(&mut self) -> &mut VerificationArchive {
        let mission_id = self.mission_id.clone();
        self.verification_archive
            .get_or_insert_with(|| VerificationArchive::new(&mission_id))
    }

    pub fn configure_budget(&mut self, max_tokens: usize, config: &ContextConfig) {
        self.token_budget = super::TokenBudget::with_config(
            max_tokens,
            config.compaction.compaction_threshold,
            config.budget_allocation.clone(),
            config.phase_complexity.clone(),
        );
    }

    pub fn update_timestamp(&mut self) {
        self.last_updated_at = Utc::now();
    }

    pub fn current_phase(&self) -> Option<&PhaseSummary> {
        self.phase_summaries
            .iter()
            .find(|p| p.status == PhaseStatus::InProgress)
    }

    pub fn completed_phases(&self) -> impl Iterator<Item = &PhaseSummary> {
        self.phase_summaries
            .iter()
            .filter(|p| p.status == PhaseStatus::Completed)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MissionSummary {
    pub objective: String,
    pub progress: Progress,
    pub current_phase: String,
    pub blockers: Vec<String>,
    pub critical_learnings: VecDeque<String>,
    pub key_decisions: VecDeque<String>,
}

impl MissionSummary {
    pub fn set_objective(&mut self, objective: impl Into<String>, limits: &ContextLimitsConfig) {
        self.objective = truncate_chars(&objective.into(), limits.max_objective_chars);
    }

    pub fn add_learning(&mut self, learning: impl Into<String>, limits: &ContextLimitsConfig) {
        let l = learning.into();
        if !self.critical_learnings.contains(&l) {
            if self.critical_learnings.len() >= limits.max_critical_learnings {
                self.critical_learnings.pop_front();
            }
            self.critical_learnings.push_back(l);
        }
    }

    pub fn add_decision(&mut self, decision: impl Into<String>, limits: &ContextLimitsConfig) {
        let d = decision.into();
        if !self.key_decisions.contains(&d) {
            if self.key_decisions.len() >= limits.max_key_decisions {
                self.key_decisions.pop_front();
            }
            self.key_decisions.push_back(d);
        }
    }

    pub fn add_blocker(&mut self, blocker: impl Into<String>) {
        let b = blocker.into();
        if !self.blockers.contains(&b) {
            self.blockers.push(b);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    pub phase_id: String,
    pub name: String,
    pub status: PhaseStatus,
    pub task_count: usize,
    pub completed_count: usize,
    pub one_line_summary: String,
    pub key_changes: Vec<String>,
    pub files_modified: Vec<String>,
    pub learnings: Vec<String>,
    pub verification_passed: bool,
}

impl PhaseSummary {
    pub fn new(phase_id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            phase_id: phase_id.into(),
            name: name.into(),
            status: PhaseStatus::Pending,
            task_count: 0,
            completed_count: 0,
            one_line_summary: String::new(),
            key_changes: Vec::new(),
            files_modified: Vec::new(),
            learnings: Vec::new(),
            verification_passed: false,
        }
    }

    pub fn add_change(&mut self, change: impl Into<String>, limits: &ContextLimitsConfig) {
        let c = change.into();
        if !self.key_changes.contains(&c) && self.key_changes.len() < limits.max_phase_changes {
            self.key_changes.push(c);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::InProgress => write!(f, "in progress"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Registry of task context data for LLM context building.
/// Does NOT store execution state - use Mission.tasks for status/retry_count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskRegistry {
    pub tasks: Vec<TaskEntry>,
    pub dependency_graph: DependencyGraph,
    pub execution_order: Vec<String>,
}

impl TaskRegistry {
    pub fn add_task(&mut self, entry: TaskEntry) {
        for dep in &entry.dependencies {
            self.dependency_graph
                .add_edge(dep.clone(), entry.id.clone());
        }
        self.tasks.push(entry);
    }

    pub fn task(&self, id: &str) -> Option<&TaskEntry> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn task_mut(&mut self, id: &str) -> Option<&mut TaskEntry> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn tasks_with_summaries(&self) -> impl Iterator<Item = &TaskEntry> {
        self.tasks.iter().filter(|t| t.summary.is_some())
    }

    pub fn compute_execution_order(&mut self) {
        self.execution_order = self.dependency_graph.topological_sort();
    }
}

/// Context-only task data for LLM context building.
/// Execution state (status, retry_count) is in Mission.tasks, not here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEntry {
    pub id: String,
    pub phase: String,
    pub description: String,
    pub one_line: String,
    /// Dependencies are stored here for context building (topological sort, dependency chain display)
    pub dependencies: Vec<String>,
    /// AI-generated summary of completed work (context-only)
    pub summary: Option<String>,
    /// Files affected by this task (context-only, for LLM awareness)
    pub files_changed: Vec<String>,
    /// Verification outcome (context-only, for LLM awareness)
    pub verification_status: VerificationStatus,
    /// Last error message (context-only, for LLM debugging hints)
    pub last_error: Option<String>,
}

impl TaskEntry {
    pub fn new(
        id: impl Into<String>,
        phase: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let desc = description.into();
        let one_line = truncate_chars(&desc, 80);

        Self {
            id: id.into(),
            phase: phase.into(),
            description: desc,
            one_line,
            dependencies: Vec::new(),
            summary: None,
            files_changed: Vec::new(),
            verification_status: VerificationStatus::Pending,
            last_error: None,
        }
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn set_summary(&mut self, summary: impl Into<String>) {
        self.summary = Some(truncate_chars(&summary.into(), 100));
    }

    pub fn add_file(&mut self, file: impl Into<String>) {
        let f = file.into();
        if !self.files_changed.contains(&f) {
            self.files_changed.push(f);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    #[default]
    Pending,
    Passed,
    Failed,
    Skipped,
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Passed => write!(f, "passed"),
            Self::Failed => write!(f, "failed"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyGraph {
    edges: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    pub fn add_edge(&mut self, from: String, to: String) {
        self.edges.entry(from).or_default().push(to);
    }

    /// Performs topological sort with cycle detection.
    /// Returns tasks in dependency order (dependencies first).
    /// Uses Kahn's algorithm with O(V + E) complexity.
    ///
    /// If circular dependencies exist, returns the sorted nodes that can be executed,
    /// followed by the nodes involved in cycles (in arbitrary order).
    /// Logs a warning with the specific nodes that form cycles.
    pub fn topological_sort(&self) -> Vec<String> {
        use std::cmp::Reverse;
        use std::collections::HashSet;

        let mut all_nodes: HashSet<String> = self.edges.keys().cloned().collect();
        for deps in self.edges.values() {
            all_nodes.extend(deps.iter().cloned());
        }

        let mut in_degree: HashMap<String, usize> =
            all_nodes.iter().map(|n| (n.clone(), 0)).collect();
        for deps in self.edges.values() {
            for dep in deps {
                *in_degree.entry(dep.clone()).or_default() += 1;
            }
        }

        let mut heap: BinaryHeap<Reverse<String>> = in_degree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| Reverse(n.clone()))
            .collect();

        let mut result = Vec::with_capacity(all_nodes.len());
        while let Some(Reverse(node)) = heap.pop() {
            // Process dependencies before moving node to result
            if let Some(deps) = self.edges.get(&node) {
                for dep in deps {
                    if let Some(d) = in_degree.get_mut(dep) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            heap.push(Reverse(dep.clone()));
                        }
                    }
                }
            }
            result.push(node); // Move instead of clone
        }

        if result.len() != all_nodes.len() {
            // Identify nodes involved in cycles using reference set to avoid cloning
            let sorted_set: HashSet<&String> = result.iter().collect();
            let cyclic_nodes: Vec<_> = all_nodes
                .iter()
                .filter(|n| !sorted_set.contains(n))
                .cloned()
                .collect();

            tracing::error!(
                cyclic_nodes = ?cyclic_nodes,
                "Circular dependency detected. These tasks cannot be executed: {:?}",
                cyclic_nodes
            );

            // Append cyclic nodes to the end (they will fail but at least be visible)
            result.extend(cyclic_nodes);
        }

        result
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionState {
    pub total_compactions: u32,
    pub last_compaction: Option<DateTime<Utc>>,
    pub compression_ratio: f32,
    pub original_tokens: usize,
    pub current_tokens: usize,
}

impl CompactionState {
    pub fn record_compaction(&mut self, original: usize, final_tokens: usize) {
        self.total_compactions += 1;
        self.last_compaction = Some(Utc::now());
        self.original_tokens = original;
        self.current_tokens = final_tokens;
        self.compression_ratio = if original > 0 {
            final_tokens as f32 / original as f32
        } else {
            1.0
        };
    }
}
