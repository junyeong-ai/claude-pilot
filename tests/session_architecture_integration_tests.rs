#![allow(clippy::field_reassign_with_default)]
//! Comprehensive integration tests for the new session-based architecture.
//!
//! Tests the complete flow:
//! - OrchestrationSession lifecycle
//! - AgentContext with participant awareness
//! - Communication tools end-to-end
//! - SharedState visibility
//! - DAG-based task scheduling
//! - Cross-workspace coordination
//! - Lock acquisition and release

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use claude_pilot::agent::multi::session::{
    AgentCapabilities, AgentContext, AgentContextBuilder, CheckpointReason, CommunicationTools,
    DependencyType, LockResult, LockType, NotificationLog, NotificationType, OrchestrationSession,
    ParticipantRegistry, Phase, SessionConfig, SessionStatus, SharedState, TaskDAG, TaskDependency,
    TaskInfo, TaskResult, TaskStatus,
};
use claude_pilot::agent::multi::{AgentId, AgentRole};

// ============================================================================
// OrchestrationSession Lifecycle Tests
// ============================================================================

mod session_lifecycle {
    use super::*;

    #[test]
    fn test_session_creation_and_initialization() {
        let session =
            OrchestrationSession::new("Implement auth feature", PathBuf::from("/tmp/test"));

        assert_eq!(session.status(), SessionStatus::Initializing);
        assert_eq!(session.phase(), &Phase::Setup);
        assert!(!session.id.is_empty());
        assert_eq!(session.mission, "Implement auth feature");
    }

    #[test]
    fn test_session_with_custom_config() {
        let config = SessionConfig {
            max_duration: Duration::from_secs(3600),
            checkpoint_interval: Duration::from_secs(60),
            context_budget_threshold: 0.9,
            max_parallel_tasks: 5,
            max_task_retries: 2,
            enable_cross_workspace: false,
            max_notifications: 500,
            notification_max_age: Duration::from_secs(1800),
        };

        let session = OrchestrationSession::new("Test mission", PathBuf::from("/tmp"))
            .with_config(config.clone())
            .with_context_budget(100000, 5000);

        assert_eq!(session.config().max_parallel_tasks, 5);
        assert_eq!(session.context_budget().total, 100000);
        assert_eq!(session.context_budget().reserved, 5000);
    }

    #[test]
    fn test_session_phase_transitions() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));

        // Start session
        session.start();
        assert_eq!(session.status(), SessionStatus::Active);

        // Advance through phases
        session.advance_phase(); // Setup -> Planning
        assert_eq!(session.phase(), &Phase::Planning);

        session.advance_phase(); // Planning -> Implementation
        assert_eq!(session.phase(), &Phase::Implementation);

        session.advance_phase(); // Implementation -> Verification
        assert_eq!(session.phase(), &Phase::Verification);

        session.advance_phase(); // Verification -> Finalization
        assert_eq!(session.phase(), &Phase::Finalization);

        // No more phases
        assert!(session.advance_phase().is_none());
    }

    #[test]
    fn test_session_pause_resume() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        assert_eq!(session.status(), SessionStatus::Active);

        session.pause();
        assert_eq!(session.status(), SessionStatus::Paused);

        session.resume();
        assert_eq!(session.status(), SessionStatus::Active);
    }

    #[test]
    fn test_session_completion() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();
        session.complete();

        assert_eq!(session.status(), SessionStatus::Completed);
        assert_eq!(session.phase(), &Phase::Finalization);
        assert!(session.status().is_terminal());
    }

    #[test]
    fn test_session_failure() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();
        session.fail("Build error: compilation failed");

        assert_eq!(session.status(), SessionStatus::Failed);
        assert!(session.status().is_terminal());
        assert_eq!(
            session.metadata().get("failure_reason"),
            Some(&"Build error: compilation failed".to_string())
        );
    }

    #[test]
    fn test_session_summary() {
        let mut session = OrchestrationSession::new("Test mission", PathBuf::from("/tmp"))
            .with_context_budget(200000, 10000);

        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("coder")];
        session.register_agent("agent-1", "auth", "workspace-1", caps);

        session.add_task(TaskInfo {
            id: "task-1".to_string(),
            description: "Test task".to_string(),
            module: "auth".to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: 1000,
            priority: 1,
            affected_files: vec![],
        });

        session.start();

        let summary = session.summary();
        assert_eq!(summary.participant_count, 1);
        assert_eq!(summary.task_stats.total, 1);
        assert_eq!(summary.status, SessionStatus::Active);
    }
}

