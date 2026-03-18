use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::super::consensus::{ConsensusTask, TaskComplexity};
use super::super::traits::{
    AgentRole, AgentTask, TaskContext, TaskPriority, calculate_priority_score,
};
use super::Coordinator;
use crate::workspace::Workspace;

impl Coordinator {
    pub(super) fn build_module_map(
        workspace: &Workspace,
        workspace_id: Option<&str>,
    ) -> (HashMap<String, Vec<PathBuf>>, HashMap<String, Vec<String>>) {
        let mut module_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();

        for module in workspace.modules() {
            let paths: Vec<PathBuf> = module.paths.iter().map(PathBuf::from).collect();

            let module_key = match workspace_id {
                Some(ws_id) => format!("{}::{}", ws_id, module.id),
                None => module.id.clone(),
            };

            module_files.insert(module_key.clone(), paths);

            let deps: Vec<String> = module
                .dependencies
                .iter()
                .map(|d| match workspace_id {
                    Some(ws_id) => format!("{}::{}", ws_id, &d.module_id),
                    None => d.module_id.clone(),
                })
                .collect();
            dependencies.insert(module_key, deps);
        }

        (module_files, dependencies)
    }

    pub(super) fn to_agent_task(task: &ConsensusTask, context: &TaskContext) -> AgentTask {
        let role = task
            .assigned_module
            .as_ref()
            .filter(|m| !m.is_empty())
            .map(|module_id| {
                if module_id.contains("::") {
                    AgentRole::module_coder_from_qualified(module_id)
                } else {
                    AgentRole::module(module_id)
                }
            })
            .unwrap_or_else(AgentRole::core_coder);

        AgentTask {
            id: task.id.clone(),
            description: task.description.clone(),
            context: context.clone(),
            priority: task.priority,
            role: Some(role.clone()),
        }
    }

