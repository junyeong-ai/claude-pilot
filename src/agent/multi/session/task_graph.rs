//! Task dependency graph (DAG) for orchestration sessions.
//!
//! Manages task dependencies, execution ordering, and status tracking
//! for all tasks within a session.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::agent::multi::AgentId;

/// Status of a task in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Task is waiting for dependencies.
    Pending,
    /// Task dependencies are satisfied, ready to execute.
    Ready,
    /// Task is currently being executed.
    InProgress,
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
    /// Task was cancelled.
    Cancelled,
    /// Task is blocked by failed dependency.
    Blocked,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Ready)
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending | Self::Ready)
    }
}

/// Status of a dependency relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyStatus {
    /// Dependency is not yet satisfied.
    Waiting,
    /// Dependency is satisfied (task completed).
    Satisfied,
    /// Dependency failed (blocking dependent tasks).
    Failed,
}

/// Type of dependency between tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyType {
    /// Must complete before dependent can start.
    Blocking,
    /// Should complete first but not required.
    SoftDependency,
    /// Tasks share resources (for conflict detection).
    ResourceConflict,
}

/// A dependency relationship between tasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDependency {
    /// The task that must complete first.
    pub prerequisite: String,
    /// The task that depends on the prerequisite.
    pub dependent: String,
    /// Type of dependency.
    pub dependency_type: DependencyType,
}

/// Information about a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    /// Unique task identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Target module for the task.
    pub module: String,
    /// Required role to execute.
    pub required_role: String,
    /// Estimated complexity (tokens).
    pub estimated_complexity: u32,
    /// Priority (higher = more important).
    pub priority: u32,
    /// Files affected by this task.
    pub affected_files: Vec<String>,
}

/// A node in the task DAG.
#[derive(Debug, Clone)]
pub struct TaskNode {
    /// Task information.
    pub info: TaskInfo,
    /// Current status.
    pub status: TaskStatus,
    /// Assigned agent (if any).
    pub assigned_to: Option<AgentId>,
    /// Tasks this depends on.
    pub dependencies: HashSet<String>,
    /// Tasks that depend on this.
    pub dependents: HashSet<String>,
    /// Execution result (if completed).
    pub result: Option<TaskResult>,
    /// Retry count.
    pub retry_count: u32,
}

impl TaskNode {
    pub fn new(info: TaskInfo) -> Self {
        Self {
            info,
            status: TaskStatus::Pending,
            assigned_to: None,
            dependencies: HashSet::new(),
            dependents: HashSet::new(),
            result: None,
            retry_count: 0,
        }
    }

    pub fn has_unmet_dependencies(&self, completed: &HashSet<String>) -> bool {
        !self.dependencies.is_subset(completed)
    }

    pub fn unmet_dependencies<'a>(&'a self, completed: &'a HashSet<String>) -> Vec<&'a String> {
        self.dependencies.difference(completed).collect()
    }
}

/// Result of task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub success: bool,
    pub output: String,
    pub modified_files: Vec<String>,
    pub artifacts: Vec<String>,
}

/// Directed Acyclic Graph for task management.
#[derive(Debug, Default)]
pub struct TaskDAG {
    nodes: HashMap<String, TaskNode>,
    completed: HashSet<String>,
    failed: HashSet<String>,
}

impl TaskDAG {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a task to the graph.
    pub fn add_task(&mut self, info: TaskInfo) {
        let id = info.id.clone();
        self.nodes.insert(id, TaskNode::new(info));
    }

    /// Add a dependency between tasks.
    pub fn add_dependency(&mut self, dependency: TaskDependency) -> Result<(), String> {
        // Validate both tasks exist
        if !self.nodes.contains_key(&dependency.prerequisite) {
            return Err(format!(
                "Prerequisite task not found: {}",
                dependency.prerequisite
            ));
        }
        if !self.nodes.contains_key(&dependency.dependent) {
            return Err(format!(
                "Dependent task not found: {}",
                dependency.dependent
            ));
        }

        // Check for cycles before adding
        if self.would_create_cycle(&dependency.prerequisite, &dependency.dependent) {
            return Err(format!(
                "Adding dependency {} -> {} would create a cycle",
                dependency.prerequisite, dependency.dependent
            ));
        }

        // Add the dependency
        if let Some(prereq) = self.nodes.get_mut(&dependency.prerequisite) {
            prereq.dependents.insert(dependency.dependent.clone());
        }
        if let Some(dep) = self.nodes.get_mut(&dependency.dependent) {
            dep.dependencies.insert(dependency.prerequisite);
        }

        Ok(())
    }

    fn would_create_cycle(&self, from: &str, to: &str) -> bool {
        // BFS from 'to' to see if we can reach 'from'
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(to.to_string());

        while let Some(current) = queue.pop_front() {
            if current == from {
                return true;
            }
            if visited.insert(current.clone())
                && let Some(node) = self.nodes.get(&current)
            {
                for dep in &node.dependents {
                    queue.push_back(dep.clone());
                }
            }
        }
        false
    }

    /// Get a task by ID.
    pub fn get(&self, id: &str) -> Option<&TaskNode> {
        self.nodes.get(id)
    }

