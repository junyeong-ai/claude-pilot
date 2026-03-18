//! Tests for the hierarchy module.

use crate::agent::multi::hierarchy::*;
use crate::agent::multi::shared::{AgentId, TierLevel};
use crate::domain::Quorum;
use crate::config::ConsensusConfig;
use crate::agent::multi::scope::AgentScope;

fn test_config() -> ConsensusConfig {
        ConsensusConfig {
            flat_threshold: 5,
            hierarchical_threshold: 15,
            ..Default::default()
        }
    }

    #[test]
    fn test_direct_strategy_single_participant() {
        let selector = StrategySelector::new(test_config());
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());

        let scope = AgentScope::Module {
            workspace: "test".to_string(),
            module: "auth".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        assert!(matches!(strategy, ConsensusStrategy::Direct { .. }));
        assert_eq!(strategy.name(), "direct");
    }

    #[test]
    fn test_flat_strategy_few_participants() {
        let selector = StrategySelector::new(test_config());
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());
        participants.add_module_agent("db".to_string());
        participants.add_module_agent("api".to_string());

        let scope = AgentScope::Group {
            workspace: "test".to_string(),
            group: "core".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        assert!(matches!(strategy, ConsensusStrategy::Flat { .. }));
        assert_eq!(strategy.participant_count(), 3);
    }

    #[test]
    fn test_participant_set_basics() {
        let mut set = ParticipantSet::new();

        set.add_module_agent("auth".to_string());
        set.add_module_agent("db".to_string());
        set.add_group_coordinator("core".to_string(), "auth".to_string());

        assert_eq!(set.len(), 3);
        assert_eq!(set.distinct_groups(), 1);
        assert!(!set.spans_multiple_domains());
    }

    #[test]
    fn test_participant_set_dedup() {
        let mut set = ParticipantSet::new();

        set.add_module_agent("auth".to_string());
        set.add_module_agent("auth".to_string());

        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_tier_level_display() {
        assert_eq!(TierLevel::Module.as_str(), "module");
        assert_eq!(TierLevel::Group.as_str(), "group");
        assert_eq!(TierLevel::Domain.as_str(), "domain");
        assert_eq!(TierLevel::Workspace.as_str(), "workspace");
        assert_eq!(TierLevel::CrossWorkspace.as_str(), "cross_workspace");
    }

    #[test]
    fn test_multi_instance_agents_per_module() {
        let mut set = ParticipantSet::new();

        // Add multiple agents for the same module (e.g., planning-0, coder-0)
        set.add_module_agents(
            "auth".to_string(),
            vec![
                AgentId::new("planning-0"),
                AgentId::new("coder-0"),
                AgentId::new("verifier-0"),
            ],
        );

        set.add_module_agents(
            "db".to_string(),
            vec![AgentId::new("planning-1"), AgentId::new("coder-1")],
        );

        // Should have 5 total participants (3 + 2)
        assert_eq!(set.len(), 5);

        // But only 2 distinct modules
        assert_eq!(set.module_count(), 2);

        // All agents should be accessible
        let all = set.all();
        assert!(all.iter().any(|a| a.as_str() == "planning-0"));
        assert!(all.iter().any(|a| a.as_str() == "coder-1"));
    }

    #[test]
    fn test_enrich_from_pool_planning_only() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());
        base_set.add_module_agent("db".to_string());

        // Pool key format: "{module}:{role}" (e.g., "auth:planning")
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "auth:planning" => vec![AgentId::new("auth:planning-0")],
                "db:planning" => vec![AgentId::new("db:planning-0")],
                "auth:coder" => vec![AgentId::new("auth:coder-0")],
                "db:coder" => vec![AgentId::new("db:coder-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        assert_eq!(base_set.len(), 2);
        assert_eq!(enriched.len(), 2);
        assert_eq!(enriched.module_count(), 2);

        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:planning-0"));
        assert!(all.iter().any(|a| a.as_str() == "db:planning-0"));
        assert!(!all.iter().any(|a| a.as_str().contains("coder")));
    }

    #[test]
    fn test_enrich_from_pool_coder_role() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());
        base_set.add_module_agent("db".to_string());

        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "auth:coder" => vec![AgentId::new("auth:coder-0")],
                "db:coder" => vec![AgentId::new("db:coder-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("coder"));

        assert_eq!(enriched.len(), 2);
        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:coder-0"));
        assert!(all.iter().any(|a| a.as_str() == "db:coder-0"));
    }

    #[test]
    fn test_enrich_from_pool_fallback_to_global() {
        let mut base_set = ParticipantSet::new();
        base_set.add_module_agent("auth".to_string());

        // No module-scoped agents, but global planning agents exist
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "planning" => vec![
                    AgentId::new("auth:planning-0"),
                    AgentId::new("db:planning-0"),
                ],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        // Should find "auth:planning-0" via global fallback filtered by module
        assert_eq!(enriched.len(), 1);
        let all = enriched.all();
        assert!(all.iter().any(|a| a.as_str() == "auth:planning-0"));
    }

    #[test]
    fn test_enrich_qualified_modules() {
        let mut base_set = ParticipantSet::new();
        base_set.add_qualified_module_agents(
            "project-a::auth".to_string(),
            vec![AgentId::new("placeholder")],
        );

        // Qualified module pool key: "project-a:auth:planning"
        let mock_pool_lookup = |pool_key: &str| -> Vec<AgentId> {
            match pool_key {
                "project-a:auth:planning" => vec![AgentId::new("project-a:auth:planning-0")],
                _ => vec![],
            }
        };

        let enriched = base_set.enrich_from_pool(mock_pool_lookup, Some("planning"));

        assert_eq!(enriched.len(), 1);
        let all = enriched.all();
        assert!(
            all.iter()
                .any(|a| a.as_str() == "project-a:auth:planning-0")
        );
    }

    #[test]
    fn test_agents_by_role_selection() {
        let mut set = ParticipantSet::new();

        // Add mixed agents from different modules
        set.add_module_agents(
            "auth".to_string(),
            vec![
                AgentId::new("module-auth-planning-0"),
                AgentId::new("module-auth-coder-0"),
            ],
        );
        set.add_module_agents(
            "db".to_string(),
            vec![
                AgentId::new("module-db-planning-0"),
                AgentId::new("module-db-coder-0"),
            ],
        );

        // Select all planning agents across modules
        let planning = set.agents_by_role("planning");
        assert_eq!(planning.len(), 2);
        assert!(planning.iter().all(|a| a.as_str().contains("planning")));

        // Select all coder agents across modules
        let coders = set.agents_by_role("coder");
        assert_eq!(coders.len(), 2);
        assert!(coders.iter().all(|a| a.as_str().contains("coder")));
    }

    #[test]
    fn test_cross_workspace_qualified_modules() {
        let mut set = ParticipantSet::new();

        // Add agents from different workspaces (using :: separator)
        set.add_qualified_module_agents(
            "project-a::auth".to_string(),
            vec![AgentId::new("project-a::auth::planning-0")],
        );
        set.add_qualified_module_agents(
            "project-b::api".to_string(),
            vec![AgentId::new("project-b::api::planning-0")],
        );

        // Should span multiple workspaces
        assert!(set.spans_multiple_workspaces());

        // Total participants
        assert_eq!(set.len(), 2);

        // CrossWorkspace tier should include these
        let cross_ws_participants = set.participants_at_tier(TierLevel::CrossWorkspace);
        assert_eq!(cross_ws_participants.len(), 2);
    }

    #[test]
    fn test_consensus_units_with_multi_instance() {
        let mut set = ParticipantSet::new();

        set.add_module_agents_in_group(
            "auth".to_string(),
            "core".to_string(),
            vec![
                AgentId::new("auth-planning-0"),
                AgentId::new("auth-coder-0"),
            ],
        );

        let units = set.to_consensus_units(TierLevel::Module);

        // Should have 1 unit for the auth module
        assert_eq!(units.len(), 1);

        // The unit should contain both agents
        assert_eq!(units[0].participants.len(), 2);
    }

    #[test]
    fn test_per_group_module_tier() {
        let config = ConsensusConfig {
            flat_threshold: 3,
            hierarchical_threshold: 8,
            ..Default::default()
        };
        let selector = StrategySelector::new(config);

        let mut participants = ParticipantSet::new();
        // Add agents from multiple modules in different groups to trigger hierarchical
        participants.add_module_agent_in_group("auth".to_string(), "security".to_string());
        participants.add_module_agent_in_group("api".to_string(), "core".to_string());
        participants.add_module_agent_in_group("db".to_string(), "data".to_string());
        participants.add_module_agent_in_group("cache".to_string(), "data".to_string());
        participants.add_group_coordinator("security".to_string(), "auth".to_string());
        participants.add_group_coordinator("core".to_string(), "api".to_string());
        participants.add_group_coordinator("data".to_string(), "db".to_string());

        let scope = AgentScope::Workspace {
            workspace: "test".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        // Should be hierarchical since we have 3 groups
        if let ConsensusStrategy::Hierarchical { tiers } = strategy {
            // Find module tier
            let module_tier = tiers.iter().find(|t| t.level == TierLevel::Module);
            assert!(module_tier.is_some(), "Module tier should exist");

            let module_tier = module_tier.unwrap();
            // Per-group units: security(auth), core(api), data(db+cache) = 3 units
            assert_eq!(
                module_tier.units.len(),
                3,
                "Should have one unit per group"
            );

            // Find the data group unit which has 2 modules
            let data_unit = module_tier
                .units
                .iter()
                .find(|u| u.participants.len() == 2);
            assert!(
                data_unit.is_some(),
                "Data group unit should have 2 participants (db + cache)"
            );
            let data_unit = data_unit.unwrap();
            assert!(
                data_unit.cross_visibility,
                "Cross-visibility should be enabled"
            );

            // Total participants across all units should be 4
            let total: usize = module_tier.units.iter().map(|u| u.participants.len()).sum();
            assert_eq!(total, 4, "All 4 module agents distributed across groups");
        } else {
            panic!("Expected hierarchical strategy, got {:?}", strategy.name());
        }
    }

    #[test]
    fn test_consensus_unit_quorum() {
        let unit = ConsensusUnit::new(
            "test",
            vec![AgentId::new("a"), AgentId::new("b"), AgentId::new("c")],
        )
        .with_quorum(Quorum::Supermajority)
        .with_cross_visibility(true);

        assert_eq!(unit.quorum, Quorum::Supermajority);
        assert!(unit.cross_visibility);
        assert!(unit.is_collective());

        // Check quorum threshold
        assert_eq!(unit.quorum.threshold(3), 2); // 2/3 of 3 = 2
        assert!(unit.quorum.is_met(2, 3));
        assert!(!unit.quorum.is_met(1, 3));
    }

    #[test]
    fn test_domain_tier_includes_group_coordinators() {
        let mut set = ParticipantSet::new();

        // Add modules in groups belonging to a domain
        set.add_module_agent_in_group("auth".to_string(), "security".to_string());
        set.add_module_agent_in_group("crypto".to_string(), "security".to_string());
        set.add_module_agent_in_group("api".to_string(), "core".to_string());

        // Add group coordinators with domain assignment
        set.add_group_coordinator_in_domain(
            "security".to_string(),
            "auth".to_string(),
            Some("backend".to_string()),
        );
        set.add_group_coordinator_in_domain(
            "core".to_string(),
            "api".to_string(),
            Some("backend".to_string()),
        );

        // Add domain coordinator
        set.add_domain_coordinator("backend".to_string());

        // Get domain tier units
        let units = set.to_consensus_units(TierLevel::Domain);

        // Should have 1 domain unit
        assert_eq!(units.len(), 1, "Should have 1 domain unit");

        let domain_unit = &units[0];
        assert!(
            domain_unit.id.contains("backend"),
            "Unit ID should contain domain ID"
        );

        // Domain unit should include both group coordinators, not just the domain coordinator
        assert_eq!(
            domain_unit.participants.len(),
            2,
            "Domain unit should include both group coordinators belonging to the domain"
        );

        // Verify coordinators are present
        let participant_ids: Vec<_> = domain_unit
            .participants
            .iter()
            .map(|a| a.as_str())
            .collect();
        assert!(
            participant_ids.iter().any(|id| id.contains("security")),
            "Should include security group coordinator"
        );
        assert!(
            participant_ids.iter().any(|id| id.contains("core")),
            "Should include core group coordinator"
        );
    }
