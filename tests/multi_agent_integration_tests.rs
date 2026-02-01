//! Integration tests for multi-agent architecture.
//!
//! Tests the complete flow: context composition, messaging, and consensus.

use std::path::PathBuf;
use std::sync::Arc;

use claude_pilot::agent::multi::context::{ComposedContext, ContextComposer, PersonaLoader};
use claude_pilot::agent::multi::messaging::{
    AgentMessage, AgentMessageBus, MessagePayload, MessageType,
};
use claude_pilot::agent::multi::rules::{ResolvedRules, RuleRegistry};
use claude_pilot::agent::multi::skills::{SkillRegistry, SkillType};
use claude_pilot::agent::multi::{AgentRole, AgentTask, RoleCategory, TaskContext, TaskPriority};
use claude_pilot::state::{DomainEvent, EventPayload, EventStore};

mod context_composition {
    use super::*;

    #[test]
    fn test_empty_context_composition() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "coder".into(),
            category: RoleCategory::Core,
        };
        let task = AgentTask {
            id: "task-1".into(),
            description: "Implement feature".into(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(role.clone()),
        };

        let ctx = composer.compose(&role, &task, &[]);
        assert!(ctx.active_skill.is_some());
        assert_eq!(ctx.active_skill, Some(SkillType::Implement));
    }

    #[test]
    fn test_skill_selection_by_role() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let cases = vec![
            ("reviewer", SkillType::CodeReview),
            ("architect", SkillType::Plan),
            ("planning", SkillType::Plan),
        ];

        for (role_id, expected_skill) in cases {
            let role = AgentRole {
                id: role_id.into(),
                category: RoleCategory::Core,
            };
            let task = AgentTask {
                id: "task-1".into(),
                description: "Do something".into(),
                context: TaskContext::default(),
                priority: TaskPriority::Normal,
                role: Some(role.clone()),
            };

            let ctx = composer.compose(&role, &task, &[]);
            assert_eq!(
                ctx.active_skill,
                Some(expected_skill),
                "Role {} should select {:?}",
                role_id,
                expected_skill
            );
        }
    }

    #[test]
    fn test_skill_selection_by_task_description() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let coder_role = AgentRole {
            id: "coder".into(),
            category: RoleCategory::Core,
        };

        let cases = vec![
            ("Debug the login issue", SkillType::Debug),
            ("Fix bug in authentication", SkillType::Debug),
            ("Refactor the auth module", SkillType::Refactor),
            ("Restructure the codebase", SkillType::Refactor),
            ("Add new feature", SkillType::Implement),
        ];

        for (description, expected_skill) in cases {
            let task = AgentTask {
                id: "task-1".into(),
                description: description.into(),
                context: TaskContext::default(),
                priority: TaskPriority::Normal,
                role: Some(coder_role.clone()),
            };

            let ctx = composer.compose(&coder_role, &task, &[]);
            assert_eq!(
                ctx.active_skill,
                Some(expected_skill),
                "Task '{}' should select {:?}",
                description,
                expected_skill
            );
        }
    }

    #[test]
    fn test_composed_context_contains_skill_methodology() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let ctx = composer.compose_for_skill(SkillType::CodeReview, &[], "review code");

        assert!(ctx.system_prompt.contains("Code Review"));
        assert_eq!(ctx.active_skill, Some(SkillType::CodeReview));
    }

    #[test]
    fn test_rule_count_in_context() {
        let ctx = ComposedContext {
            system_prompt: "test".into(),
            injected_rules: ResolvedRules::new(),
            active_skill: None,
            persona_name: None,
        };

        assert_eq!(ctx.rule_count(), 0);
    }
}

mod messaging {
    use super::*;

    #[tokio::test]
    async fn test_message_bus_send_receive() {
        let bus = AgentMessageBus::new(16);
        let mut receiver = bus.subscribe("agent-1");

        let msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::Text {
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
            MessagePayload::Text {
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
            MessagePayload::Text {
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
        let mut filtered = bus.subscribe_filtered("agent-1", vec![MessageType::ConsensusVote]);

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
                decision: claude_pilot::state::VoteDecision::Approve,
                rationale: "looks good".into(),
            },
        ))
        .unwrap();

        let received = filtered.recv().await.unwrap();
        assert!(received.is_some());
        assert_eq!(received.unwrap().message_type(), MessageType::ConsensusVote);
    }

    #[test]
    fn test_consensus_vote_message() {
        let msg = AgentMessage::consensus_vote(
            "reviewer-0",
            1,
            claude_pilot::state::VoteDecision::Approve,
            "looks good".into(),
        );

        assert_eq!(msg.from, "reviewer-0");
        assert_eq!(msg.to, "coordinator");
        assert_eq!(msg.message_type(), MessageType::ConsensusVote);
    }
}

