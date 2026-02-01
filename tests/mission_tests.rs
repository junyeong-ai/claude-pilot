use claude_pilot::MissionState;
use claude_pilot::mission::{
    IsolationMode, Learning, LearningCategory, Mission, Priority, Task, TaskStatus,
};

#[test]
fn test_mission_creation() {
    let mission = Mission::new("m-001", "Add OAuth2 authentication");

    assert_eq!(mission.id, "m-001");
    assert_eq!(mission.description, "Add OAuth2 authentication");
    assert_eq!(mission.status, MissionState::Pending);
    assert_eq!(mission.isolation, IsolationMode::Auto);
    assert!(mission.tasks.is_empty());
}

#[test]
fn test_mission_with_builders() {
    let mission = Mission::new("m-002", "Refactor database layer")
        .with_isolation(IsolationMode::Worktree)
        .with_priority(Priority::P1)
        .with_base_branch("develop");

    assert_eq!(mission.isolation, IsolationMode::Worktree);
    assert!(matches!(mission.priority, Priority::P1));
    assert_eq!(mission.base_branch, "develop");
}

#[test]
fn test_mission_branch_name() {
    let mission = Mission::new("m-042", "Test mission");
    assert_eq!(mission.branch_name(), "pilot/m-042");
}

#[test]
fn test_mission_progress() {
    let mut mission = Mission::new("m-001", "Test");

    let mut task1 = Task::new("T001", "First task");
    task1.status = TaskStatus::Completed;

    let task2 = Task::new("T002", "Second task");

    let mut task3 = Task::new("T003", "Third task");
    task3.status = TaskStatus::Completed;

    mission.tasks = vec![task1, task2, task3];

    let progress = mission.progress();
    assert_eq!(progress.completed, 2);
    assert_eq!(progress.total, 3);
    assert_eq!(progress.percentage, 66);
}

#[test]
fn test_task_creation() {
    let task = Task::new("T001", "Implement login form")
        .with_phase("Phase 1")
        .with_dependencies(vec!["T000".to_string()]);

    assert_eq!(task.id, "T001");
    assert_eq!(task.description, "Implement login form");
    assert_eq!(task.phase, Some("Phase 1".to_string()));
    assert_eq!(task.dependencies, vec!["T000"]);
    assert_eq!(task.status, TaskStatus::Pending);
}

#[test]
fn test_task_can_start() {
    let task = Task::new("T002", "Task with deps").with_dependencies(vec!["T001".to_string()]);

    assert!(!task.can_start(&[]));
    assert!(task.can_start(&["T001"]));
}

#[test]
fn test_task_retry_logic() {
    let mut task = Task::new("T001", "Failing task");
    task.max_retries = 3;

    assert!(task.can_retry());

    task.fail("Error 1".to_string());
    assert!(task.can_retry());
    assert_eq!(task.retry_count, 1);
    assert_eq!(task.status, TaskStatus::Pending);

    task.fail("Error 2".to_string());
    assert!(task.can_retry());
    assert_eq!(task.retry_count, 2);
    assert_eq!(task.status, TaskStatus::Pending);

    task.fail("Error 3".to_string());
    assert!(task.can_retry());
    assert_eq!(task.retry_count, 3);
    assert_eq!(task.status, TaskStatus::Pending);

    task.fail("Error 4".to_string());
    assert!(!task.can_retry());
    assert_eq!(task.retry_count, 4);
    assert_eq!(task.status, TaskStatus::Failed);
}

#[test]
fn test_mission_status_transitions() {
    assert!(!MissionState::Pending.is_terminal());
    assert!(!MissionState::Running.is_terminal());
    assert!(MissionState::Completed.is_terminal());
    assert!(MissionState::Failed.is_terminal());
    assert!(MissionState::Cancelled.is_terminal());

    assert!(MissionState::Running.is_active());
    assert!(MissionState::Verifying.is_active());
    assert!(!MissionState::Pending.is_active());

    assert!(MissionState::Paused.can_resume());
    assert!(!MissionState::Failed.can_resume());
    assert!(!MissionState::Completed.can_resume());

    assert!(MissionState::Failed.can_retry());
    assert!(MissionState::Cancelled.can_retry());
    assert!(!MissionState::Paused.can_retry());
    assert!(!MissionState::Completed.can_retry());
}

#[test]
fn test_learning_creation() {
    let learning = Learning::new(
        LearningCategory::Gotcha,
        "Always validate input before processing",
    )
    .with_source_task("T005");

    assert_eq!(learning.category, LearningCategory::Gotcha);
    assert_eq!(learning.content, "Always validate input before processing");
    assert_eq!(learning.source_task, Some("T005".to_string()));
}

#[test]
fn test_next_tasks() {
    let mut mission = Mission::new("m-001", "Test");

    let task1 = Task::new("T001", "First").with_dependencies(vec![]);
    let task2 = Task::new("T002", "Second").with_dependencies(vec!["T001".to_string()]);
    let task3 = Task::new("T003", "Third").with_dependencies(vec![]);

    mission.tasks = vec![task1, task2, task3];

    let next = mission.next_tasks();
    assert_eq!(next.len(), 2);
    assert!(next.iter().any(|t| t.id == "T001"));
    assert!(next.iter().any(|t| t.id == "T003"));
}

#[test]
fn test_isolation_mode_display() {
    assert_eq!(IsolationMode::Auto.to_string(), "auto");
    assert_eq!(IsolationMode::None.to_string(), "none");
    assert_eq!(IsolationMode::Branch.to_string(), "branch");
    assert_eq!(IsolationMode::Worktree.to_string(), "worktree");
}

#[test]
fn test_priority_from_str() {
    use std::str::FromStr;

    assert!(matches!(Priority::from_str("p1").unwrap(), Priority::P1));
    assert!(matches!(Priority::from_str("P2").unwrap(), Priority::P2));
    assert!(matches!(Priority::from_str("high").unwrap(), Priority::P1));
    assert!(matches!(Priority::from_str("low").unwrap(), Priority::P3));
    assert!(Priority::from_str("invalid").is_err());
}