// ============================================================================
// AgentContext Tests
// ============================================================================

mod agent_context {
    use super::*;

    #[test]
    fn test_agent_context_creation() {
        let ctx = AgentContext::new("agent-1", "session-123")
            .with_phase("implementation")
            .with_working_dir(PathBuf::from("/tmp/project"))
            .with_working_files(vec!["auth.rs".to_string(), "config.rs".to_string()]);

        assert_eq!(ctx.agent_id, "agent-1");
        assert_eq!(ctx.session_id, "session-123");
        assert_eq!(ctx.phase, "implementation");
        assert_eq!(ctx.working_files.len(), 2);
    }

    #[test]
    fn test_agent_context_locked_files_detection() {
        let ctx = AgentContext::new("agent-1", "session-1")
            .with_working_files(vec!["shared.rs".to_string(), "auth.rs".to_string()])
            .with_locked_files(HashMap::from([
                ("shared.rs".to_string(), "agent-2".to_string()),
                ("other.rs".to_string(), "agent-3".to_string()),
            ]));

        let overlapping = ctx.agents_on_same_files();
        assert_eq!(overlapping.len(), 1);
        assert!(overlapping.contains(&"agent-2"));
    }

    #[test]
    fn test_agent_context_conflict_detection() {
        use claude_pilot::agent::multi::session::ConflictSummary;

        let ctx = AgentContext::new("agent-1", "session-1").with_conflicts(vec![ConflictSummary {
            id: "conflict-1".to_string(),
            agents: vec!["agent-1".to_string(), "agent-2".to_string()],
            files: vec!["shared.rs".to_string()],
            description: "Concurrent modification".to_string(),
        }]);

        assert!(ctx.has_conflicts());
        assert_eq!(ctx.active_conflicts.len(), 1);
    }

    #[test]
    fn test_agent_context_format_for_prompt() {
        use claude_pilot::agent::multi::session::{
            AgentStatus, ParticipantSummary, TaskAssignment,
        };

        let ctx = AgentContext::new("agent-1", "session-1")
            .with_phase("implementation")
            .with_participants(vec![ParticipantSummary {
                id: "agent-2".to_string(),
                module: "api".to_string(),
                roles: vec!["coder".to_string()],
                domains: vec![],
                status: AgentStatus::Busy,
            }])
            .with_task(TaskAssignment {
                id: "task-1".to_string(),
                description: "Implement auth".to_string(),
                module: "auth".to_string(),
                role: "coder".to_string(),
                files: vec!["auth.rs".to_string()],
                dependencies: vec![],
                priority: 1,
                instructions: None,
            });

        let prompt = ctx.format_for_prompt();

        assert!(prompt.contains("session-1"));
        assert!(prompt.contains("implementation"));
        assert!(prompt.contains("task-1"));
        assert!(prompt.contains("agent-2"));
    }

    #[test]
    fn test_agent_context_builder_from_session() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("coder")];
        session.register_agent("agent-1", "auth", "workspace-1", caps);

        session.notify(
            "system",
            "*",
            NotificationType::Text {
                message: "Session started".to_string(),
            },
        );

        let agent_id = AgentId::new("agent-1");
        let ctx = AgentContextBuilder::new(&session, agent_id)
            .with_max_notifications(10)
            .build();

        assert_eq!(ctx.agent_id, "agent-1");
        // The builder includes participants from participant_summaries()
        // Note: participant summaries includes the registering agent itself
        assert!(!ctx.notifications.is_empty());
    }
}