mod event_sourcing {
    use super::*;

    #[tokio::test]
    async fn test_agent_events_recording() {
        let temp_dir = std::env::temp_dir().join(format!("test_events_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        let event = DomainEvent::new(
            "mission-1",
            EventPayload::AgentTaskAssigned {
                agent_id: "coder-0".into(),
                task_id: "task-1".into(),
                role: "coder".into(),
            },
        );
        store.append(event).await.unwrap();

        let event = DomainEvent::new(
            "mission-1",
            EventPayload::AgentTaskCompleted {
                agent_id: "coder-0".into(),
                task_id: "task-1".into(),
                success: true,
                duration_ms: 1500,
            },
        );
        store.append(event).await.unwrap();

        let events = store.query("mission-1", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        assert!(matches!(
            &events[0].payload,
            EventPayload::AgentTaskAssigned { agent_id, .. } if agent_id == "coder-0"
        ));
        assert!(matches!(
            &events[1].payload,
            EventPayload::AgentTaskCompleted { success: true, .. }
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
                message_type: "task_assignment".into(),
                correlation_id: "corr-123".into(),
            },
        );
        store.append(event).await.unwrap();

        let event = DomainEvent::new(
            "corr-123",
            EventPayload::AgentMessageReceived {
                from_agent: "coordinator".into(),
                to_agent: "coder-0".into(),
                message_type: "task_assignment".into(),
                correlation_id: "corr-123".into(),
            },
        );
        store.append(event).await.unwrap();

        let events = store.query("corr-123", 0).await.unwrap();
        assert_eq!(events.len(), 2);

        std::fs::remove_dir_all(&temp_dir).ok();
    }
}

mod skills_registry {
    use super::*;

    #[test]
    fn test_all_skills_available() {
        let registry = SkillRegistry::default();

        let skill_types = [
            SkillType::CodeReview,
            SkillType::Implement,
            SkillType::Plan,
            SkillType::Debug,
            SkillType::Refactor,
        ];

        for skill_type in skill_types {
            let skill = registry.get(skill_type);
            assert!(
                !skill.methodology.is_empty(),
                "{:?} should have methodology",
                skill_type
            );
        }
    }

    #[test]
    fn test_skill_type_from_name() {
        assert_eq!(
            SkillType::from_name("code-review"),
            Some(SkillType::CodeReview)
        );
        assert_eq!(
            SkillType::from_name("implement"),
            Some(SkillType::Implement)
        );
        assert_eq!(SkillType::from_name("plan"), Some(SkillType::Plan));
        assert_eq!(SkillType::from_name("debug"), Some(SkillType::Debug));
        assert_eq!(SkillType::from_name("refactor"), Some(SkillType::Refactor));
        assert_eq!(SkillType::from_name("unknown"), None);
    }
}

mod architect_agent {
    use super::*;

    #[test]
    fn test_architect_skill_selection() {
        let rules = RuleRegistry::new(PathBuf::new());
        let skills = SkillRegistry::default();
        let personas = PersonaLoader::default();
        let composer = ContextComposer::new(&rules, &skills, &personas);

        let role = AgentRole {
            id: "architect".into(),
            category: RoleCategory::Advisor,
        };
        let task = AgentTask {
            id: "task-1".into(),
            description: "Review design decisions".into(),
            context: TaskContext::default(),
            priority: TaskPriority::Normal,
            role: Some(role.clone()),
        };

        let ctx = composer.compose(&role, &task, &[]);
        assert_eq!(ctx.active_skill, Some(SkillType::Plan));
    }

    #[test]
    fn test_architect_role_category() {
        let role = AgentRole {
            id: "architect".into(),
            category: RoleCategory::Advisor,
        };
        assert!(role.is_planning_relevant());
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
                "architect",
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
                if agent_id == "architect-0" && role == "architect"
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
    async fn test_context_injection_events() {
        let temp_dir = std::env::temp_dir().join(format!("test_context_{}", std::process::id()));
        let store = Arc::new(EventStore::new(&temp_dir).unwrap());

        store
            .append(DomainEvent::rules_injected(
                "m-1",
                "coder-0",
                "t-1",
                3,
                vec!["tech".into(), "domain".into()],
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::skill_activated(
                "m-1",
                "coder-0",
                "t-1",
                "implement",
            ))
            .await
            .unwrap();
        store
            .append(DomainEvent::persona_loaded(
                "m-1",
                "reviewer-0",
                "code-reviewer",
            ))
            .await
            .unwrap();

        let events = store.query("m-1", 0).await.unwrap();
        assert_eq!(events.len(), 3);

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
                "approve",
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
