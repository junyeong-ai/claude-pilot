//! Mission orchestration for multi-workspace task execution.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{debug, info};

use super::scope::AgentScope;
use crate::agent::multi::consensus::{
    ConsensusEngine, ConsensusResult, ConsensusTask, TaskComplexity,
};
use crate::agent::multi::shared::AgentId;
use crate::agent::multi::{
    AgentPool, AgentRole, AgentTask, AgentTaskResult, TaskContext, TaskPriority,
};
use crate::error::{PilotError, Result};
use crate::workspace::WorkspaceSet;

/// A mission to be executed across workspaces.
#[derive(Debug, Clone)]
pub struct Mission {
    pub id: String,
    pub description: String,
    pub affected_files: Vec<PathBuf>,
    pub priority: TaskPriority,
}

impl Mission {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            description: description.into(),
            affected_files: Vec::new(),
            priority: TaskPriority::Normal,
        }
    }

    pub fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.affected_files = files;
        self
    }

    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }
}

/// Result of mission execution.
#[derive(Debug)]
pub struct MissionResult {
    pub mission_id: String,
    pub status: MissionStatus,
    pub results: Vec<AgentTaskResult>,
    pub scopes_executed: Vec<AgentScope>,
}

/// Status of mission execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionStatus {
    Success,
    PartialSuccess { completed: usize, total: usize },
    Failed { reason: String },
}

/// Orchestrates mission execution across workspaces.
pub struct MissionOrchestrator {
    workspaces: Arc<WorkspaceSet>,
    pool: Arc<AgentPool>,
    consensus: Option<ConsensusEngine>,
}

impl MissionOrchestrator {
    pub fn new(workspaces: WorkspaceSet, pool: Arc<AgentPool>) -> Self {
        Self {
            workspaces: Arc::new(workspaces),
            pool,
            consensus: None,
        }
    }

    pub fn with_consensus(mut self, engine: ConsensusEngine) -> Self {
        self.consensus = Some(engine);
        self
    }

    /// Execute a mission using 4-phase workflow.
    pub async fn execute(&self, mission: Mission) -> Result<MissionResult> {
        info!(
            mission_id = %mission.id,
            description = %mission.description,
            files = mission.affected_files.len(),
            "Starting mission execution"
        );

        // Phase 1: Determine scopes
        let scopes = self.determine_scopes(&mission);
        debug!(scopes = scopes.len(), "Determined agent scopes");

        // Phase 2: Plan via consensus (if enabled)
        let plan = self.plan(&mission, &scopes).await?;

        // Phase 3: Execute tasks
        let results = self.execute_tasks(&plan).await?;

        // Phase 4: Verify and return
        let status = self.determine_status(&results);

        Ok(MissionResult {
            mission_id: mission.id,
            status,
            results,
            scopes_executed: scopes,
        })
    }

    fn determine_scopes(&self, mission: &Mission) -> Vec<AgentScope> {
        let mut scopes_by_workspace: HashMap<String, Vec<PathBuf>> = HashMap::new();

        for file in &mission.affected_files {
            if let Some(ws) = self.workspaces.workspace_for_file(file) {
                scopes_by_workspace
                    .entry(ws.name.clone())
                    .or_default()
                    .push(file.clone());
            }
        }

        if scopes_by_workspace.is_empty() {
            if let Some(first) = self.workspaces.first() {
                return vec![AgentScope::Workspace {
                    workspace: first.name.clone(),
                }];
            }
            return Vec::new();
        }

        scopes_by_workspace
            .into_iter()
            .map(|(ws_name, files)| {
                let ws = self.workspaces.get(&ws_name).expect("workspace must exist");
                AgentScope::from_files(ws, &files)
            })
            .collect()
    }

    async fn plan(&self, mission: &Mission, scopes: &[AgentScope]) -> Result<ExecutionPlan> {
        if let Some(ref consensus) = self.consensus {
            let task = ConsensusTask {
                id: mission.id.clone(),
                description: mission.description.clone(),
                assigned_module: None,
                dependencies: Vec::new(),
                priority: mission.priority,
                estimated_complexity: TaskComplexity::default(),
                files_affected: mission
                    .affected_files
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect(),
                priority_score: 0.0,
            };

            let participants = self.select_participant_ids(scopes);
            let context = TaskContext {
                mission_id: mission.id.clone(),
                related_files: task.files_affected.clone(),
                ..Default::default()
            };

            let result = consensus
                .run_with_dynamic_joining(
                    &mission.description,
                    &context,
                    &participants,
                    &self.pool,
                    |_requests| Vec::new(), // No dynamic agent registration
                )
                .await?;

            Ok(ExecutionPlan::from_consensus(result, scopes.to_vec()))
        } else {
            Ok(ExecutionPlan::direct(mission, scopes.to_vec()))
        }
    }