    pub(super) fn normalize_issue_key(issue: &str) -> String {
        const MAX_ISSUE_LENGTH: usize = 500;
        let truncated = if issue.len() > MAX_ISSUE_LENGTH {
            &issue[..MAX_ISSUE_LENGTH]
        } else {
            issue
        };

        truncated
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .take(10)
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    pub(super) fn has_blockers(&self, context: &TaskContext) -> bool {
        !context.blockers.is_empty()
    }

    pub(super) fn extract_implementation_tasks(&self, context: &TaskContext) -> Vec<ConsensusTask> {
        let default_score = calculate_priority_score(None, None);
        let mut tasks: Vec<_> = context
            .key_findings
            .iter()
            .enumerate()
            .map(|(i, finding)| ConsensusTask {
                id: format!("impl-{}", i),
                description: finding.clone(),
                priority: TaskPriority::Normal,
                dependencies: Vec::new(),
                assigned_module: None,
                priority_score: default_score,
                files_affected: Vec::new(),
                estimated_complexity: TaskComplexity::Medium,
            })
            .collect();

        if tasks.is_empty() && !context.related_files.is_empty() {
            tasks.push(ConsensusTask {
                id: "impl-main".to_string(),
                description: format!(
                    "Implement changes in files: {}",
                    context.related_files.join(", ")
                ),
                priority: TaskPriority::Normal,
                dependencies: Vec::new(),
                assigned_module: None,
                priority_score: default_score,
                files_affected: context.related_files.clone(),
                estimated_complexity: TaskComplexity::Medium,
            });
        }

        if let Some(ref ws) = self.workspace {
            for task in &mut tasks {
                if task.assigned_module.is_none() {
                    task.assigned_module = self.infer_module_from_files(
                        &task.files_affected,
                        &context.related_files,
                        ws.modules(),
                    );
                }
            }
        }

        tasks.sort_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        tasks
    }

    pub(super) fn infer_module_from_files(
        &self,
        task_files: &[String],
        context_files: &[String],
        modules: &[modmap::Module],
    ) -> Option<String> {
        let files_to_check: Vec<&str> = if task_files.is_empty() {
            context_files.iter().map(|s| s.as_str()).collect()
        } else {
            task_files.iter().map(|s| s.as_str()).collect()
        };

        if files_to_check.is_empty() {
            return None;
        }

        let mut best_module = None;
        let mut best_count = 0usize;

        for module in modules {
            let count = files_to_check
                .iter()
                .filter(|f| {
                    module
                        .paths
                        .iter()
                        .any(|p: &String| f.starts_with(p.as_str()))
                })
                .count();
            if count > best_count {
                best_count = count;
                best_module = Some(module.name.clone());
            }
        }

        best_module
    }

    pub(super) fn prepare_tasks(&self, tasks: Vec<ConsensusTask>) -> Vec<ConsensusTask> {
        let mut scored_tasks: Vec<_> = tasks
            .into_iter()
            .map(|mut t| {
                let complexity_factor = t.estimated_complexity.weight();
                let priority_factor = match t.priority {
                    TaskPriority::Low => 0.25,
                    TaskPriority::Normal => 0.5,
                    TaskPriority::High => 0.75,
                    TaskPriority::Critical => 1.0,
                };
                t.priority_score = priority_factor * 0.7 + (1.0 - complexity_factor) * 0.3;
                t
            })
            .collect();

        scored_tasks.sort_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        scored_tasks
    }

    pub(super) fn partition_tasks<'a>(
        &self,
        tasks: &'a [ConsensusTask],
    ) -> (Vec<&'a ConsensusTask>, Vec<&'a ConsensusTask>) {
        use std::collections::HashSet;

        let mut independent = Vec::new();
        let mut dependent = Vec::new();
        let mut claimed_files: HashSet<PathBuf> = HashSet::new();

        for task in tasks {
            if !task.dependencies.is_empty() {
                dependent.push(task);
                continue;
            }

            let has_conflict = task.files_affected.iter().any(|f| {
                let normalized = std::fs::canonicalize(f).unwrap_or_else(|_| {
                    Path::new(f).to_path_buf()
                });
                claimed_files.contains(&normalized)
            });

            if has_conflict {
                debug!(
                    task_id = %task.id,
                    "Task has file conflicts with independent tasks, marking as dependent"
                );
                dependent.push(task);
            } else {
                for file in &task.files_affected {
                    let normalized = std::fs::canonicalize(file).unwrap_or_else(|_| {
                        Path::new(file).to_path_buf()
                    });
                    claimed_files.insert(normalized);
                }
                independent.push(task);
            }
        }

        (independent, dependent)
    }

    pub(super) fn topological_sort_tasks<'a>(
        &self,
        tasks: &'a [ConsensusTask],
    ) -> Vec<&'a ConsensusTask> {
        use std::collections::{HashMap, HashSet, VecDeque};

        if tasks.is_empty() {
            return Vec::new();
        }

        let task_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        let task_map: HashMap<&str, &ConsensusTask> =
            tasks.iter().map(|t| (t.id.as_str(), t)).collect();

        for task in tasks {
            in_degree.entry(task.id.as_str()).or_insert(0);

            for dep in &task.dependencies {
                if task_ids.contains(dep.as_str()) {
                    *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(dep.as_str())
                        .or_default()
                        .push(task.id.as_str());
                }
            }
        }

        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg == 0)
            .map(|(&id, _)| id)
            .collect();

        let mut sorted = Vec::with_capacity(tasks.len());

        while let Some(task_id) = queue.pop_front() {
            if let Some(&task) = task_map.get(task_id) {
                sorted.push(task);

                if let Some(deps) = dependents.get(task_id) {
                    for &dep_id in deps {
                        if let Some(deg) = in_degree.get_mut(dep_id) {
                            *deg = deg.saturating_sub(1);
                            if *deg == 0 {
                                queue.push_back(dep_id);
                            }
                        }
                    }
                }
            }
        }

        if sorted.len() < tasks.len() {
            warn!(
                sorted = sorted.len(),
                total = tasks.len(),
                "Circular dependency detected, appending remaining tasks"
            );
            for task in tasks {
                if !sorted.iter().any(|t| t.id == task.id) {
                    sorted.push(task);
                }
            }
        }

        sorted
    }
}