// ============================================================================
// Communication Tools Tests
// ============================================================================

mod communication_tools {
    use super::*;
    use claude_pilot::agent::multi::session::{
        AcquireLockInput, CoordinateInput, CoordinationAction, LockTypeInput,
        NotificationTypeInput, NotifyInput, QueryFilters, QueryInput, QueryResults, QueryType,
        SaveProgressInput,
    };

    fn create_test_tools() -> CommunicationTools {
        CommunicationTools::new(
            Arc::new(RwLock::new(NotificationLog::new(
                1000,
                Duration::from_secs(3600),
            ))),
            Arc::new(SharedState::new()),
            Arc::new(RwLock::new(ParticipantRegistry::new())),
        )
    }

    #[test]
    fn test_notify_and_query_notifications() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Send notification
        let result = tools.notify(
            &agent,
            NotifyInput {
                target: "*".to_string(),
                notification_type: NotificationTypeInput::TaskCompleted {
                    task_id: "task-1".to_string(),
                    files_modified: vec!["auth.rs".to_string()],
                },
                priority: None,
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Query notifications
        let query_result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Notifications,
                filters: QueryFilters {
                    task_related_only: true,
                    ..Default::default()
                },
                limit: 10,
            },
        );
        assert!(query_result.is_ok());
        let output = query_result.unwrap();
        assert!(output.total_count >= 1);
    }

    #[test]
    fn test_lock_workflow() {
        let tools = create_test_tools();
        let agent1 = AgentId::new("agent-1");
        let agent2 = AgentId::new("agent-2");

        // Agent 1 acquires lock
        let result = tools.acquire_lock(
            &agent1,
            AcquireLockInput {
                resource: "auth.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: Some(60),
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().acquired);

        // Agent 2 tries to acquire - should fail
        let result = tools.acquire_lock(
            &agent2,
            AcquireLockInput {
                resource: "auth.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: None,
            },
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.acquired);
        assert_eq!(output.holder, Some("agent-1".to_string()));

        // Release lock
        assert!(tools.release_lock(&agent1, "auth.rs"));

        // Agent 2 can now acquire
        let result = tools.acquire_lock(
            &agent2,
            AcquireLockInput {
                resource: "auth.rs".to_string(),
                lock_type: LockTypeInput::Exclusive,
                duration_secs: None,
            },
        );
        assert!(result.unwrap().acquired);
    }

    #[test]
    fn test_query_locks() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Acquire multiple locks
        tools
            .acquire_lock(
                &agent,
                AcquireLockInput {
                    resource: "file1.rs".to_string(),
                    lock_type: LockTypeInput::Exclusive,
                    duration_secs: None,
                },
            )
            .unwrap();

        tools
            .acquire_lock(
                &agent,
                AcquireLockInput {
                    resource: "file2.rs".to_string(),
                    lock_type: LockTypeInput::Exclusive,
                    duration_secs: None,
                },
            )
            .unwrap();

        // Query all locks
        let result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Locks,
                filters: QueryFilters::default(),
                limit: 50,
            },
        );

        assert!(result.is_ok());
        let output = result.unwrap();
        assert_eq!(output.total_count, 2);

        if let QueryResults::Locks(locks) = output.results {
            assert_eq!(locks.len(), 2);
        } else {
            panic!("Expected Locks results");
        }
    }

    #[test]
    fn test_coordinate_counters() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Increment counter
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Increment,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().counter_value, Some(1));

        // Increment again
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Increment,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert_eq!(result.unwrap().counter_value, Some(2));

        // Decrement
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Decrement,
                key: "task_count".to_string(),
                data: None,
            },
        );
        assert_eq!(result.unwrap().counter_value, Some(1));
    }

    #[test]
    fn test_coordinate_data() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Set coordination data
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Set,
                key: "config".to_string(),
                data: Some(serde_json::json!({"max_retries": 3, "timeout": 5000})),
            },
        );
        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Get coordination data
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Get,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.success);
        assert_eq!(
            output.data,
            Some(serde_json::json!({"max_retries": 3, "timeout": 5000}))
        );

        // Delete
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Delete,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(result.unwrap().success);

        // Verify deleted
        let result = tools.coordinate(
            &agent,
            CoordinateInput {
                action: CoordinationAction::Get,
                key: "config".to_string(),
                data: None,
            },
        );
        assert!(!result.unwrap().success);
    }

    #[test]
    fn test_save_progress() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        let result = tools.save_progress(
            &agent,
            SaveProgressInput {
                task_id: "task-1".to_string(),
                progress_percent: 50,
                message: "Implemented core logic".to_string(),
                artifacts: vec!["auth.rs".to_string()],
            },
        );

        assert!(result.is_ok());
        assert!(result.unwrap().success);

        // Query progress
        let query_result = tools.query(
            &agent,
            QueryInput {
                query_type: QueryType::Progress,
                filters: QueryFilters {
                    task_id: Some("task-1".to_string()),
                    ..Default::default()
                },
                limit: 10,
            },
        );
        assert!(query_result.is_ok());
        assert!(query_result.unwrap().total_count >= 1);
    }

    #[test]
    fn test_progress_validation() {
        let tools = create_test_tools();
        let agent = AgentId::new("agent-1");

        // Invalid progress percent
        let result = tools.save_progress(
            &agent,
            SaveProgressInput {
                task_id: "task-1".to_string(),
                progress_percent: 150,
                message: "Invalid".to_string(),
                artifacts: vec![],
            },
        );

        assert!(result.is_err());
    }
}

