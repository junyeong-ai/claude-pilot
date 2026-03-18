//! Integration tests for multi-agent architecture.
//!
//! Tests messaging, consensus, and module context integration.

use std::sync::Arc;

use claude_pilot::agent::multi::messaging::{
    AgentMessage, AgentMessageBus, AgentMessageType, MessagePayload,
};
use claude_pilot::domain::VoteDecision;
use claude_pilot::state::{DomainEvent, EventPayload, EventStore};

mod messaging {
    use super::*;

    #[tokio::test]
    async fn test_message_bus_send_receive() {
        let bus = AgentMessageBus::new(16);
        let mut receiver = bus.subscribe("agent-1");

        let msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::Broadcast {
                content: "hello".into(),
            },
        );

        bus.try_send(msg).unwrap();

        let received = receiver.try_recv().unwrap();
        assert!(received.is_some());
        assert_eq!(received.unwrap().from, "sender");
    }

    #[tokio::test]
    async fn test_message_bus_broadcast() {
        let bus = AgentMessageBus::new(16);
        let mut receiver1 = bus.subscribe("agent-1");
        let mut receiver2 = bus.subscribe("agent-2");

        let msg = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::Broadcast {
                content: "announcement".into(),
            },
        );

        bus.try_send(msg).unwrap();

        assert!(receiver1.try_recv().unwrap().is_some());
        assert!(receiver2.try_recv().unwrap().is_some());
    }

    #[tokio::test]
    async fn test_message_bus_with_event_store() {
        let temp_dir = std::env::temp_dir().join(format!("test_store_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());
        let bus = AgentMessageBus::default().with_event_store(Arc::clone(&store));
        let mut receiver = bus.subscribe("agent-1");

        let msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::Broadcast {
                content: "result".into(),
            },
        );
        let correlation_id = msg.correlation_id.clone();

        bus.send(msg).await.unwrap();

        let received = receiver.recv().await.unwrap();
        assert!(received.is_some());

        let events = store.query(&correlation_id, 0).await.unwrap();
        assert!(
            events.iter().any(|e| matches!(
                &e.payload,
                EventPayload::AgentMessageSent { from_agent, .. } if from_agent == "sender"
            )),
            "Should have AgentMessageSent event"
        );

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_filtered_receiver() {
        let bus = AgentMessageBus::new(16);
        let mut filtered = bus.subscribe_filtered("agent-1", vec![AgentMessageType::ConsensusVote]);

        bus.try_send(AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::TaskAssignment {
                task: claude_pilot::agent::multi::AgentTask::new("task-1", "test task"),
            },
        ))
        .unwrap();

        bus.try_send(AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::ConsensusVote {
                round: 1,
                decision: claude_pilot::domain::VoteDecision::Approve,
                rationale: "looks good".into(),
            },
        ))
        .unwrap();

        let received = filtered.recv().await.unwrap();
        assert!(received.is_some());
        assert_eq!(received.unwrap().message_type(), AgentMessageType::ConsensusVote);
    }

    #[test]
    fn test_consensus_vote_message() {
        let msg = AgentMessage::consensus_vote(
            "reviewer-0",
            "group-coordinator-backend",
            1,
            claude_pilot::domain::VoteDecision::Approve,
            "looks good".into(),
        );

        assert_eq!(msg.from, "reviewer-0");
        assert_eq!(msg.to, "group-coordinator-backend");
        assert_eq!(msg.message_type(), AgentMessageType::ConsensusVote);
    }
}

mod event_sourcing {
    use super::*;

