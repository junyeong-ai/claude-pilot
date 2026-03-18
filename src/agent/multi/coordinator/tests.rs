use std::sync::Arc;

use super::super::consensus::TaskComplexity;
use super::super::pool::AgentPool;
use super::super::traits::{extract_files_from_output, TaskPriority};
use super::types::ComplexityMetrics;
use super::Coordinator;
use crate::agent::multi::consensus::ConsensusTask;
use crate::config::MultiAgentConfig;

#[test]
fn test_extract_files() {
    let output = r#"
Found relevant files:
- src/auth/login.rs
- src/utils.rs (confidence: 0.9)
Also check `src/lib.rs` and src/main.rs
"#;

    let files = extract_files_from_output(output, 20, None);
    assert!(!files.is_empty());
    assert!(files.iter().any(|f| f.contains("src/auth")));
}

#[test]
fn test_partition_tasks() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    let tasks = vec![
        ConsensusTask {
            id: "1".to_string(),
            description: "Task 1".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/a.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
        ConsensusTask {
            id: "2".to_string(),
            description: "Task 2".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec!["1".to_string()],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/b.rs".to_string()],
            estimated_complexity: TaskComplexity::Medium,
        },
        ConsensusTask {
            id: "3".to_string(),
            description: "Task 3".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/c.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
    ];

    let (independent, dependent) = coordinator.partition_tasks(&tasks);
    assert_eq!(independent.len(), 2); // Tasks 1 and 3 (no deps, different files)
    assert_eq!(dependent.len(), 1); // Task 2 (explicit dependency)
}

#[test]
fn test_partition_tasks_file_conflict() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    // Task 1 and Task 2 both modify src/shared.rs - conflict!
    let tasks = vec![
        ConsensusTask {
            id: "1".to_string(),
            description: "Task 1".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/shared.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
        ConsensusTask {
            id: "2".to_string(),
            description: "Task 2".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![], // No explicit deps but has file conflict
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/shared.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
        ConsensusTask {
            id: "3".to_string(),
            description: "Task 3".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/other.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
    ];

    let (independent, dependent) = coordinator.partition_tasks(&tasks);
    // Task 1 and 3 can run in parallel (different files)
    // Task 2 conflicts with Task 1 on shared.rs, moved to dependent
    assert_eq!(independent.len(), 2);
    assert_eq!(dependent.len(), 1);
    assert_eq!(dependent[0].id, "2"); // Task 2 was marked dependent due to conflict
}

#[test]
fn test_partition_tasks_path_normalization() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    // Test path normalization: relative vs absolute, redundant separators
    let tasks = vec![
        ConsensusTask {
            id: "1".to_string(),
            description: "Task 1".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["./src/main.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
        ConsensusTask {
            id: "2".to_string(),
            description: "Task 2".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            // Same file but with different path representation
            files_affected: vec!["src/main.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
        ConsensusTask {
            id: "3".to_string(),
            description: "Task 3".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec!["src/lib.rs".to_string()],
            estimated_complexity: TaskComplexity::Low,
        },
    ];

    let (independent, dependent) = coordinator.partition_tasks(&tasks);

    // Task 1 and 2 should be detected as conflicting (both modify src/main.rs)
    // Task 3 should be independent (different file)
    // The exact partitioning depends on whether src/main.rs exists in the test environment
    // If it exists: Task 1 independent, Task 2 dependent (conflict), Task 3 independent
    // If it doesn't exist: fallback to path normalization, still should detect conflict
    assert!(
        (independent.len() == 2 && dependent.len() == 1)
            || (independent.len() == 1 && dependent.len() == 2),
        "Expected 2 independent and 1 dependent, or 1 independent and 2 dependent, got {} independent and {} dependent",
        independent.len(),
        dependent.len()
    );

    // Verify Task 3 is always in independent (different file)
    assert!(
        independent.iter().any(|t| t.id == "3"),
        "Task 3 should be independent (different file)"
    );
}

#[test]
fn test_coordinator_metrics() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    let metrics = coordinator.metrics();
    assert_eq!(metrics.total_executions, 0);
    assert_eq!(metrics.success_rate, 0.0);
}

#[test]
fn test_coordinator_shutdown() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    assert!(!coordinator.is_shutdown());
    coordinator.shutdown();
    assert!(coordinator.is_shutdown());
}

#[test]
fn test_coordinator_pool_stats() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    let stats = coordinator.pool_stats();
    assert_eq!(stats.agent_count, 0);
    assert!(!stats.is_shutdown);
}

#[test]
fn test_prepare_tasks() {
    let config = MultiAgentConfig::default();
    let pool = Arc::new(AgentPool::new(config.clone()));
    let coordinator = Coordinator::new(config, pool);

    let consensus_tasks = vec![
        ConsensusTask {
            id: "task-1".to_string(),
            description: "High priority simple task".to_string(),
            assigned_module: Some("api".to_string()),
            dependencies: vec![],
            priority: TaskPriority::Critical,
            estimated_complexity: TaskComplexity::Trivial,
            files_affected: vec!["src/api.rs".to_string()],
            priority_score: 0.0,
        },
        ConsensusTask {
            id: "task-2".to_string(),
            description: "Low priority complex task".to_string(),
            assigned_module: Some("db".to_string()),
            dependencies: vec!["task-1".to_string()],
            priority: TaskPriority::Low,
            estimated_complexity: TaskComplexity::Complex,
            files_affected: vec!["src/db.rs".to_string()],
            priority_score: 0.0,
        },
    ];

    let impl_tasks = coordinator.prepare_tasks(consensus_tasks);

    // High priority + low complexity should come first
    assert_eq!(impl_tasks[0].id, "task-1");
    assert!(impl_tasks[0].priority_score > impl_tasks[1].priority_score);
}

#[test]
fn test_complexity_metrics_calculation() {
    let tasks = vec![
        ConsensusTask {
            id: "1".to_string(),
            description: "Trivial task".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec![],
            estimated_complexity: TaskComplexity::Trivial, // weight: 0.2
        },
        ConsensusTask {
            id: "2".to_string(),
            description: "Medium task".to_string(),
            priority: TaskPriority::Normal,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.5,
            files_affected: vec![],
            estimated_complexity: TaskComplexity::Medium, // weight: 0.6
        },
        ConsensusTask {
            id: "3".to_string(),
            description: "Complex task".to_string(),
            priority: TaskPriority::High,
            dependencies: vec![],
            assigned_module: None,
            priority_score: 0.8,
            files_affected: vec![],
            estimated_complexity: TaskComplexity::Complex, // weight: 1.0
        },
    ];

    // Total weight: 0.2 + 0.6 + 1.0 = 1.8
    let completed_ids = vec!["1".to_string(), "2".to_string()]; // Completed: 0.2 + 0.6 = 0.8
    let metrics = ComplexityMetrics::from_tasks(&tasks, &completed_ids);

    assert_eq!(metrics.by_level.get(&TaskComplexity::Trivial), Some(&1));
    assert_eq!(metrics.by_level.get(&TaskComplexity::Medium), Some(&1));
    assert_eq!(metrics.by_level.get(&TaskComplexity::Complex), Some(&1));
    assert!((metrics.total_score - 1.8).abs() < 0.001);
    assert!((metrics.completed_score - 0.8).abs() < 0.001);
    // Completion: 0.8 / 1.8 * 100 = 44.44%
    assert!((metrics.completion_pct - 44.44).abs() < 0.1);
}

#[test]
fn test_complexity_metrics_empty_tasks() {
    let tasks: Vec<ConsensusTask> = vec![];
    let completed_ids: Vec<String> = vec![];
    let metrics = ComplexityMetrics::from_tasks(&tasks, &completed_ids);

    assert!(metrics.by_level.is_empty());
    assert_eq!(metrics.total_score, 0.0);
    assert_eq!(metrics.completed_score, 0.0);
    assert_eq!(metrics.completion_pct, 100.0); // No tasks = 100% complete
}

#[test]
fn test_normalize_issue_key_normal_length() {
    let issue = "File src/main.rs has an error on line 42";
    let normalized = Coordinator::normalize_issue_key(issue);
    // "File src/main.rs has an error on line 42" -> 10 words after normalization
    assert_eq!(normalized, "file src main rs has an error on line 42");
    assert!(normalized.len() < 100);
}

#[test]
fn test_normalize_issue_key_long_input() {
    // Create a very long issue description (1000 chars)
    let long_issue = "a".repeat(1000);
    let normalized = Coordinator::normalize_issue_key(&long_issue);

    // Should be truncated and not cause memory issues
    // The function truncates at 500 chars, filters to alphanumeric,
    // takes first 10 words, joins with spaces
    assert!(normalized.len() <= 500);
}

#[test]
fn test_normalize_issue_key_special_chars() {
    let issue = "Error!!! @@@File### src/main.rs:123 --- failed!!!";
    let normalized = Coordinator::normalize_issue_key(issue);
    // Special chars filtered, whitespace preserved, first 10 words taken
    assert_eq!(normalized, "error file src main rs 123 failed");
}

#[test]
fn test_normalize_issue_key_max_words() {
    let issue = "one two three four five six seven eight nine ten eleven twelve";
    let normalized = Coordinator::normalize_issue_key(issue);
    // Should only take first 10 words
    assert_eq!(
        normalized,
        "one two three four five six seven eight nine ten"
    );
}
