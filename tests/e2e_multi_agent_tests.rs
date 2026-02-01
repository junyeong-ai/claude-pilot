#![allow(clippy::field_reassign_with_default)]
//! End-to-end tests for multi-agent architecture.
//!
//! These tests verify the complete workflow of the multi-agent system,
//! from mission creation through consensus, implementation, and verification.

mod fixtures;

use std::sync::Arc;

use claude_pilot::agent::multi::context::{ContextComposer, PersonaLoader};
use claude_pilot::agent::multi::messaging::{
    AgentMessage, AgentMessageBus, MessagePayload, MessageType,
};
use claude_pilot::agent::multi::rules::RuleRegistry;
use claude_pilot::agent::multi::skills::{SkillRegistry, SkillType};
use claude_pilot::agent::multi::{
    AgentPoolBuilder, AgentRole, AgentTask, Coordinator, RoleCategory, TaskContext, TaskPriority,
};
use claude_pilot::config::MultiAgentConfig;
use claude_pilot::state::{DomainEvent, EventPayload, EventStore, RoundOutcome, VoteDecision};

use fixtures::mock_agent::{MockTaskAgentBuilder, ResponseScenario};
use fixtures::project::{ErrorType, TestProjectBuilder, TestProjectFixture};
use fixtures::verifier::{EventPattern, EventVerifier};

mod scenario_simple {
    use super::*;

    #[tokio::test]
    async fn test_simple_mission_flow() {
        // Setup: Create a simple Rust project
        let fixture = TestProjectFixture::rust_project();

        // Create mock agent with standard responses
        let mock = MockTaskAgentBuilder::new()
            .static_response(
                "research",
                r#"{"affected_files": ["src/lib.rs"], "complexity": "trivial"}"#,
            )
            .static_response("coder", "// Implementation complete")
            .sequential_responses(
                "verifier",
                vec![
                    r#"{"status": "pass", "issues": []}"#,
                    r#"{"status": "pass", "issues": []}"#,
                ],
            )
            .build();

        // Verify mock responses work
        let response = mock.run_prompt("research task", fixture.path()).await;
        assert!(response.contains("affected_files"));

        // Verify call tracking
        mock.assert_called("research", 1);
    }

    #[tokio::test]
    async fn test_project_fixture_creation() {
        let fixture = TestProjectFixture::rust_project();

        assert!(fixture.path().join("Cargo.toml").exists());
        assert!(fixture.path().join("src/lib.rs").exists());
    }

    #[tokio::test]
    async fn test_multi_domain_fixture() {
        let fixture = TestProjectFixture::multi_domain();

        assert_eq!(fixture.domains.len(), 3);
        assert!(fixture.path().join("src/auth/mod.rs").exists());
        assert!(fixture.path().join("src/api/mod.rs").exists());
        assert!(fixture.path().join("src/db/mod.rs").exists());
    }

    #[tokio::test]
    async fn test_fixture_with_module_map() {
        let fixture = TestProjectFixture::multi_domain().with_module_map();

        assert!(fixture.has_module_map);
        assert!(fixture.path().join("module_map.json").exists());
    }
}

mod scenario_verification {
    use super::*;

    #[tokio::test]
    async fn test_verification_retry_flow() {
        // Setup: Project with intentional error
        let fixture = TestProjectBuilder::new()
            .language("rust")
            .file(
                "Cargo.toml",
                "[package]\nname = \"test\"\nversion = \"0.1.0\"",
            )
            .file_with_error("src/lib.rs", ErrorType::TypeMismatch)
            .build()
            .unwrap();

        assert!(fixture.path().join("src/lib.rs").exists());

        // Mock agent: First verification fails, then passes twice
        let mock = MockTaskAgentBuilder::new()
            .sequential_responses(
                "verifier",
                vec![
                    r#"{"status": "fail", "issues": [{"type": "type_mismatch"}]}"#,
                    r#"{"status": "pass", "issues": []}"#,
                    r#"{"status": "pass", "issues": []}"#,
                ],
            )
            .build();

        // Simulate verification rounds
        let v1 = mock.run_prompt("verifier round 1", fixture.path()).await;
        let v2 = mock.run_prompt("verifier round 2", fixture.path()).await;
        let v3 = mock.run_prompt("verifier round 3", fixture.path()).await;

        assert!(v1.contains("fail"));
        assert!(v2.contains("pass"));
        assert!(v3.contains("pass"));

        mock.assert_called("verifier", 3);
    }