// ============================================================================
// SharedState Visibility Tests
// ============================================================================

mod shared_state_visibility {
    use super::*;

    #[test]
    fn test_shared_state_visibility_across_agents() {
        let state = Arc::new(SharedState::new());

        // Agent 1 updates progress
        state.update_progress("agent-1", "task-1", 50, "Working on it");

        // Agent 2 can see the progress (any agent can query by agent_id string)
        let progress = state.get_progress("agent-1", "task-1");
        assert!(progress.is_some());
        assert_eq!(progress.unwrap().progress_percent, 50);
    }

    #[test]
    fn test_coordination_data_sharing() {
        let state = Arc::new(SharedState::new());

        // Agent 1 sets data
        state
            .set_coordination(
                "shared_config",
                &serde_json::json!({"key": "value"}),
                "agent-1",
            )
            .unwrap();

        // Agent 2 can read it
        let data: Option<serde_json::Value> = state.get_coordination("shared_config");
        assert!(data.is_some());
        assert_eq!(data.unwrap()["key"], "value");
    }

    #[test]
    fn test_counter_sharing() {
        let state = Arc::new(SharedState::new());

        // Multiple agents increment
        state.increment("global_count");
        state.increment("global_count");
        state.increment("global_count");

        // All see the same value
        assert_eq!(state.get_counter("global_count"), 3);
    }

    #[test]
    fn test_lock_visibility() {
        let state = SharedState::new();
        let agent1 = AgentId::new("agent-1");

        state.acquire_lock("resource", &agent1, LockType::Exclusive, None);

        // Any agent can see who holds the lock
        let holder = state.lock_holder("resource");
        assert!(holder.is_some());
        assert_eq!(holder.unwrap().as_str(), "agent-1");

        // Check if locked
        assert!(state.is_locked("resource"));
    }

    #[test]
    fn test_snapshot_captures_all_state() {
        let state = SharedState::new();
        let agent = AgentId::new("agent-1");

        state.acquire_lock("file.rs", &agent, LockType::Exclusive, None);
        state.update_progress("agent-1", "task-1", 50, "Working");
        state.set_coordination("key", &"value", "agent-1").unwrap();
        state.set_counter("count", 42);

        let snapshot = state.snapshot();
        assert!(snapshot.locks.contains_key("file.rs"));
        assert!(!snapshot.progress.is_empty());
        assert!(!snapshot.coordination.is_empty());
        assert_eq!(snapshot.counters.get("count"), Some(&42));
    }
}

