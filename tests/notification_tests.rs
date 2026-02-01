use claude_pilot::notification::{EventType, MissionEvent};

#[test]
fn test_event_type_as_str() {
    assert_eq!(EventType::MissionCreated.as_str(), "mission.created");
    assert_eq!(EventType::MissionStarted.as_str(), "mission.started");
    assert_eq!(EventType::MissionCompleted.as_str(), "mission.completed");
    assert_eq!(EventType::MissionFailed.as_str(), "mission.failed");
    assert_eq!(EventType::MissionCancelled.as_str(), "mission.cancelled");
    assert_eq!(EventType::MissionPaused.as_str(), "mission.paused");
    assert_eq!(EventType::TaskStarted.as_str(), "task.started");
    assert_eq!(EventType::TaskCompleted.as_str(), "task.completed");
    assert_eq!(EventType::TaskFailed.as_str(), "task.failed");
    assert_eq!(
        EventType::VerificationFailed.as_str(),
        "verification.failed"
    );
}

#[test]
fn test_event_type_is_error() {
    assert!(EventType::MissionFailed.is_error());
    assert!(EventType::TaskFailed.is_error());
    assert!(EventType::VerificationFailed.is_error());

    assert!(!EventType::MissionCreated.is_error());
    assert!(!EventType::MissionCompleted.is_error());
    assert!(!EventType::TaskCompleted.is_error());
}

#[test]
fn test_mission_event_creation() {
    let event = MissionEvent::new(EventType::MissionCreated, "m-001");

    assert!(matches!(event.event_type, EventType::MissionCreated));
    assert_eq!(event.mission_id, "m-001");
    assert!(event.task_id.is_none());
    assert!(event.message.is_none());
    assert!(event.progress.is_none());
}

#[test]
fn test_mission_event_builders() {
    let event = MissionEvent::new(EventType::TaskCompleted, "m-001")
        .with_task("T001")
        .with_message("Task done")
        .with_progress(5, 10);

    assert!(matches!(event.event_type, EventType::TaskCompleted));
    assert_eq!(event.mission_id, "m-001");
    assert_eq!(event.task_id, Some("T001".to_string()));
    assert_eq!(event.message, Some("Task done".to_string()));
    assert_eq!(event.progress, Some((5, 10)));
}

#[test]
fn test_mission_event_title() {
    let event = MissionEvent::new(EventType::MissionCompleted, "m-001");
    let title = event.title();

    assert!(title.contains("Claude-Pilot"));
    assert!(title.contains("mission.completed"));
}

#[test]
fn test_mission_event_body() {
    let event = MissionEvent::new(EventType::TaskCompleted, "m-001")
        .with_task("T001")
        .with_progress(3, 5)
        .with_message("All tests passed");

    let body = event.body();

    assert!(body.contains("Mission: m-001"));
    assert!(body.contains("Task: T001"));
    assert!(body.contains("Progress: 3/5"));
    assert!(body.contains("All tests passed"));
}