    #[tokio::test]
    async fn test_convergent_verification_events() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_convergent_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Simulate verification rounds: fail, pass, pass
        store
            .append(DomainEvent::verification_round(
                "mission-1",
                1,
                false,
                1,
                100,
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::verification_round(
                "mission-1",
                2,
                true,
                0,
                100,
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::verification_round(
                "mission-1",
                3,
                true,
                0,
                100,
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        // Should have 2 consecutive clean rounds
        verifier.assert_convergent_verification(2);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod scenario_consensus {
    use super::*;

    #[tokio::test]
    async fn test_consensus_vote_events() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_consensus_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Simulate consensus round
        store
            .append(DomainEvent::consensus_round_started(
                "mission-1",
                1,
                "abc123",
                vec!["agent-1".into(), "agent-2".into()],
            ))
            .await
            .unwrap();

        store
            .append(DomainEvent::consensus_vote_received(
                "mission-1",
                1,
                "agent-1",
                VoteDecision::Approve,
                0.9,
            ))
            .await
            .unwrap();

        store
            .append(DomainEvent::consensus_vote_received(
                "mission-1",
                1,
                "agent-2",
                VoteDecision::Approve,
                0.85,
            ))
            .await
            .unwrap();

        store
            .append(DomainEvent::consensus_round_completed(
                "mission-1",
                1,
                RoundOutcome::Approved,
                0.95,
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        verifier.assert_sequence(&[
            EventPattern::ConsensusRoundStarted { round: 1 },
            EventPattern::ConsensusVote {
                agent: "agent-1".into(),
                decision: VoteDecision::Approve,
            },
            EventPattern::ConsensusVote {
                agent: "agent-2".into(),
                decision: VoteDecision::Approve,
            },
            EventPattern::ConsensusRoundCompleted { round: 1 },
        ]);

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_multi_domain_consensus() {
        let fixture = TestProjectFixture::multi_domain().with_module_map();

        // Mock: Different domains have different responses
        let mock = MockTaskAgentBuilder::new()
            .response(
                "consensus",
                ResponseScenario::conditional(
                    vec![
                        ("auth", r#"{"vote": "approve", "domain": "auth"}"#),
                        ("api", r#"{"vote": "conditional", "domain": "api"}"#),
                        ("db", r#"{"vote": "approve", "domain": "db"}"#),
                    ],
                    r#"{"vote": "abstain"}"#,
                ),
            )
            .build();

        // Each domain should get appropriate response
        let auth_vote = mock
            .run_prompt("consensus auth check", fixture.path())
            .await;
        let api_vote = mock.run_prompt("consensus api check", fixture.path()).await;
        let db_vote = mock.run_prompt("consensus db check", fixture.path()).await;

        assert!(auth_vote.contains("approve"));
        assert!(api_vote.contains("conditional"));
        assert!(db_vote.contains("approve"));
    }
}

mod scenario_escalation {
    use super::*;
    use claude_pilot::agent::multi::escalation::EscalationLevel;

    #[tokio::test]
    async fn test_escalation_events() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_escalation_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Simulate escalation
        store
            .append(DomainEvent::consensus_escalated(
                "mission-1",
                EscalationLevel::ArchitecturalMediation,
                "consensus timeout",
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        verifier.assert_escalation(EscalationLevel::ArchitecturalMediation);

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_no_escalation_on_quick_consensus() {
        let temp_dir = std::env::temp_dir().join(format!("test_no_esc_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Quick consensus without escalation
        store
            .append(DomainEvent::consensus_completed(
                "mission-1",
                1,
                2,
                "consensus_reached",
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        verifier.assert_no_escalation();

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod scenario_conflict {
    use super::*;
    use claude_pilot::agent::multi::consensus::ConflictSeverity;

    #[tokio::test]
    async fn test_conflict_detection_and_resolution() {
        let temp_dir = std::env::temp_dir().join(format!("test_conflict_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Conflict detected
        store
            .append(DomainEvent::consensus_conflict_detected(
                "mission-1",
                1,
                "conflict-1",
                vec!["domain-auth".into(), "domain-api".into()],
                ConflictSeverity::Moderate,
            ))
            .await
            .unwrap();

        // Conflict resolved
        store
            .append(DomainEvent::consensus_conflict_resolved(
                "mission-1",
                "conflict-1",
                "merge_proposals",
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        verifier.assert_conflicts_resolved();

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod scenario_dynamic_agent {
    use super::*;

    #[tokio::test]
    async fn test_dynamic_agent_spawn_event() {
        let temp_dir = std::env::temp_dir().join(format!("test_dynamic_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // Dynamic agent spawn
        store
            .append(DomainEvent::agent_spawned(
                "mission-1",
                "domain-plugin-0",
                "domain",
                Some("plugin"),
            ))
            .await
            .unwrap();

        // Agent participates
        store
            .append(DomainEvent::new(
                "mission-1",
                EventPayload::AgentTaskAssigned {
                    agent_id: "domain-plugin-0".into(),
                    task_id: "task-1".into(),
                    role: "domain".into(),
                },
            ))
            .await
            .unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        verifier.assert_agent_participated("domain-plugin-0");
        verifier.assert_sequence(&[
            EventPattern::AgentSpawned {
                role: "domain".into(),
            },
            EventPattern::TaskAssigned {
                agent: "domain-plugin-0".into(),
            },
        ]);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod scenario_messaging {
    use super::*;

    #[tokio::test]
    async fn test_message_bus_integration() {
        let temp_dir =
            std::env::temp_dir().join(format!("test_messaging_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        let bus = AgentMessageBus::default().with_event_store(Arc::clone(&store));

        // Subscribe agents
        let mut coder_rx = bus.subscribe("coder-0");
        let mut reviewer_rx = bus.subscribe("reviewer-0");

        // Coordinator sends task assignment
        let msg = AgentMessage::new(
            "coordinator",
            "coder-0",
            MessagePayload::Text {
                content: "implement feature".into(),
            },
        );
        bus.send(msg).await.unwrap();

        // Broadcast announcement
        let broadcast = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::Text {
                content: "mission started".into(),
            },
        );
        bus.send(broadcast).await.unwrap();

        // Verify coder received both
        let msg1 = coder_rx.recv().await.unwrap();
        let msg2 = coder_rx.recv().await.unwrap();
        assert!(msg1.is_some());
        assert!(msg2.is_some());

        // Verify reviewer only received broadcast
        let msg = reviewer_rx.recv().await.unwrap();
        assert!(msg.is_some());

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_consensus_vote_message() {
        let bus = AgentMessageBus::new(16);
        let mut coordinator_rx = bus.subscribe("coordinator");

        let vote = AgentMessage::consensus_vote(
            "reviewer-0",
            1,
            VoteDecision::Approve,
            "looks good".into(),
        );
        bus.try_send(vote).unwrap();

        let received = coordinator_rx.try_recv().unwrap();
        assert!(received.is_some());
        let msg = received.unwrap();
        assert_eq!(msg.from, "reviewer-0");
        assert_eq!(msg.message_type(), MessageType::ConsensusVote);
    }
}

mod scenario_context_composition {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_skill_selection_for_coder() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "coder".into(),
            category: RoleCategory::Core,
        };

        // Debug task should select Debug skill
        let task = AgentTask {
            id: "task-1".into(),
            description: "Debug the authentication issue".into(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(role.clone()),
        };

        let ctx = composer.compose(&role, &task, &[]);
        assert_eq!(ctx.active_skill, Some(SkillType::Debug));

        // Refactor task should select Refactor skill
        let task = AgentTask {
            id: "task-2".into(),
            description: "Refactor the user module".into(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(role.clone()),
        };

        let ctx = composer.compose(&role, &task, &[]);
        assert_eq!(ctx.active_skill, Some(SkillType::Refactor));
    }

    #[tokio::test]
    async fn test_reviewer_gets_code_review_skill() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "reviewer".into(),
            category: RoleCategory::Core,
        };

        let task = AgentTask {
            id: "task-1".into(),
            description: "Review the changes".into(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(role.clone()),
        };

        let ctx = composer.compose(&role, &task, &[]);
        assert_eq!(ctx.active_skill, Some(SkillType::CodeReview));
    }
}

mod scenario_agent_pool {
    use super::*;

    #[test]
    fn test_pool_creation_with_config() {
        let config = MultiAgentConfig::default();
        let pool = AgentPoolBuilder::new(config.clone()).build().unwrap();

        // Pool should be created (empty without agents added)
        assert_eq!(pool.total_agents(), 0);
    }

    #[test]
    fn test_pool_statistics() {
        let config = MultiAgentConfig::default();
        let pool = AgentPoolBuilder::new(config).build().unwrap();

        let stats = pool.statistics();
        assert_eq!(stats.agent_count, 0);
        assert_eq!(stats.total_load, 0);
    }
}

mod scenario_coordinator {
    use super::*;

    #[tokio::test]
    async fn test_coordinator_initialization() {
        let config = MultiAgentConfig::default();
        let pool = AgentPoolBuilder::new(config.clone()).build().unwrap();

        let coordinator = Coordinator::new(config, Arc::new(pool));
        assert!(!coordinator.is_shutdown());
    }

    #[tokio::test]
    async fn test_coordinator_with_event_store() {
        let temp_dir = std::env::temp_dir().join(format!("test_coord_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");

        let config = MultiAgentConfig::default();
        let pool = AgentPoolBuilder::new(config.clone()).build().unwrap();
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        let coordinator = Coordinator::new(config, Arc::new(pool)).with_event_store(store);

        assert!(!coordinator.is_shutdown());

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod integration {
    use super::*;

    #[tokio::test]
    async fn test_full_workflow_simulation() {
        // This test simulates a complete workflow without actual LLM calls

        // 1. Setup project fixture
        let fixture = TestProjectFixture::multi_domain().with_module_map();

        // 2. Setup mock agent
        let mock = MockTaskAgentBuilder::new()
            .static_response(
                "research",
                r#"{
                "affected_files": ["src/auth/mod.rs", "src/api/routes.rs"],
                "complexity": "moderate",
                "domains": ["auth", "api"]
            }"#,
            )
            .static_response(
                "planning",
                r#"{
                "tasks": [
                    {"id": "1", "description": "modify auth", "domain": "auth"},
                    {"id": "2", "description": "update api", "domain": "api"}
                ]
            }"#,
            )
            .static_response("consensus", r#"{"vote": "approve", "confidence": 0.9}"#)
            .static_response("coder", "// Implementation complete")
            .sequential_responses(
                "verifier",
                vec![
                    r#"{"status": "pass", "issues": []}"#,
                    r#"{"status": "pass", "issues": []}"#,
                ],
            )
            .build();

        // 3. Setup event store
        let temp_dir = std::env::temp_dir().join(format!("test_full_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        let db_path = temp_dir.join("events.db");
        let store = Arc::new(EventStore::new(&db_path).unwrap());

        // 4. Record simulated events
        let mission_id = "full-test-mission";

        // Research phase
        store
            .append(DomainEvent::agent_spawned(
                mission_id,
                "research-0",
                "research",
                None,
            ))
            .await
            .unwrap();
        let _ = mock.run_prompt("research", fixture.path()).await;
        store
            .append(DomainEvent::new(
                mission_id,
                EventPayload::AgentTaskCompleted {
                    agent_id: "research-0".into(),
                    task_id: "research-task".into(),
                    success: true,
                    duration_ms: 100,
                },
            ))
            .await
            .unwrap();

        // Consensus phase
        store
            .append(DomainEvent::consensus_round_started(
                mission_id,
                1,
                "abc123",
                vec!["domain-auth".into(), "domain-api".into()],
            ))
            .await
            .unwrap();

        let _ = mock.run_prompt("consensus", fixture.path()).await;
        store
            .append(DomainEvent::consensus_completed(
                mission_id,
                1,
                2,
                "consensus_reached",
            ))
            .await
            .unwrap();

        // Implementation phase
        store
            .append(DomainEvent::new(
                mission_id,
                EventPayload::AgentTaskAssigned {
                    agent_id: "coder-0".into(),
                    task_id: "task-1".into(),
                    role: "coder".into(),
                },
            ))
            .await
            .unwrap();

        let _ = mock.run_prompt("coder", fixture.path()).await;
        store
            .append(DomainEvent::new(
                mission_id,
                EventPayload::AgentTaskCompleted {
                    agent_id: "coder-0".into(),
                    task_id: "task-1".into(),
                    success: true,
                    duration_ms: 500,
                },
            ))
            .await
            .unwrap();

        // Verification phase (2 clean rounds)
        for round in 1..=2u32 {
            store
                .append(DomainEvent::verification_round(
                    mission_id, round, true, 0, 100,
                ))
                .await
                .unwrap();

            let _ = mock.run_prompt("verifier", fixture.path()).await;
        }

        // 5. Verify the workflow
        let events = store.query(mission_id, 0).await.unwrap();
        let verifier = EventVerifier::new(events);

        // Check event sequence
        verifier.assert_sequence(&[
            EventPattern::AgentSpawned {
                role: "research".into(),
            },
            EventPattern::ConsensusRoundStarted { round: 1 },
            EventPattern::ConsensusCompleted { rounds: 1 },
            EventPattern::TaskAssigned {
                agent: "coder-0".into(),
            },
            EventPattern::TaskCompleted { success: true },
            EventPattern::VerificationRound { round: 1 },
            EventPattern::VerificationRound { round: 2 },
        ]);

        // Check convergent verification
        verifier.assert_convergent_verification(2);

        // Check no escalation
        verifier.assert_no_escalation();

        // Check mock was called correctly
        mock.assert_called("research", 1);
        mock.assert_called("consensus", 1);
        mock.assert_called("coder", 1);
        mock.assert_called("verifier", 2);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