// ============================================================================
// TaskDAG Dependency Tests
// ============================================================================

mod task_dag_scheduling {
    use super::*;

    fn create_task(id: &str, module: &str, files: Vec<&str>) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            description: format!("Task {}", id),
            module: module.to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: 1000,
            priority: 1,
            affected_files: files.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn test_dag_respects_dependencies() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec![]));
        dag.add_task(create_task("task-2", "api", vec![]));
        dag.add_task(create_task("task-3", "db", vec![]));

        // task-2 depends on task-1
        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-2".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        // task-3 depends on task-2
        dag.add_dependency(TaskDependency {
            prerequisite: "task-2".to_string(),
            dependent: "task-3".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        dag.refresh_ready_status();

        // Only task-1 should be ready
        let ready = dag.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].info.id, "task-1");

        // Complete task-1
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        dag.complete_task(
            "task-1",
            TaskResult {
                success: true,
                output: "Done".to_string(),
                modified_files: vec![],
                artifacts: vec![],
            },
        )
        .unwrap();

        // Now task-2 should be ready
        let ready = dag.ready_tasks();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].info.id, "task-2");
    }

    #[test]
    fn test_dag_parallel_execution() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec![]));
        dag.add_task(create_task("task-2", "api", vec![]));
        dag.add_task(create_task("task-3", "db", vec![]));
        dag.add_task(create_task("task-4", "final", vec![]));

        // task-4 depends on all others
        for prereq in &["task-1", "task-2", "task-3"] {
            dag.add_dependency(TaskDependency {
                prerequisite: prereq.to_string(),
                dependent: "task-4".to_string(),
                dependency_type: DependencyType::Blocking,
            })
            .unwrap();
        }

        dag.refresh_ready_status();

        // task-1, task-2, task-3 should all be ready (parallel)
        let ready = dag.ready_tasks();
        assert_eq!(ready.len(), 3);

        // Verify parallel groups
        let groups = dag.parallel_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 3); // First parallel group
        assert_eq!(groups[1].len(), 1); // task-4 alone
    }

    #[test]
    fn test_dag_cycle_detection() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec![]));
        dag.add_task(create_task("task-2", "api", vec![]));

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
    fn test_dag_failure_blocks_dependents() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec![]));
        dag.add_task(create_task("task-2", "api", vec![]));

        dag.add_dependency(TaskDependency {
            prerequisite: "task-1".to_string(),
            dependent: "task-2".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        dag.refresh_ready_status();

        // Fail task-1
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        dag.fail_task("task-1", "Build error".to_string()).unwrap();

        // task-2 should be blocked
        assert_eq!(dag.get("task-2").unwrap().status, TaskStatus::Blocked);
    }

    #[test]
    fn test_dag_retry() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec![]));

        dag.refresh_ready_status();
        dag.start_task("task-1", AgentId::new("agent-1")).unwrap();
        dag.fail_task("task-1", "Temporary error".to_string())
            .unwrap();

        // Retry - sets to Pending and then calls refresh_ready_status which makes it Ready
        dag.retry_task("task-1").unwrap();
        // After retry with no unmet dependencies, task becomes Ready
        let status = dag.get("task-1").unwrap().status;
        assert!(status == TaskStatus::Ready || status == TaskStatus::Pending);
        assert_eq!(dag.get("task-1").unwrap().retry_count, 1);
    }

    #[test]
    fn test_dag_topological_order() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("a", "m", vec![]));
        dag.add_task(create_task("b", "m", vec![]));
        dag.add_task(create_task("c", "m", vec![]));
        dag.add_task(create_task("d", "m", vec![]));

        // d -> c -> b -> a
        dag.add_dependency(TaskDependency {
            prerequisite: "a".to_string(),
            dependent: "b".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();
        dag.add_dependency(TaskDependency {
            prerequisite: "b".to_string(),
            dependent: "c".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();
        dag.add_dependency(TaskDependency {
            prerequisite: "c".to_string(),
            dependent: "d".to_string(),
            dependency_type: DependencyType::Blocking,
        })
        .unwrap();

        let order = dag.topological_order();

        // a should come before b, b before c, c before d
        let pos_a = order.iter().position(|&x| x == "a").unwrap();
        let pos_b = order.iter().position(|&x| x == "b").unwrap();
        let pos_c = order.iter().position(|&x| x == "c").unwrap();
        let pos_d = order.iter().position(|&x| x == "d").unwrap();

        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
        assert!(pos_c < pos_d);
    }

    #[test]
    fn test_dag_conflicting_tasks() {
        let mut dag = TaskDAG::new();
        dag.add_task(create_task("task-1", "auth", vec!["shared.rs", "auth.rs"]));
        dag.add_task(create_task("task-2", "api", vec!["shared.rs", "api.rs"]));
        dag.add_task(create_task("task-3", "db", vec!["db.rs"]));

        let conflicts = dag.conflicting_tasks("task-1");
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts.contains(&"task-2"));
    }
}

// ============================================================================
// Cross-Workspace Coordination Tests
// ============================================================================

mod cross_workspace {
    use super::*;

    #[test]
    fn test_cross_workspace_detection() {
        let mut session =
            OrchestrationSession::new("Test", PathBuf::from("/tmp")).with_config(SessionConfig {
                enable_cross_workspace: true,
                ..Default::default()
            });

        let mut caps1 = AgentCapabilities::default();
        caps1.roles = vec![AgentRole::new("coder")];

        let mut caps2 = AgentCapabilities::default();
        caps2.roles = vec![AgentRole::new("coder")];

        session.register_agent("agent-1", "auth", "workspace-1", caps1);
        session.register_agent("agent-2", "api", "workspace-2", caps2);

        assert!(session.is_cross_workspace());
    }

    #[test]
    fn test_single_workspace() {
        let mut session =
            OrchestrationSession::new("Test", PathBuf::from("/tmp")).with_config(SessionConfig {
                enable_cross_workspace: true,
                ..Default::default()
            });

        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("coder")];

        session.register_agent("agent-1", "auth", "workspace-1", caps.clone());
        session.register_agent("agent-2", "api", "workspace-1", caps);

        assert!(!session.is_cross_workspace());
    }

    #[test]
    fn test_cross_workspace_disabled() {
        let mut session =
            OrchestrationSession::new("Test", PathBuf::from("/tmp")).with_config(SessionConfig {
                enable_cross_workspace: false,
                ..Default::default()
            });

        let mut caps = AgentCapabilities::default();
        caps.roles = vec![AgentRole::new("coder")];

        session.register_agent("agent-1", "auth", "workspace-1", caps.clone());
        session.register_agent("agent-2", "api", "workspace-2", caps);

        // Even with multiple workspaces, cross-workspace is disabled
        assert!(!session.is_cross_workspace());
    }
}