    fn select_participant_ids(&self, scopes: &[AgentScope]) -> Vec<AgentId> {
        let mut ids = Vec::new();

        for scope in scopes {
            match scope {
                AgentScope::Module { module, .. } => {
                    let role = AgentRole::module(module);
                    if let Some(agent) = self.pool.select(&role) {
                        ids.push(AgentId::new(agent.id()));
                    }
                }
                _ => {
                    if let Some(agent) = self.pool.select(&AgentRole::core_planning()) {
                        ids.push(AgentId::new(agent.id()));
                    }
                }
            }
        }

        if ids.is_empty()
            && let Some(agent) = self.pool.select(&AgentRole::core_coder())
        {
            ids.push(AgentId::new(agent.id()));
        }

        ids
    }

    async fn execute_tasks(&self, plan: &ExecutionPlan) -> Result<Vec<AgentTaskResult>> {
        let mut results = Vec::new();

        for task in &plan.tasks {
            let role = task.role.clone().unwrap_or_else(AgentRole::core_coder);
            let agent = self.pool.select(&role).ok_or_else(|| {
                PilotError::AgentExecution(format!("No agent available for role {:?}", role))
            })?;

            let ws = plan
                .scopes
                .first()
                .and_then(|s| self.workspaces.get(s.workspace_name()))
                .ok_or_else(|| PilotError::Config("No workspace for task".into()))?;

            let result = agent.execute(task, &ws.root).await?;
            results.push(result);
        }

        Ok(results)
    }

    fn determine_status(&self, results: &[AgentTaskResult]) -> MissionStatus {
        if results.is_empty() {
            return MissionStatus::Failed {
                reason: "No tasks executed".into(),
            };
        }

        let successful = results.iter().filter(|r| r.success).count();
        let total = results.len();

        if successful == total {
            MissionStatus::Success
        } else if successful > 0 {
            MissionStatus::PartialSuccess {
                completed: successful,
                total,
            }
        } else {
            MissionStatus::Failed {
                reason: "All tasks failed".into(),
            }
        }
    }

    #[inline]
    pub fn workspaces(&self) -> &WorkspaceSet {
        &self.workspaces
    }
}

/// Internal execution plan.
struct ExecutionPlan {
    tasks: Vec<AgentTask>,
    scopes: Vec<AgentScope>,
}

impl ExecutionPlan {
    fn from_consensus(result: ConsensusResult, scopes: Vec<AgentScope>) -> Self {
        let tasks = match result {
            ConsensusResult::Agreed {
                tasks: consensus_tasks,
                ..
            } => consensus_tasks
                .into_iter()
                .map(|ct| AgentTask {
                    id: ct.id,
                    description: ct.description,
                    context: TaskContext::default(),
                    priority: ct.priority,
                    role: None,
                })
                .collect(),
            ConsensusResult::PartialAgreement { plan, .. } => {
                vec![AgentTask {
                    id: uuid::Uuid::new_v4().to_string(),
                    description: plan,
                    context: TaskContext::default(),
                    priority: TaskPriority::Normal,
                    role: None,
                }]
            }
            ConsensusResult::NoConsensus { summary, .. } => {
                vec![AgentTask {
                    id: uuid::Uuid::new_v4().to_string(),
                    description: summary,
                    context: TaskContext::default(),
                    priority: TaskPriority::Normal,
                    role: None,
                }]
            }
        };

        Self { tasks, scopes }
    }

    fn direct(mission: &Mission, scopes: Vec<AgentScope>) -> Self {
        let task = AgentTask {
            id: uuid::Uuid::new_v4().to_string(),
            description: mission.description.clone(),
            context: TaskContext {
                related_files: mission
                    .affected_files
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect(),
                ..Default::default()
            },
            priority: mission.priority,
            role: None,
        };

        Self {
            tasks: vec![task],
            scopes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mission_creation() {
        let mission = Mission::new("Add user authentication")
            .with_files(vec![PathBuf::from("src/auth/mod.rs")])
            .with_priority(TaskPriority::High);

        assert_eq!(mission.description, "Add user authentication");
        assert_eq!(mission.affected_files.len(), 1);
        assert_eq!(mission.priority, TaskPriority::High);
    }

    #[test]
    fn test_mission_status() {
        assert_eq!(MissionStatus::Success, MissionStatus::Success);

        let partial = MissionStatus::PartialSuccess {
            completed: 2,
            total: 3,
        };
        assert!(matches!(partial, MissionStatus::PartialSuccess { .. }));
    }
}
