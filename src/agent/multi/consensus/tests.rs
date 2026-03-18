//! Tests for the consensus engine.

use super::*;
use super::super::traits::{TaskPriority, extract_field};
use crate::domain::Severity;

#[test]
fn test_proposal_score_weights() {
    let score = AgentProposalScore {
        agent_id: "test".into(),
        module_relevance: 1.0,
        historical_accuracy: 1.0,
        evidence_strength: 1.0,
        confidence: 1.0,
    };

    let total = score.weighted_score();
    assert!((total - 1.0).abs() < 0.001);
}

#[test]
fn test_proposal_score_weighted() {
    let score = AgentProposalScore {
        agent_id: "test".into(),
        module_relevance: 0.9,
        historical_accuracy: 0.7,
        evidence_strength: 0.8,
        confidence: 0.6,
    };

    let expected = 0.9 * 0.35 + 0.7 * 0.25 + 0.8 * 0.25 + 0.6 * 0.15;
    assert!((score.weighted_score() - expected).abs() < 0.001);
}

#[test]
fn test_extract_field() {
    let line = r#"task: "implement auth" | module: "auth" | deps: "db" | priority: "high""#;
    assert_eq!(extract_field(line, "task:"), Some("implement auth".into()));
    assert_eq!(extract_field(line, "module:"), Some("auth".into()));
    assert_eq!(extract_field(line, "deps:"), Some("db".into()));
    assert_eq!(extract_field(line, "priority:"), Some("high".into()));
}

#[test]
fn test_parse_agent_requests() {
    let synthesis = r#"
CONSENSUS: PARTIAL

We need more expertise.
AGENT_NEEDED: module="payments" reason="Payment integration required"
AGENT_NEEDED: module="security" reason="Auth review needed"
"#;

    let requests = ConsensusEngine::parse_agent_requests(synthesis);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].module, "payments");
    assert_eq!(requests[1].module, "security");
}

#[test]
fn test_conflict_severity() {
    let conflict = Conflict::new("c1", "API design", Severity::Error)
        .with_agents(vec!["a1".into(), "a2".into()])
        .with_positions(vec!["REST".into(), "GraphQL".into()]);

    assert_eq!(conflict.severity, Severity::Error);
}

#[test]
fn test_consensus_task_serialization() {
    let task = ConsensusTask {
        id: "task-1".into(),
        description: "Implement auth".into(),
        assigned_module: Some("auth".into()),
        dependencies: vec!["task-0".into()],
        priority: TaskPriority::High,
        estimated_complexity: TaskComplexity::Medium,
        files_affected: vec!["src/auth.rs".into()],
        priority_score: 0.8,
    };

    let json = serde_json::to_string(&task).unwrap();
    let parsed: ConsensusTask = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.id, "task-1");
    assert_eq!(parsed.estimated_complexity, TaskComplexity::Medium);
}

#[test]
fn test_synthesis_output_parsing() {
    let json = r#"{
        "consensus": "agreed",
        "plan": "Implement the feature",
        "tasks": [
            {
                "id": "task-1",
                "description": "Add endpoint",
                "assigned_module": "api",
                "dependencies": [],
                "priority": "high",
                "estimated_complexity": "medium",
                "files_affected": ["src/api.rs"]
            }
        ],
        "conflicts": [],
        "dissents": [],
        "agent_needed": []
    }"#;

    let output: ConsensusSynthesisOutput = serde_json::from_str(json).unwrap();
    assert!(output.is_agreed());
    assert_eq!(output.tasks.len(), 1);
    assert_eq!(output.tasks[0].estimated_complexity, TaskComplexity::Medium);
}

#[test]
fn test_agent_performance_history() {
    let mut history = AgentPerformanceHistory::default();
    assert_eq!(history.accuracy_rate(), 0.5); // Default for new agents

    history.total_proposals = 10;
    history.accepted_proposals = 8;
    assert!((history.accuracy_rate() - 0.8).abs() < 0.001);

    history.module_contributions.insert("auth".into(), 5);
    history.module_contributions.insert("api".into(), 5);
    assert!((history.module_strength("auth") - 0.5).abs() < 0.001);
}