// ============================================================================
// Lock Acquisition Tests
// ============================================================================

mod lock_acquisition {
    use super::*;

    #[test]
    fn test_lock_exclusive() {
        let state = SharedState::new();
        let agent1 = AgentId::new("agent-1");
        let agent2 = AgentId::new("agent-2");

        // Acquire exclusive lock
        let result = state.acquire_lock("file.rs", &agent1, LockType::Exclusive, None);
        assert_eq!(result, LockResult::Acquired);

        // Same agent re-acquiring
        let result = state.acquire_lock("file.rs", &agent1, LockType::Exclusive, None);
        assert_eq!(result, LockResult::AlreadyHeld);

        // Different agent
        let result = state.acquire_lock("file.rs", &agent2, LockType::Exclusive, None);
        assert!(matches!(result, LockResult::Denied { holder } if holder == agent1));
    }

    #[test]
    fn test_lock_expiration() {
        let state = SharedState::new();
        let agent1 = AgentId::new("agent-1");

        // Acquire with very short duration
        state.acquire_lock(
            "file.rs",
            &agent1,
            LockType::Exclusive,
            Some(Duration::from_millis(1)),
        );

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(5));

        // Another agent can now acquire since lock expired
        let agent2 = AgentId::new("agent-2");
        let result = state.acquire_lock("file.rs", &agent2, LockType::Exclusive, None);
        assert_eq!(result, LockResult::Acquired);
    }

    #[test]
    fn test_locks_held_by_agent() {
        let state = SharedState::new();
        let agent = AgentId::new("agent-1");

        state.acquire_lock("file1.rs", &agent, LockType::Exclusive, None);
        state.acquire_lock("file2.rs", &agent, LockType::Exclusive, None);
        state.acquire_lock("file3.rs", &agent, LockType::Exclusive, None);

        let locks = state.locks_held_by(&agent);
        assert_eq!(locks.len(), 3);
    }

    #[test]
    fn test_cleanup_expired_locks() {
        let state = SharedState::new();
        let agent = AgentId::new("agent-1");

        state.acquire_lock(
            "file1.rs",
            &agent,
            LockType::Exclusive,
            Some(Duration::from_millis(1)),
        );
        state.acquire_lock(
            "file2.rs",
            &agent,
            LockType::Exclusive,
            Some(Duration::from_millis(1)),
        );

        std::thread::sleep(Duration::from_millis(5));

        let cleaned = state.cleanup_expired_locks();
        assert_eq!(cleaned, 2);
    }
}