    /// Get a mutable reference to a task.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut TaskNode> {
        self.nodes.get_mut(id)
    }

    /// Get all tasks ready to execute.
    pub fn ready_tasks(&self) -> Vec<&TaskNode> {
        self.nodes
            .values()
            .filter(|node| {
                node.status == TaskStatus::Ready
                    || (node.status == TaskStatus::Pending
                        && !node.has_unmet_dependencies(&self.completed))
            })
            .collect()
    }

    /// Get tasks ready for a specific module.
    pub fn ready_tasks_for_module(&self, module: &str) -> Vec<&TaskNode> {
        self.ready_tasks()
            .into_iter()
            .filter(|node| node.info.module == module)
            .collect()
    }

    /// Get tasks ready for a specific role.
    pub fn ready_tasks_for_role(&self, role: &str) -> Vec<&TaskNode> {
        self.ready_tasks()
            .into_iter()
            .filter(|node| node.info.required_role == role)
            .collect()
    }

    /// Update task status to Ready if dependencies are satisfied.
    pub fn refresh_ready_status(&mut self) {
        let ready_ids: Vec<String> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.status == TaskStatus::Pending && !node.has_unmet_dependencies(&self.completed)
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in ready_ids {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.status = TaskStatus::Ready;
            }
        }
    }

    /// Mark a task as in progress.
    pub fn start_task(&mut self, id: &str, agent: AgentId) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("Task not found: {}", id))?;

        if !node.status.is_runnable() && node.status != TaskStatus::Pending {
            return Err(format!(
                "Task {} is not runnable (status: {:?})",
                id, node.status
            ));
        }

        if node.has_unmet_dependencies(&self.completed) {
            return Err(format!(
                "Task {} has unmet dependencies: {:?}",
                id,
                node.unmet_dependencies(&self.completed)
            ));
        }

        node.status = TaskStatus::InProgress;
        node.assigned_to = Some(agent);
        Ok(())
    }

    /// Mark a task as completed.
    pub fn complete_task(&mut self, id: &str, result: TaskResult) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("Task not found: {}", id))?;

        node.status = TaskStatus::Completed;
        node.result = Some(result);
        self.completed.insert(id.to_string());

        // Refresh ready status for dependent tasks
        self.refresh_ready_status();
        Ok(())
    }

    /// Mark a task as failed.
    pub fn fail_task(&mut self, id: &str, error: String) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("Task not found: {}", id))?;

        node.status = TaskStatus::Failed;
        node.result = Some(TaskResult {
            success: false,
            output: error,
            modified_files: Vec::new(),
            artifacts: Vec::new(),
        });
        self.failed.insert(id.to_string());

        let dependents: Vec<String> = node.dependents.iter().cloned().collect();
        for dep_id in dependents {
            if let Some(dep_node) = self.nodes.get_mut(&dep_id)
                && dep_node.status.is_pending()
            {
                dep_node.status = TaskStatus::Blocked;
            }
        }

        Ok(())
    }

    /// Retry a failed task.
    pub fn retry_task(&mut self, id: &str) -> Result<(), String> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| format!("Task not found: {}", id))?;

        if node.status != TaskStatus::Failed {
            return Err(format!("Task {} is not failed", id));
        }

        node.status = TaskStatus::Pending;
        node.retry_count += 1;
        node.assigned_to = None;
        self.failed.remove(id);

        self.refresh_ready_status();
        Ok(())
    }

    /// Get topological order of tasks.
    pub fn topological_order(&self) -> Vec<&str> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut temp_visited = HashSet::new();

        fn visit<'a>(
            id: &'a str,
            nodes: &'a HashMap<String, TaskNode>,
            visited: &mut HashSet<&'a str>,
            temp_visited: &mut HashSet<&'a str>,
            result: &mut Vec<&'a str>,
        ) {
            if visited.contains(id) {
                return;
            }
            if temp_visited.contains(id) {
                return; // Cycle detected, skip
            }

            temp_visited.insert(id);

            if let Some(node) = nodes.get(id) {
                for dep in &node.dependencies {
                    visit(dep, nodes, visited, temp_visited, result);
                }
            }

            temp_visited.remove(id);
            visited.insert(id);
            result.push(id);
        }

        for id in self.nodes.keys() {
            visit(
                id,
                &self.nodes,
                &mut visited,
                &mut temp_visited,
                &mut result,
            );
        }

        result
    }

    /// Get parallel execution groups (tasks that can run together).
    pub fn parallel_groups(&self) -> Vec<Vec<&str>> {
        let mut groups = Vec::new();
        let mut remaining: HashSet<&str> = self.nodes.keys().map(|s| s.as_str()).collect();
        let mut completed: HashSet<String> = self.completed.clone();

        while !remaining.is_empty() {
            // Find all tasks with satisfied dependencies
            let group: Vec<&str> = remaining
                .iter()
                .copied()
                .filter(|id| {
                    if let Some(node) = self.nodes.get(*id) {
                        !node.has_unmet_dependencies(&completed)
                    } else {
                        false
                    }
                })
                .collect();

            if group.is_empty() {
                // Remaining tasks have cyclic dependencies or failed prereqs
                break;
            }

            for id in &group {
                remaining.remove(id);
                completed.insert((*id).to_string());
            }

            groups.push(group);
        }

        groups
    }

    /// Check if all tasks are complete.
    pub fn is_complete(&self) -> bool {
        self.nodes.values().all(|n| n.status.is_terminal())
    }

    /// Check if any task has failed.
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    /// Get completion statistics.
    pub fn stats(&self) -> TaskStats {
        let mut stats = TaskStats::default();
        for node in self.nodes.values() {
            match node.status {
                TaskStatus::Pending => stats.pending += 1,
                TaskStatus::Ready => stats.ready += 1,
                TaskStatus::InProgress => stats.in_progress += 1,
                TaskStatus::Completed => stats.completed += 1,
                TaskStatus::Failed => stats.failed += 1,
                TaskStatus::Cancelled => stats.cancelled += 1,
                TaskStatus::Blocked => stats.blocked += 1,
            }
        }
        stats.total = self.nodes.len();
        stats
    }

    /// Get all task IDs.
    pub fn task_ids(&self) -> Vec<&str> {
        self.nodes.keys().map(|s| s.as_str()).collect()
    }

    /// Get count of tasks.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get tasks that conflict with a given task (shared files).
    pub fn conflicting_tasks(&self, id: &str) -> Vec<&str> {
        let Some(task) = self.nodes.get(id) else {
            return Vec::new();
        };

        let files: HashSet<&str> = task
            .info
            .affected_files
            .iter()
            .map(|s| s.as_str())
            .collect();

        self.nodes
            .iter()
            .filter(|(other_id, other_node)| {
                *other_id != id
                    && other_node
                        .info
                        .affected_files
                        .iter()
                        .any(|f| files.contains(f.as_str()))
            })
            .map(|(id, _)| id.as_str())
            .collect()
    }
}