#[test]
fn test_needs_additional_agents() {
    assert!(ConsensusEngine::needs_additional_agents(
        "AGENT_NEEDED: module=\"x\""
    ));
    assert!(ConsensusEngine::needs_additional_agents(
        "Need module expert"
    ));
    assert!(ConsensusEngine::needs_additional_agents(
        "require specialist"
    ));
    assert!(!ConsensusEngine::needs_additional_agents("All good"));
}

#[test]
fn test_consensus_task_workspace_extraction() {
    // Unqualified module - workspace() should return None
    let task1 = ConsensusTask::new("t1", "Task 1").with_module("auth");
    assert!(task1.workspace().is_none());
    assert!(!task1.is_qualified());
    assert_eq!(task1.module_name(), Some("auth"));

    // Qualified module - workspace() should return workspace
    let task2 = ConsensusTask::new("t2", "Task 2").with_qualified_module("project-a", "auth");
    assert_eq!(task2.workspace(), Some("project-a"));
    assert!(task2.is_qualified());
    assert_eq!(task2.module_name(), Some("auth"));

    // Full qualification with domain
    let task3 = ConsensusTask::new("t3", "Task 3").with_full_qualification(
        "project-a",
        "security",
        "auth",
    );
    assert_eq!(task3.workspace(), Some("project-a"));
    assert!(task3.is_qualified());
    assert_eq!(task3.module_name(), Some("auth"));
    assert!(task3.is_in_workspace("project-a"));
    assert!(!task3.is_in_workspace("project-b"));

    // Parse qualified should work
    let parsed = task3.parse_qualified();
    assert!(parsed.is_some());
    let qm = parsed.unwrap();
    assert_eq!(qm.workspace, "project-a");
    assert_eq!(qm.domain, Some("security".to_string()));
    assert_eq!(qm.module, "auth");
}

#[test]
fn test_conflict_new_with_explicit_severity() {
    let c1 = Conflict::new("c1", "API breaking change", Severity::Critical);
    assert_eq!(c1.severity, Severity::Critical);
    assert_eq!(c1.topic, "API breaking change");
    assert!(c1.agents.is_empty());

    let c2 = Conflict::new("c2", "Type mismatch", Severity::Error);
    assert_eq!(c2.severity, Severity::Error);

    let c3 = Conflict::new("c3", "Naming conflict", Severity::Warning);
    assert_eq!(c3.severity, Severity::Warning);

    let c4 = Conflict::new("c4", "Style preference", Severity::Info);
    assert_eq!(c4.severity, Severity::Info);
}

#[test]
fn test_conflict_builder_methods() {
    let c = Conflict::new("c1", "API design", Severity::Warning)
        .with_agents(vec!["auth".into(), "api".into()])
        .with_positions(vec!["REST".into(), "GraphQL".into()])
        .with_severity(Severity::Error);
    assert_eq!(c.agents.len(), 2);
    assert_eq!(c.positions.len(), 2);
    assert_eq!(c.topic, "API design");
    assert_eq!(c.severity, Severity::Error);
}

#[test]
fn test_conflict_from_tier_results() {
    // Simulates how coordinator/recovery.rs constructs conflicts from tiers
    let descriptions = [
        "Critical breaking change",
        "Minor style issue",
        "Type mismatch in UserDTO",
    ];

    let conflicts: Vec<Conflict> = descriptions
        .iter()
        .enumerate()
        .map(|(i, desc)| {
            Conflict::new(
                format!("tier-conflict-{}", i),
                *desc,
                Severity::Warning,
            )
        })
        .collect();

    assert_eq!(conflicts.len(), 3);
    assert_eq!(conflicts[0].id, "tier-conflict-0");
    assert_eq!(conflicts[0].severity, Severity::Warning);
    assert_eq!(conflicts[1].topic, "Minor style issue");
}