// ============================================================================
// Checkpoint Tests
// ============================================================================

mod checkpoints {
    use super::*;

    #[test]
    fn test_manual_checkpoint_creation() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        let checkpoint = session.create_checkpoint(CheckpointReason::Manual {
            description: "Test checkpoint".to_string(),
        });
        assert!(!checkpoint.id.is_empty());
        assert!(matches!(checkpoint.reason, CheckpointReason::Manual { .. }));
    }

    #[test]
    fn test_context_budget_checkpoint() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"))
            .with_context_budget(100000, 5000);

        session.start();

        // Add significant usage
        session.add_context_usage(85000); // Above 80% threshold

        assert!(session.should_checkpoint_for_budget());
    }

    #[test]
    fn test_phase_transition_checkpoint() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        // Advance phase creates checkpoint internally
        session.advance_phase(); // Setup -> Planning

        // Verify checkpoint exists
        assert!(session.summary().checkpoint_count >= 1);
    }
}

// ============================================================================
// Notification Tests
// ============================================================================

mod notifications {
    use super::*;

    #[test]
    fn test_session_notifications() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        session.notify(
            "agent-1",
            "*",
            NotificationType::Text {
                message: "Hello everyone".to_string(),
            },
        );

        let agent_id = AgentId::new("agent-2");
        let notifications = session.notifications_for_agent(&agent_id, 10);
        assert!(!notifications.is_empty());
    }

    #[test]
    fn test_conflict_notification() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        session.notify_conflict(
            "conflict-1",
            vec!["agent-1".to_string(), "agent-2".to_string()],
            vec!["shared.rs".to_string()],
            "Concurrent modification detected",
        );

        let agent_id = AgentId::new("agent-1");
        let notifications = session.notifications_for_agent(&agent_id, 10);
        let has_conflict = notifications.iter().any(|n| {
            matches!(
                &n.notification_type,
                NotificationType::ConflictDetected { .. }
            )
        });
        assert!(has_conflict);
    }

    #[test]
    fn test_notification_acknowledgment() {
        let mut session = OrchestrationSession::new("Test", PathBuf::from("/tmp"));
        session.start();

        let notification_id = session.notify(
            "agent-1",
            "*",
            NotificationType::Text {
                message: "Test".to_string(),
            },
        );

        session.acknowledge_notification(notification_id, "agent-2");

        // Notification should be acknowledged
        let agent_id = AgentId::new("agent-2");
        let notifications = session.notifications_for_agent(&agent_id, 10);
        if let Some(n) = notifications.iter().find(|n| n.id == notification_id) {
            assert!(n.is_acknowledged_by("agent-2"));
        }
    }
}

