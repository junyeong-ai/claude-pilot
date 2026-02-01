use claude_pilot::config::PilotConfig;

#[test]
fn test_default_config() {
    let config = PilotConfig::default();

    assert_eq!(config.orchestrator.max_iterations, 100);
    assert_eq!(config.orchestrator.max_parallel_tasks, 10);
    assert!(config.orchestrator.auto_commit);

    assert_eq!(config.isolation.small_change_threshold, 2);
    assert_eq!(config.isolation.large_change_threshold, 10);

    assert_eq!(config.git.default_branch, "main");
    assert_eq!(config.git.branch_prefix, "pilot");
    assert!(!config.git.auto_push);

    assert!(!config.learning.auto_extract);
    assert!(config.learning.extract_skills);
    assert!(config.learning.extract_rules);
    assert!(!config.learning.extract_agents); // Experimental - disabled by default
    assert!((config.learning.min_confidence - 0.6).abs() < f32::EPSILON);

    assert_eq!(config.agent.max_retries, 3);
    assert_eq!(config.agent.timeout_secs, 600);
    assert_eq!(config.agent.planning_timeout_secs, 0);

    // Notification config defaults
    assert!(config.notification.enabled);
    assert!(config.notification.desktop);
    assert!(config.notification.event_log);
    assert!(config.notification.hook_command.is_none());
}

#[test]
fn test_config_clone() {
    let config = PilotConfig::default();
    let cloned = config.clone();

    assert_eq!(
        config.orchestrator.max_iterations,
        cloned.orchestrator.max_iterations
    );
    assert_eq!(config.git.default_branch, cloned.git.default_branch);
}