/// Statistics about task execution.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TaskStats {
    pub total: usize,
    pub pending: usize,
    pub ready: usize,
    pub in_progress: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub blocked: usize,
}

impl TaskStats {
    pub fn completion_ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.completed as f64 / self.total as f64
        }
    }

    pub fn success_ratio(&self) -> f64 {
        let terminal = self.completed + self.failed + self.cancelled;
        if terminal == 0 {
            0.0
        } else {
            self.completed as f64 / terminal as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_task(id: &str, module: &str) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            description: format!("Task {}", id),
            module: module.to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: 1000,
            priority: 1,
            affected_files: Vec::new(),
        }
    }

    #[test]
    fn test_task_lifecycle() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth"));

        // Initial state
        assert_eq!(dag.get("task-1").unwrap().status, TaskStatus::Pending);

        // Start task
        dag.refresh_ready_status();
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        assert_eq!(dag.get("task-1").unwrap().status, TaskStatus::InProgress);

        // Complete task
        dag.complete_task(
            "task-1",
            TaskResult {
                success: true,
                output: "Done".to_string(),
                modified_files: vec!["auth.rs".to_string()],
                artifacts: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(dag.get("task-1").unwrap().status, TaskStatus::Completed);
    }

    #[test]
    fn test_dependencies() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth"));
        dag.add_task(create_task("task-2", "api"));

        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-2".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        dag.refresh_ready_status();

        // task-1 should be ready, task-2 should be pending
        assert_eq!(dag.get("task-1").unwrap().status, TaskStatus::Ready);
        assert_eq!(dag.get("task-2").unwrap().status, TaskStatus::Pending);

        // Complete task-1
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        dag.complete_task(
            "task-1",
            TaskResult {
                success: true,
                output: "Done".to_string(),
                modified_files: Vec::new(),
                artifacts: Vec::new(),
            },
        )
        .unwrap();

        // task-2 should now be ready
        assert_eq!(dag.get("task-2").unwrap().status, TaskStatus::Ready);
    }

    #[test]
    fn test_cycle_detection() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth"));
        dag.add_task(create_task("task-2", "api"));

        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-2".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        // Adding reverse dependency should fail
        let result = dag.add_dependency(TaskDependency {
            prerequisite: "task-2".to_string(),
            dependent: "task-1".to_string(),
            dependency_type: DependencyType::Blocking,
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_parallel_groups() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth"));
        dag.add_task(create_task("task-2", "api"));
        dag.add_task(create_task("task-3", "db"));
        dag.add_task(create_task("task-4", "auth"));

        // task-4 depends on task-1 and task-2
        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-4".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();
        dag.add_dependency(TaskDependency {
            prerequisite: "task-2".to_string(),
            dependent: "task-4".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        let groups = dag.parallel_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 3); // task-1, task-2, task-3 in parallel
        assert_eq!(groups[1].len(), 1); // task-4 after
    }

    #[test]
    fn test_failure_blocks_dependents() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth"));
        dag.add_task(create_task("task-2", "api"));

        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-2".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        dag.refresh_ready_status();
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        dag.fail_task("task-1", "Build error".to_string()).unwrap();

        // task-2 should be blocked
        assert_eq!(dag.get("task-2").unwrap().status, TaskStatus::Blocked);
    }
}