// ============================================================================
// End-to-End Session Workflow Test
// ============================================================================

mod e2e_workflow {
    use super::*;

    #[test]
    fn test_complete_session_workflow() {
        // Create session
        let mut session = OrchestrationSession::new(
            "Implement authentication feature",
            PathBuf::from("/tmp/test-project"),
        )
        .with_context_budget(200000, 10000)
        .with_metadata("project", "test-app");

        // Register participants
        let mut coder_caps = AgentCapabilities::default();
        coder_caps.roles = vec![AgentRole::new("coder")];
        coder_caps.max_concurrent_tasks = 2;

        let mut reviewer_caps = AgentCapabilities::default();
        reviewer_caps.roles = vec![AgentRole::new("reviewer")];
        reviewer_caps.max_concurrent_tasks = 1;

        session.register_agent("auth-coder-0", "auth", "project-a", coder_caps.clone());
        session.register_agent("auth-reviewer-0", "auth", "project-a", reviewer_caps);

        // Add tasks with dependencies
        session.add_task(TaskInfo {
            id: "implement-auth".to_string(),
            description: "Implement authentication module".to_string(),
            module: "auth".to_string(),
            required_role: "coder".to_string(),
            estimated_complexity: 2000,
            priority: 1,
            affected_files: vec!["src/auth/mod.rs".to_string()],
        });

        session.add_task(TaskInfo {
            id: "review-auth".to_string(),
            description: "Review authentication implementation".to_string(),
            module: "auth".to_string(),
            required_role: "reviewer".to_string(),
            estimated_complexity: 500,
            priority: 2,
            affected_files: vec!["src/auth/mod.rs".to_string()],
        });

        session
            .add_task_dependency("implement-auth", "review-auth", DependencyType::Blocking)
            .unwrap();

        // Start session
        session.start();
        assert_eq!(session.status(), SessionStatus::Active);

        // Advance to implementation phase
        session.set_phase(Phase::Implementation);

        // Assign and complete implementation task
        let coder_id = AgentId::new("auth-coder-0");
        session.assign_task("implement-auth", &coder_id).unwrap();

        // Review task cannot be started yet (has unmet dependency)
        let reviewer_id = AgentId::new("auth-reviewer-0");
        let result = session.assign_task("review-auth", &reviewer_id);
        assert!(
            result.is_err(),
            "review-auth should fail to start before implement-auth completes"
        );

        // Complete implementation
        session
            .complete_task(
                "implement-auth",
                &coder_id,
                TaskResult {
                    success: true,
                    output: "Authentication module implemented".to_string(),
                    modified_files: vec!["src/auth/mod.rs".to_string()],
                    artifacts: vec![],
                },
            )
            .unwrap();

        // Now review task can be assigned
        session.assign_task("review-auth", &reviewer_id).unwrap();
        session
            .complete_task(
                "review-auth",
                &reviewer_id,
                TaskResult {
                    success: true,
                    output: "Code review passed".to_string(),
                    modified_files: vec![],
                    artifacts: vec!["review-report.md".to_string()],
                },
            )
            .unwrap();

        // All tasks complete
        assert!(session.all_tasks_complete());

        // Complete session
        session.complete();
        assert_eq!(session.status(), SessionStatus::Completed);

        // Verify final stats
        let stats = session.task_stats();
        assert_eq!(stats.completed, 2);
        assert_eq!(stats.failed, 0);
    }
}