    #[tokio::test]
    async fn test_agent_events_recording() {
        let temp_dir = std::env::temp_dir().join(format!("test_events_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        let event = DomainEvent::task_started("mission-1", "task-1", "Implement auth module");
        store.append(event).await.unwrap();

        let event = DomainEvent::task_completed("mission-1", "task-1", vec!["src/auth.rs".into()], 1500);
        store.append(event).await.unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        assert!(matches!(
            &events[0].payload,
            EventPayload::TaskStarted { task_id, .. } if task_id == "task-1"
        ));
        assert!(matches!(
            &events[1].payload,
            EventPayload::TaskCompleted { task_id, .. } if task_id == "task-1"
        ));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_message_events_recording() {
        let temp_dir = std::env::temp_dir().join(format!("test_msgs_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        let event = DomainEvent::new(
            "corr-123",
            EventPayload::AgentMessageSent {
                from_agent: "coordinator".into(),
                to_agent: "coder-0".into(),
                message_type: claude_pilot::agent::multi::AgentMessageType::TaskAssignment,
                correlation_id: "corr-123".into(),
            },
        );
        store.append(event).await.unwrap();

        let event = DomainEvent::new(
            "corr-123",
            EventPayload::AgentMessageReceived {
                from_agent: "coordinator".into(),
                to_agent: "coder-0".into(),
                message_type: claude_pilot::agent::multi::AgentMessageType::TaskAssignment,
                correlation_id: "corr-123".into(),
            },
        );
        store.append(event).await.unwrap();

        let events = store.query("corr-123", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod new_event_payloads {
    use super::*;

    #[tokio::test]
    async fn test_agent_lifecycle_events() {
        let temp_dir = std::env::temp_dir().join(format!("test_lifecycle_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        store
            .append(DomainEvent::agent_spawned(
                "m-1",
                "architect-0",
                claude_pilot::agent::multi::identity::RoleType::Architect,
                None,
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::agent_terminated(
                "m-1",
                "architect-0",
                "shutdown",
                5,
            ))
            .await
            .unwrap();

        let events = store.query("m-1", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        assert!(matches!(
            &events[0].payload,
            EventPayload::AgentSpawned { agent_id, role, .. }
                if agent_id == "architect-0" && *role == claude_pilot::agent::multi::identity::RoleType::Architect
        ));
        assert!(matches!(
            &events[1].payload,
            EventPayload::AgentTerminated {
                tasks_completed: 5,
                ..
            }
        ));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_manifest_context_injection_event() {
        let temp_dir = std::env::temp_dir().join(format!("test_context_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        store
            .append(DomainEvent::manifest_context_injected(
                "m-1",
                "coder-0",
                "t-1",
                vec!["auth".into(), "api".into()],
            ))
            .await
            .unwrap();

        let events = store.query("m-1", 0).await.unwrap();
        assert_eq!(events.len(), 1);

        assert!(matches!(
            &events[0].payload,
            EventPayload::ManifestContextInjected { agent_id, task_id, module_ids }
                if agent_id == "coder-0" && task_id == "t-1" && module_ids.len() == 2
        ));

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[tokio::test]
    async fn test_consensus_enhanced_events() {
        let temp_dir = std::env::temp_dir().join(format!("test_consensus_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        store
            .append(DomainEvent::consensus_proposal_submitted(
                "m-1",
                1,
                "architect-0",
                "abc123",
                vec!["auth".into(), "api".into()],
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::consensus_module_vote(
                "m-1",
                1,
                "domain-auth-0",
                "auth",
                VoteDecision::Approve,
                0.85,
            ))
            .await
            .unwrap();

        let events = store.query("m-1", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        assert!(matches!(
            &events[0].payload,
            EventPayload::ConsensusProposalSubmitted { round: 1, proposer_agent, .. }
                if proposer_agent == "architect-0"
        ));

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod module_context_integration {
    use super::*;
    use claude_pilot::agent::TaskAgent;
    use claude_pilot::agent::multi::ModuleAgent;
    use claude_pilot::agent::multi::SpecializedAgent;
    use claude_pilot::agent::multi::context::ModuleContextBuilder;
    use modmap::{
        Convention, EvidenceLocation, IssueCategory, IssueSeverity, KnownIssue, Module,
        ModuleContext as ManifestModuleContext, ModuleDependency, ModuleMetrics,
    };

    fn create_test_module() -> Module {
        Module {
            id: "auth".into(),
            name: "Authentication".into(),
            paths: vec!["src/auth/".into()],
            key_files: vec!["src/auth/mod.rs".into()],
            dependencies: vec![ModuleDependency::runtime("db")],
            dependents: vec!["api".into()],
            responsibility: "Handle authentication and authorization".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.85, 0.9, 0.7),
            conventions: vec![Convention::new("error-types", "Use Result types")],
            known_issues: vec![KnownIssue::new(
                "token-refresh",
                "Token refresh may fail under load",
                IssueSeverity::Medium,
                IssueCategory::Performance,
            )],
            evidence: vec![EvidenceLocation::new_range("src/auth/mod.rs", 1, 50)],
        }
    }

    #[test]
    fn test_module_agent_with_manifest_context() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let manifest_ctx = ManifestModuleContext::new()
            .with_rules(vec!["rules/modules/auth.md".into()])
            .with_conventions(vec!["Use bcrypt for password hashing".into()]);

        let agent = ModuleAgent::with_manifest_context(module, manifest_ctx, task_agent);
        let prompt = agent.system_prompt();

        // Module section should be present
        assert!(prompt.contains("# Module: Authentication"));
        assert!(prompt.contains("Handle authentication"));

        // Manifest context section should be present
        assert!(prompt.contains("# Module Context (Manifest)"));
        assert!(prompt.contains("rules/modules/auth.md"));
        assert!(prompt.contains("Use bcrypt for password hashing"));
    }

    #[test]
    fn test_module_context_builder_layering() {
        let module = create_test_module();
        let manifest_ctx = ManifestModuleContext::new()
            .with_rules(vec!["rules/auth.md".into()])
            .with_conventions(vec!["Custom convention".into()]);

        let builder = ModuleContextBuilder::new(&module).with_manifest_context(&manifest_ctx);

        let prompt = builder.build_system_prompt();

        // Layer 1: Module base context
        assert!(prompt.contains("# Module: Authentication"));
        assert!(prompt.contains("## Responsibility"));

        // Layer 2: Manifest context
        assert!(prompt.contains("# Module Context (Manifest)"));
        assert!(prompt.contains("rules/auth.md"));
        assert!(prompt.contains("Custom convention"));

        // Check ordering (Module section should come before Manifest section)
        let module_pos = prompt.find("# Module: Authentication").unwrap();
        let manifest_pos = prompt.find("# Module Context (Manifest)").unwrap();
        assert!(
            module_pos < manifest_pos,
            "Module section should come before manifest context"
        );
    }

    #[test]
    fn test_empty_manifest_context_not_included() {
        let module = create_test_module();
        let manifest_ctx = ManifestModuleContext::new();

        let builder = ModuleContextBuilder::new(&module).with_manifest_context(&manifest_ctx);

        let prompt = builder.build_system_prompt();

        // Module section should be present
        assert!(prompt.contains("# Module: Authentication"));

        // Empty manifest context should NOT be included
        assert!(!prompt.contains("# Module Context (Manifest)"));
    }
}
