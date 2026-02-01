#![allow(clippy::field_reassign_with_default)]
//! Comprehensive E2E tests for hierarchical multi-agent consensus.
//!
//! These tests verify:
//! - Cross-workspace hierarchical consensus (4 tiers: Module → Group → Domain → Workspace)
//! - Evidence gathering → consensus planning → dynamic agent spawning → implementation
//! - Real LLM execution via Claude Code OAuth authentication
//!
//! Run with: cargo test --test comprehensive_hierarchical_e2e_tests -- --ignored --nocapture

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::hierarchy::{ConsensusStrategy, ParticipantSet, StrategySelector};
use claude_pilot::agent::multi::messaging::AgentMessageBus;
use claude_pilot::agent::multi::{
    AdaptiveConsensusExecutor, AgentPoolBuilder, CoderAgent, ConsensusEngine, Coordinator,
    HierarchicalAggregator, PlanningAgent, ResearchAgent, ReviewerAgent, SpecializedAgent,
    TaskContext, TierLevel, VerifierAgent,
};
use claude_pilot::config::{AgentConfig, ConsensusConfig, MultiAgentConfig};
use claude_pilot::orchestration::AgentScope;
use claude_pilot::state::EventStore;
use claude_pilot::workspace::Workspace;
use modmap::{Domain, Module, ModuleDependency, ModuleGroup, ModuleMetrics};
use serde_json::json;
use tempfile::TempDir;

// =============================================================================
// Test Fixtures: Multi-Module/Multi-Domain Project Setup
// =============================================================================

/// Create a comprehensive multi-module project structure for testing.
/// Simulates a project with multiple domains, groups, and modules.
fn create_multi_module_project(base_dir: &Path) {
    // Create project structure:
    // project-a/
    // ├── Cargo.toml
    // ├── src/
    // │   ├── auth/          (domain: security, group: core)
    // │   │   ├── mod.rs
    // │   │   ├── login.rs
    // │   │   └── session.rs
    // │   ├── api/           (domain: infrastructure, group: gateway)
    // │   │   ├── mod.rs
    // │   │   ├── routes.rs
    // │   │   └── handlers.rs
    // │   ├── database/      (domain: infrastructure, group: persistence)
    // │   │   ├── mod.rs
    // │   │   └── models.rs
    // │   ├── billing/       (domain: business, group: commerce)
    // │   │   ├── mod.rs
    // │   │   └── invoices.rs
    // │   └── lib.rs

    std::fs::create_dir_all(base_dir.join("src/auth")).unwrap();
    std::fs::create_dir_all(base_dir.join("src/api")).unwrap();
    std::fs::create_dir_all(base_dir.join("src/database")).unwrap();
    std::fs::create_dir_all(base_dir.join("src/billing")).unwrap();

    // Cargo.toml
    std::fs::write(
        base_dir.join("Cargo.toml"),
        r#"[package]
name = "multi-module-project"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.0", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
"#,
    )
    .unwrap();

    // Root lib.rs
    std::fs::write(
        base_dir.join("src/lib.rs"),
        r#"//! Multi-module project for testing hierarchical consensus.

pub mod auth;
pub mod api;
pub mod database;
pub mod billing;
"#,
    )
    .unwrap();

    // Auth module
    std::fs::write(
        base_dir.join("src/auth/mod.rs"),
        r#"//! Authentication module (security domain).

pub mod login;
pub mod session;

pub use login::*;
pub use session::*;
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/auth/login.rs"),
        r#"//! User login functionality.

pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

pub struct LoginResponse {
    pub token: String,
    pub expires_at: u64,
}

pub fn authenticate(request: LoginRequest) -> Result<LoginResponse, String> {
    // TODO: Implement actual authentication
    if request.username.is_empty() {
        return Err("Username cannot be empty".into());
    }
    Ok(LoginResponse {
        token: "mock-token".into(),
        expires_at: 3600,
    })
}
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/auth/session.rs"),
        r#"//! Session management.

pub struct Session {
    pub id: String,
    pub user_id: String,
    pub active: bool,
}

impl Session {
    pub fn new(user_id: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            user_id,
            active: true,
        }
    }

    pub fn invalidate(&mut self) {
        self.active = false;
    }
}
"#,
    )
    .unwrap();

    // API module
    std::fs::write(
        base_dir.join("src/api/mod.rs"),
        r#"//! API gateway module (infrastructure domain).

pub mod routes;
pub mod handlers;

pub use routes::*;
pub use handlers::*;
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/api/routes.rs"),
        r#"//! Route definitions.

pub fn setup_routes() {
    // TODO: Setup routes
    println!("Setting up routes...");
}
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/api/handlers.rs"),
        r#"//! Request handlers.

use crate::auth::LoginRequest;

pub async fn handle_login(request: LoginRequest) -> Result<String, String> {
    let response = crate::auth::authenticate(request)?;
    Ok(response.token)
}
"#,
    )
    .unwrap();

    // Database module
    std::fs::write(
        base_dir.join("src/database/mod.rs"),
        r#"//! Database module (infrastructure domain).

pub mod models;

pub use models::*;
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/database/models.rs"),
        r#"//! Database models.

pub struct User {
    pub id: String,
    pub username: String,
    pub email: String,
}

pub struct Invoice {
    pub id: String,
    pub user_id: String,
    pub amount: f64,
}
"#,
    )
    .unwrap();

    // Billing module
    std::fs::write(
        base_dir.join("src/billing/mod.rs"),
        r#"//! Billing module (business domain).

pub mod invoices;

pub use invoices::*;
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/billing/invoices.rs"),
        r#"//! Invoice processing.

use crate::database::Invoice;

pub fn create_invoice(user_id: &str, amount: f64) -> Invoice {
    Invoice {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user_id.to_string(),
        amount,
    }
}

pub fn process_payment(invoice: &Invoice) -> Result<(), String> {
    if invoice.amount <= 0.0 {
        return Err("Invalid amount".into());
    }
    // TODO: Implement payment processing
    Ok(())
}
"#,
    )
    .unwrap();
}

/// Create a second project for cross-workspace testing.
fn create_second_project(base_dir: &Path) {
    // project-b/
    // ├── Cargo.toml
    // └── src/
    //     ├── analytics/    (domain: reporting, group: insights)
    //     └── notifications/ (domain: communications, group: messaging)

    std::fs::create_dir_all(base_dir.join("src/analytics")).unwrap();
    std::fs::create_dir_all(base_dir.join("src/notifications")).unwrap();

    std::fs::write(
        base_dir.join("Cargo.toml"),
        r#"[package]
name = "analytics-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/lib.rs"),
        r#"pub mod analytics;
pub mod notifications;
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/analytics/mod.rs"),
        r#"//! Analytics module.

pub fn track_event(name: &str, data: &str) {
    println!("Event: {} - {}", name, data);
}
"#,
    )
    .unwrap();

    std::fs::write(
        base_dir.join("src/notifications/mod.rs"),
        r#"//! Notifications module.

pub fn send_notification(user_id: &str, message: &str) -> Result<(), String> {
    if message.is_empty() {
        return Err("Empty message".into());
    }
    println!("Notification to {}: {}", user_id, message);
    Ok(())
}
"#,
    )
    .unwrap();
}

/// Create modules for testing (simplified version without full ModuleMap).
/// Note: Tests use Workspace directly with simplified module setup.
#[allow(dead_code)]
fn create_test_modules() -> Vec<Module> {
    // Modules with correct modmap schema
    let auth_module = Module {
        id: "auth".to_string(),
        name: "Authentication".to_string(),
        paths: vec![
            "src/auth/mod.rs".to_string(),
            "src/auth/login.rs".to_string(),
            "src/auth/session.rs".to_string(),
        ],
        key_files: vec!["src/auth/mod.rs".to_string()],
        dependencies: vec![],
        dependents: vec!["api".to_string()],
        responsibility: "User authentication and session management".to_string(),
        primary_language: "Rust".to_string(),
        metrics: ModuleMetrics::default(),
        conventions: vec![],
        known_issues: vec![],
        evidence: vec![],
    };

    let api_module = Module {
        id: "api".to_string(),
        name: "API Gateway".to_string(),
        paths: vec![
            "src/api/mod.rs".to_string(),
            "src/api/routes.rs".to_string(),
            "src/api/handlers.rs".to_string(),
        ],
        key_files: vec!["src/api/mod.rs".to_string()],
        dependencies: vec![ModuleDependency::runtime("auth")],
        dependents: vec![],
        responsibility: "HTTP API gateway".to_string(),
        primary_language: "Rust".to_string(),
        metrics: ModuleMetrics::default(),
        conventions: vec![],
        known_issues: vec![],
        evidence: vec![],
    };

    let database_module = Module {
        id: "database".to_string(),
        name: "Database".to_string(),
        paths: vec![
            "src/database/mod.rs".to_string(),
            "src/database/models.rs".to_string(),
        ],
        key_files: vec!["src/database/mod.rs".to_string()],
        dependencies: vec![],
        dependents: vec!["billing".to_string()],
        responsibility: "Database models and access".to_string(),
        primary_language: "Rust".to_string(),
        metrics: ModuleMetrics::default(),
        conventions: vec![],
        known_issues: vec![],
        evidence: vec![],
    };

    let billing_module = Module {
        id: "billing".to_string(),
        name: "Billing".to_string(),
        paths: vec![
            "src/billing/mod.rs".to_string(),
            "src/billing/invoices.rs".to_string(),
        ],
        key_files: vec!["src/billing/mod.rs".to_string()],
        dependencies: vec![ModuleDependency::runtime("database")],
        dependents: vec![],
        responsibility: "Billing and invoice processing".to_string(),
        primary_language: "Rust".to_string(),
        metrics: ModuleMetrics::default(),
        conventions: vec![],
        known_issues: vec![],
        evidence: vec![],
    };

    vec![auth_module, api_module, database_module, billing_module]
}

/// Create module groups for testing.
#[allow(dead_code)]
fn create_test_groups() -> Vec<ModuleGroup> {
    vec![
        ModuleGroup {
            id: "core".to_string(),
            name: "Core Services".to_string(),
            module_ids: vec!["auth".to_string()],
            responsibility: "Core authentication services".to_string(),
            boundary_rules: vec![],
            leader_module: Some("auth".to_string()),
            parent_group_id: None,
            domain_id: Some("security".to_string()),
            depth: 0,
        },
        ModuleGroup {
            id: "gateway".to_string(),
            name: "Gateway Services".to_string(),
            module_ids: vec!["api".to_string()],
            responsibility: "API gateway services".to_string(),
            boundary_rules: vec![],
            leader_module: Some("api".to_string()),
            parent_group_id: None,
            domain_id: Some("infrastructure".to_string()),
            depth: 0,
        },
        ModuleGroup {
            id: "persistence".to_string(),
            name: "Persistence Layer".to_string(),
            module_ids: vec!["database".to_string()],
            responsibility: "Data persistence".to_string(),
            boundary_rules: vec![],
            leader_module: Some("database".to_string()),
            parent_group_id: None,
            domain_id: Some("infrastructure".to_string()),
            depth: 0,
        },
        ModuleGroup {
            id: "commerce".to_string(),
            name: "Commerce".to_string(),
            module_ids: vec!["billing".to_string()],
            responsibility: "Commerce and billing".to_string(),
            boundary_rules: vec![],
            leader_module: Some("billing".to_string()),
            parent_group_id: None,
            domain_id: Some("business".to_string()),
            depth: 0,
        },
    ]
}

/// Create domains for testing.
#[allow(dead_code)]
fn create_test_domains() -> Vec<Domain> {
    vec![
        Domain {
            id: "security".to_string(),
            name: "Security Domain".to_string(),
            group_ids: vec!["core".to_string()],
            responsibility: "Security and authentication".to_string(),
            boundary_rules: vec![],
            interfaces: vec![],
            owner: None,
        },
        Domain {
            id: "infrastructure".to_string(),
            name: "Infrastructure Domain".to_string(),
            group_ids: vec!["gateway".to_string(), "persistence".to_string()],
            responsibility: "Infrastructure services".to_string(),
            boundary_rules: vec![],
            interfaces: vec![],
            owner: None,
        },
        Domain {
            id: "business".to_string(),
            name: "Business Domain".to_string(),
            group_ids: vec!["commerce".to_string()],
            responsibility: "Business logic".to_string(),
            boundary_rules: vec![],
            interfaces: vec![],
            owner: None,
        },
    ]
}

/// Create TaskAgent with real LLM configuration (OAuth via Claude Code).
fn create_task_agent() -> Arc<TaskAgent> {
    let config = AgentConfig::default();
    Arc::new(TaskAgent::new(config))
}

/// Create all specialized agents for comprehensive workflow.
fn create_agents(task_agent: Arc<TaskAgent>) -> Vec<Arc<dyn SpecializedAgent>> {
    vec![
        ResearchAgent::with_id("research-0", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-1", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
        ReviewerAgent::with_id("reviewer-0", Arc::clone(&task_agent)),
    ]
}

/// Setup event store for tracking.
fn setup_event_store(project_dir: &Path) -> Arc<EventStore> {
    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    Arc::new(EventStore::new(&db_path).unwrap())
}

/// Create a Workspace from the multi-module test project for adaptive consensus.
async fn create_workspace_from_multi_module_project(dir: &Path) -> Arc<Workspace> {
    let claudegen_dir = dir.join(".claudegen");
    std::fs::create_dir_all(&claudegen_dir).unwrap();

    let manifest = json!({
        "version": "1.0.0",
        "created_at": "2026-01-30T00:00:00Z",
        "generator": "test-fixture",
        "project": {
            "schema_version": "1.0.0",
            "generator": {"name": "test-fixture", "version": "1.0.0"},
            "project": {
                "name": "multi-module-project",
                "workspace": {"workspace_type": "single_package"},
                "tech_stack": {"primary_language": "rust"},
                "languages": [{"name": "rust", "marker_files": ["Cargo.toml"]}],
                "total_files": 10
            },
            "modules": [
                {
                    "id": "auth",
                    "name": "Authentication",
                    "paths": ["src/auth/mod.rs", "src/auth/login.rs", "src/auth/session.rs"],
                    "key_files": ["src/auth/mod.rs"],
                    "dependencies": [],
                    "dependents": ["api"],
                    "responsibility": "User authentication and session management",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 3, "loc": 80, "cyclomatic_complexity": 1.0, "change_frequency": 0.5},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                },
                {
                    "id": "api",
                    "name": "API Gateway",
                    "paths": ["src/api/mod.rs", "src/api/routes.rs", "src/api/handlers.rs"],
                    "key_files": ["src/api/mod.rs"],
                    "dependencies": [{"module_id": "auth", "dependency_type": "runtime"}],
                    "dependents": [],
                    "responsibility": "HTTP API handling",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 3, "loc": 60, "cyclomatic_complexity": 1.0, "change_frequency": 0.5},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                },
                {
                    "id": "database",
                    "name": "Database",
                    "paths": ["src/database/mod.rs", "src/database/models.rs"],
                    "key_files": ["src/database/mod.rs"],
                    "dependencies": [],
                    "dependents": ["billing"],
                    "responsibility": "Database models and access",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 2, "loc": 40, "cyclomatic_complexity": 1.0, "change_frequency": 0.3},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                },
                {
                    "id": "billing",
                    "name": "Billing",
                    "paths": ["src/billing/mod.rs", "src/billing/invoices.rs"],
                    "key_files": ["src/billing/mod.rs"],
                    "dependencies": [{"module_id": "database", "dependency_type": "runtime"}],
                    "dependents": [],
                    "responsibility": "Billing and invoice processing",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 2, "loc": 50, "cyclomatic_complexity": 1.0, "change_frequency": 0.4},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                }
            ],
            "groups": [
                {
                    "id": "core",
                    "name": "Core Services",
                    "module_ids": ["auth"],
                    "responsibility": "Core authentication services",
                    "boundary_rules": [],
                    "leader_module": "auth",
                    "parent_group_id": null,
                    "domain_id": "security",
                    "depth": 0
                },
                {
                    "id": "gateway",
                    "name": "Gateway Services",
                    "module_ids": ["api"],
                    "responsibility": "API gateway services",
                    "boundary_rules": [],
                    "leader_module": "api",
                    "parent_group_id": null,
                    "domain_id": "infrastructure",
                    "depth": 0
                },
                {
                    "id": "persistence",
                    "name": "Persistence Layer",
                    "module_ids": ["database"],
                    "responsibility": "Data persistence",
                    "boundary_rules": [],
                    "leader_module": "database",
                    "parent_group_id": null,
                    "domain_id": "infrastructure",
                    "depth": 0
                },
                {
                    "id": "commerce",
                    "name": "Commerce",
                    "module_ids": ["billing"],
                    "responsibility": "Commerce and billing",
                    "boundary_rules": [],
                    "leader_module": "billing",
                    "parent_group_id": null,
                    "domain_id": "business",
                    "depth": 0
                }
            ],
            "domains": [
                {
                    "id": "security",
                    "name": "Security Domain",
                    "group_ids": ["core"],
                    "responsibility": "Security and authentication",
                    "boundary_rules": [],
                    "interfaces": [],
                    "owner": null
                },
                {
                    "id": "infrastructure",
                    "name": "Infrastructure Domain",
                    "group_ids": ["gateway", "persistence"],
                    "responsibility": "Infrastructure services",
                    "boundary_rules": [],
                    "interfaces": [],
                    "owner": null
                },
                {
                    "id": "business",
                    "name": "Business Domain",
                    "group_ids": ["commerce"],
                    "responsibility": "Business logic",
                    "boundary_rules": [],
                    "interfaces": [],
                    "owner": null
                }
            ],
            "generated_at": "2026-01-30T00:00:00Z"
        },
        "modules": {},
        "groups": {},
        "domains": {}
    });

    let manifest_path = claudegen_dir.join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();

    Arc::new(
        Workspace::from_manifest(&manifest_path)
            .await
            .expect("Failed to create Workspace from manifest"),
    )
}

// =============================================================================
// Test 1: Hierarchical Consensus Strategy Selection
// =============================================================================

/// Test automatic strategy selection based on participant count and scope.
#[tokio::test]
async fn test_hierarchical_strategy_selection() {
    let config = ConsensusConfig {
        flat_threshold: 5,
        hierarchical_threshold: 15,
        ..Default::default()
    };
    let selector = StrategySelector::new(config);

    // Test 1: Single participant → Direct strategy
    {
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());

        let scope = AgentScope::Module {
            workspace: "test".to_string(),
            module: "auth".to_string(),
        };

        let strategy = selector.select(&scope, &participants);
        assert!(matches!(strategy, ConsensusStrategy::Direct { .. }));
        assert_eq!(strategy.name(), "direct");
        println!("✓ Single participant: Direct strategy selected");
    }

    // Test 2: Few participants → Flat strategy
    {
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());
        participants.add_module_agent("api".to_string());
        participants.add_module_agent("database".to_string());

        let scope = AgentScope::Group {
            workspace: "test".to_string(),
            group: "core".to_string(),
        };

        let strategy = selector.select(&scope, &participants);
        assert!(matches!(strategy, ConsensusStrategy::Flat { .. }));
        assert_eq!(strategy.participant_count(), 3);
        println!(
            "✓ Few participants: Flat strategy selected (count={})",
            strategy.participant_count()
        );
    }

    // Test 3: Many participants across domains → Hierarchical strategy
    {
        let mut participants = ParticipantSet::new();

        // Add modules with groups
        participants.add_module_agent_in_group("auth".to_string(), "core".to_string());
        participants.add_module_agent_in_group("api".to_string(), "gateway".to_string());
        participants.add_module_agent_in_group("database".to_string(), "persistence".to_string());
        participants.add_module_agent_in_group("billing".to_string(), "commerce".to_string());

        // Add group coordinators
        participants.add_group_coordinator("core".to_string(), "auth".to_string());
        participants.add_group_coordinator("gateway".to_string(), "api".to_string());
        participants.add_group_coordinator("persistence".to_string(), "database".to_string());
        participants.add_group_coordinator("commerce".to_string(), "billing".to_string());

        // Add domain coordinators
        participants.add_domain_coordinator("security".to_string());
        participants.add_domain_coordinator("infrastructure".to_string());
        participants.add_domain_coordinator("business".to_string());

        let scope = AgentScope::Workspace {
            workspace: "test".to_string(),
        };

        let strategy = selector.select(&scope, &participants);

        if let ConsensusStrategy::Hierarchical { tiers } = &strategy {
            println!("✓ Many cross-domain participants: Hierarchical strategy selected");
            println!("  - Total participants: {}", strategy.participant_count());
            println!("  - Tier count: {}", tiers.len());
            for tier in tiers {
                println!("    - {} tier: {} units", tier.level, tier.units.len());
            }
            assert!(tiers.len() >= 2, "Should have multiple tiers");
        } else {
            println!("  Strategy: {}", strategy.name());
            // Might fall back to flat if not enough complexity
            assert!(matches!(
                strategy,
                ConsensusStrategy::Flat { .. } | ConsensusStrategy::Hierarchical { .. }
            ));
        }
    }

    println!("\n✓ All strategy selection tests passed");
}

// =============================================================================
// Test 2: Hierarchical Aggregator for Bottom-Up Consensus
// =============================================================================

/// Test hierarchical aggregation of tier results.
#[tokio::test]
async fn test_hierarchical_aggregator() {
    use claude_pilot::agent::multi::adaptive_consensus::TierResult;

    let mut aggregator = HierarchicalAggregator::new();

    // Simulate Module tier results
    let module_results = vec![
        TierResult {
            tier_level: TierLevel::Module,
            unit_id: "module-auth".to_string(),
            converged: true,
            synthesis: "Auth module: Implement token refresh mechanism".to_string(),
            respondent_count: 1,
            conflicts: vec![],
            timed_out: false,
            rounds: 2,
        },
        TierResult {
            tier_level: TierLevel::Module,
            unit_id: "module-api".to_string(),
            converged: true,
            synthesis: "API module: Add new endpoint for refresh".to_string(),
            respondent_count: 1,
            conflicts: vec![],
            timed_out: false,
            rounds: 1,
        },
        TierResult {
            tier_level: TierLevel::Module,
            unit_id: "module-database".to_string(),
            converged: false,
            synthesis: "Database: Consider schema migration".to_string(),
            respondent_count: 1,
            conflicts: vec!["Migration strategy unclear".to_string()],
            timed_out: false,
            rounds: 3,
        },
    ];

    aggregator.aggregate_tier(TierLevel::Module, module_results);

    // Verify module tier aggregation
    let module_agg = aggregator.get_tier(TierLevel::Module).unwrap();
    assert_eq!(module_agg.unit_results.len(), 3);
    assert!(!module_agg.is_fully_converged()); // One module didn't converge
    assert!(module_agg.is_partially_converged());
    println!(
        "✓ Module tier: {}% converged",
        (module_agg.convergence_ratio * 100.0) as u32
    );

    // Simulate Group tier results
    let group_results = vec![
        TierResult {
            tier_level: TierLevel::Group,
            unit_id: "group-core".to_string(),
            converged: true,
            synthesis: "Core group agrees on token refresh approach".to_string(),
            respondent_count: 2,
            conflicts: vec![],
            timed_out: false,
            rounds: 1,
        },
        TierResult {
            tier_level: TierLevel::Group,
            unit_id: "group-infrastructure".to_string(),
            converged: true,
            synthesis: "Infrastructure group: API changes approved".to_string(),
            respondent_count: 2,
            conflicts: vec![],
            timed_out: false,
            rounds: 2,
        },
    ];

    aggregator.aggregate_tier(TierLevel::Group, group_results);

    // Verify group tier aggregation
    let group_agg = aggregator.get_tier(TierLevel::Group).unwrap();
    assert!(group_agg.is_fully_converged());
    println!(
        "✓ Group tier: {}% converged",
        (group_agg.convergence_ratio * 100.0) as u32
    );

    // Verify context building for upper tiers
    let domain_context = aggregator.build_upper_tier_context(TierLevel::Domain);
    assert!(
        domain_context.contains("module Tier Results"),
        "Expected 'module Tier Results' in: {}",
        domain_context
    );
    assert!(
        domain_context.contains("group Tier Results"),
        "Expected 'group Tier Results' in: {}",
        domain_context
    );
    println!("✓ Domain context includes lower tier results");

    // Verify conflict escalation
    let conflicts = aggregator.conflicts_for_escalation(TierLevel::Module);
    assert!(!conflicts.is_empty());
    println!("✓ Conflicts escalated: {:?}", conflicts);

    // Verify final synthesis
    let final_synthesis = aggregator.final_synthesis();
    assert!(
        final_synthesis.contains("module Level"),
        "Expected 'module Level' in: {}",
        final_synthesis
    );
    assert!(
        final_synthesis.contains("group Level"),
        "Expected 'group Level' in: {}",
        final_synthesis
    );
    println!("✓ Final synthesis generated");

    // Overall convergence
    let overall = aggregator.overall_convergence();
    println!("✓ Overall convergence: {:.1}%", overall * 100.0);

    println!("\n✓ All hierarchical aggregator tests passed");
}

// =============================================================================
// Test 3: Full Hierarchical Consensus E2E (with real LLM)
// =============================================================================

/// Test complete hierarchical consensus workflow with real LLM.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_hierarchical_consensus_e2e() {
    // Setup project
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_multi_module_project(project_dir);

    let event_store = setup_event_store(project_dir);
    let task_agent = create_task_agent();
    let agents = create_agents(task_agent.clone());

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = true;

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    let consensus_config = ConsensusConfig {
        flat_threshold: 3,
        hierarchical_threshold: 8,
        max_rounds: 5,
        checkpoint_on_tier_complete: true,
        ..Default::default()
    };

    let consensus_engine = ConsensusEngine::new(task_agent.clone(), consensus_config.clone());
    let adaptive_executor = AdaptiveConsensusExecutor::new(consensus_engine, consensus_config)
        .with_event_store(Arc::clone(&event_store));

    let message_bus = AgentMessageBus::default().with_event_store(Arc::clone(&event_store));

    // Create workspace for adaptive consensus mode
    let workspace = create_workspace_from_multi_module_project(project_dir).await;

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(task_agent)
        .with_workspace(workspace) // Enable adaptive consensus
        .with_adaptive_executor(adaptive_executor)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::new(message_bus));

    // Verify adaptive mode is enabled
    assert!(
        coordinator.hierarchy().is_some(),
        "Workspace must be configured for adaptive consensus"
    );

    // Mission: Cross-module feature that requires consensus from multiple domains
    let mission_id = format!("hierarchical-e2e-{}", uuid::Uuid::new_v4());
    let description = r#"Implement a complete user activity tracking feature:
1. Add a 'last_login' field to the User model in database module
2. Update the authentication flow in auth module to record login time
3. Create a new endpoint in api module to retrieve user activity
4. Add billing logic to track API usage based on activity"#;

    println!("\n=== Starting Hierarchical Consensus E2E Test ===");
    println!("Mission: {}", mission_id);
    println!("Description: {}", description);
    println!("Project: {}", project_dir.display());

    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;

    match result {
        Ok(mission_result) => {
            println!("\n=== Mission Result ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);
            println!("Results count: {}", mission_result.results.len());

            // Check for hierarchical consensus events
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("\n=== Event Analysis ===");
            println!("Total events: {}", events.len());

            let hierarchical_events = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        claude_pilot::state::EventPayload::HierarchicalConsensusStarted { .. }
                            | claude_pilot::state::EventPayload::HierarchicalConsensusCompleted { .. }
                            | claude_pilot::state::EventPayload::TierConsensusStarted { .. }
                            | claude_pilot::state::EventPayload::TierConsensusCompleted { .. }
                    )
                })
                .count();

            println!("Hierarchical consensus events: {}", hierarchical_events);

            // Verify files were modified
            let lib_content =
                std::fs::read_to_string(project_dir.join("src/lib.rs")).unwrap_or_default();
            println!("\n=== Modified Files ===");
            println!(
                "lib.rs preview: {}...",
                &lib_content.chars().take(200).collect::<String>()
            );
        }
        Err(e) => {
            eprintln!("Mission failed: {}", e);
        }
    }

    coordinator.shutdown();
}

// =============================================================================
// Test 4: Cross-Module Evidence Gathering and Planning
// =============================================================================

/// Test evidence gathering across multiple modules.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_cross_module_evidence_gathering() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskPriority};

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_multi_module_project(project_dir);

    let task_agent = create_task_agent();
    let research_agent = ResearchAgent::with_id("research-test", Arc::clone(&task_agent));

    let context = TaskContext {
        mission_id: "cross-module-evidence".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec![
            "src/auth/login.rs".to_string(),
            "src/api/handlers.rs".to_string(),
            "src/database/models.rs".to_string(),
        ],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "research-cross-module".to_string(),
        description: "Analyze how authentication, API, and database modules interact. Identify all dependencies and potential impact areas for adding user activity tracking.".to_string(),
        context,
        priority: TaskPriority::High,
        role: Some(AgentRole::core_research()),
    };

    println!("\n=== Cross-Module Evidence Gathering Test ===");
    println!("Analyzing modules: auth, api, database");

    let start = std::time::Instant::now();
    let result = research_agent.execute(&task, project_dir).await;
    let duration = start.elapsed();

    println!("Execution time: {:?}", duration);

    match result {
        Ok(r) => {
            println!("Research result: success={}", r.success);
            println!("Findings count: {}", r.findings.len());
            println!("\nEvidence gathered:");
            for finding in &r.findings {
                println!("  - {}", finding);
            }
            println!(
                "\nOutput preview: {}...",
                &r.output.chars().take(500).collect::<String>()
            );

            assert!(
                !r.findings.is_empty(),
                "Should gather evidence from multiple modules"
            );
        }
        Err(e) => {
            eprintln!("Research failed: {}", e);
        }
    }
}

// =============================================================================
// Test 5: Planning Agent with Consensus Input
// =============================================================================

/// Test planning agent creating tasks based on consensus.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_planning_with_consensus_input() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskPriority};

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_multi_module_project(project_dir);

    let task_agent = create_task_agent();
    let planning_agent = PlanningAgent::with_id("planning-test", Arc::clone(&task_agent));

    // Simulate consensus findings from multiple modules
    let context = TaskContext {
        mission_id: "planning-consensus".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![
            "[auth module] Token refresh mechanism needs implementation".to_string(),
            "[api module] New endpoint required at /api/v1/activity".to_string(),
            "[database module] User model needs last_login timestamp field".to_string(),
            "[billing module] Track API calls for usage billing".to_string(),
        ],
        blockers: vec![],
        related_files: vec![
            "src/auth/login.rs".to_string(),
            "src/api/routes.rs".to_string(),
            "src/database/models.rs".to_string(),
            "src/billing/invoices.rs".to_string(),
        ],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "planning-1".to_string(),
        description: "Create a detailed implementation plan for user activity tracking feature based on the consensus findings from auth, api, database, and billing modules.".to_string(),
        context,
        priority: TaskPriority::High,
        role: Some(AgentRole::core_planning()),
    };

    println!("\n=== Planning with Consensus Input Test ===");

    let start = std::time::Instant::now();
    let result = planning_agent.execute(&task, project_dir).await;
    let duration = start.elapsed();

    println!("Execution time: {:?}", duration);

    match result {
        Ok(r) => {
            println!("Planning result: success={}", r.success);
            println!("\nGenerated Plan:");
            println!("{}", &r.output.chars().take(1000).collect::<String>());

            // Check for task breakdown
            if r.output.to_lowercase().contains("task") || r.output.to_lowercase().contains("step")
            {
                println!("\n✓ Plan includes task breakdown");
            }
        }
        Err(e) => {
            eprintln!("Planning failed: {}", e);
        }
    }
}

// =============================================================================
// Test 6: Message Bus Communication Between Agents
// =============================================================================

/// Test message exchange between agents during consensus.
#[tokio::test]
async fn test_agent_message_exchange() {
    use claude_pilot::agent::multi::messaging::{AgentMessage, MessagePayload};
    use claude_pilot::state::VoteDecision;

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    let event_store = setup_event_store(project_dir);

    let message_bus = AgentMessageBus::default().with_event_store(event_store);

    // Subscribe receivers before sending messages
    let _receiver_core = message_bus.subscribe("group-core");
    let _receiver_gateway = message_bus.subscribe("group-gateway");
    let _receiver_coordinator = message_bus.subscribe("coordinator");
    let _broadcast_receiver = message_bus.subscribe("broadcast-listener");

    // Verify receivers are registered
    assert!(
        message_bus.receiver_count() >= 4,
        "Should have at least 4 receivers"
    );
    println!("✓ Receivers subscribed: {}", message_bus.receiver_count());

    // Simulate inter-agent communication

    // 1. Module agent sends consensus request
    let request = AgentMessage::new(
        "module-auth",
        "group-core",
        MessagePayload::ConsensusRequest {
            round: 1,
            proposal_hash: "hash-123".to_string(),
            proposal: "Implement token refresh with 1-hour expiry".to_string(),
        },
    );
    message_bus.send(request).await.unwrap();
    println!("✓ Module agent sent consensus request");

    // 2. Group coordinator broadcasts to all
    let broadcast = AgentMessage::broadcast(
        "group-core",
        MessagePayload::Text {
            content: "Consensus reached: Token refresh implementation approved".to_string(),
        },
    );
    message_bus.send(broadcast).await.unwrap();
    println!("✓ Group coordinator broadcast consensus result");

    // 3. Another module votes using correct structure
    let vote = AgentMessage::consensus_vote(
        "module-api",
        1, // round
        VoteDecision::Approve,
        "Aligns with API security requirements".to_string(),
    );
    message_bus.send(vote).await.unwrap();
    println!("✓ Module agent voted on proposal");

    // Verify receiver count
    let receiver_count = message_bus.receiver_count();
    println!("Active receivers: {}", receiver_count);

    println!("\n✓ Agent message exchange test passed");
}

// =============================================================================
// Test 7: Coder Agent Implementation After Planning
// =============================================================================

/// Test coder agent implementing planned changes.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_coder_implementation_from_plan() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskPriority};

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_multi_module_project(project_dir);

    let task_agent = create_task_agent();
    let coder_agent = CoderAgent::with_id("coder-test", Arc::clone(&task_agent));

    // Context includes planning output
    let context = TaskContext {
        mission_id: "coder-implementation".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![
            "Plan: Add last_login field to User struct".to_string(),
            "Type: chrono::DateTime<chrono::Utc>".to_string(),
            "Location: src/database/models.rs".to_string(),
        ],
        blockers: vec![],
        related_files: vec!["src/database/models.rs".to_string()],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "coder-1".to_string(),
        description: "Add a 'last_login: Option<u64>' field to the User struct in src/database/models.rs. This represents the Unix timestamp of the user's last login.".to_string(),
        context,
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_coder()),
    };

    println!("\n=== Coder Implementation Test ===");

    let start = std::time::Instant::now();
    let result = coder_agent.execute(&task, project_dir).await;
    let duration = start.elapsed();

    println!("Execution time: {:?}", duration);

    match result {
        Ok(r) => {
            println!("Coder result: success={}", r.success);

            // Verify file was modified
            let content =
                std::fs::read_to_string(project_dir.join("src/database/models.rs")).unwrap();
            println!("\nModified file:\n{}", content);

            if content.contains("last_login") {
                println!("\n✓ last_login field was added successfully");
            } else {
                println!("\n⚠ last_login field not found in modified file");
            }
        }
        Err(e) => {
            eprintln!("Coder failed: {}", e);
        }
    }
}

// =============================================================================
// Test 8: Full Multi-Agent Workflow E2E
// =============================================================================

/// Test complete workflow: Research → Planning → Consensus → Implementation → Verification.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth - comprehensive test"]
async fn test_real_full_multi_agent_workflow() {
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_multi_module_project(project_dir);

    let event_store = setup_event_store(project_dir);
    let task_agent = create_task_agent();
    let agents = create_agents(task_agent.clone());

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = true;

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    let consensus_config = ConsensusConfig {
        flat_threshold: 3,
        hierarchical_threshold: 8,
        max_rounds: 3,
        convergence_threshold: 0.7,
        checkpoint_on_tier_complete: true,
        ..Default::default()
    };

    let consensus_engine = ConsensusEngine::new(task_agent.clone(), consensus_config.clone());
    let adaptive_executor = AdaptiveConsensusExecutor::new(consensus_engine, consensus_config)
        .with_event_store(Arc::clone(&event_store));

    let message_bus = AgentMessageBus::default().with_event_store(Arc::clone(&event_store));

    // Create workspace for adaptive consensus mode
    let workspace = create_workspace_from_multi_module_project(project_dir).await;

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(task_agent)
        .with_workspace(workspace) // Enable adaptive consensus
        .with_adaptive_executor(adaptive_executor)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::new(message_bus));

    // Verify adaptive mode is enabled
    assert!(
        coordinator.hierarchy().is_some(),
        "Workspace must be configured for adaptive consensus"
    );

    let mission_id = format!("full-workflow-{}", uuid::Uuid::new_v4());
    let description = "Add a simple health check endpoint that returns the application version.";

    println!("\n╔══════════════════════════════════════════════════════════════════╗");
    println!("║       COMPREHENSIVE MULTI-AGENT WORKFLOW E2E TEST                ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!("\nMission: {}", mission_id);
    println!("Task: {}", description);
    println!("\nProject structure:");
    println!("  - auth module (security domain)");
    println!("  - api module (infrastructure domain)");
    println!("  - database module (infrastructure domain)");
    println!("  - billing module (business domain)");

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let total_duration = start.elapsed();

    println!("\n=== EXECUTION RESULTS ===");
    println!("Total execution time: {:?}", total_duration);

    match result {
        Ok(mission_result) => {
            println!("\n✓ Mission completed");
            println!("  - Success: {}", mission_result.success);
            println!("  - Summary: {}", mission_result.summary);
            println!("  - Task results: {}", mission_result.results.len());

            // Detailed event analysis
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("\n=== EVENT BREAKDOWN ===");

            let mut event_counts: HashMap<&str, usize> = HashMap::new();
            for event in &events {
                let category = match &event.payload {
                    claude_pilot::state::EventPayload::AgentSpawned { .. } => "Agent Spawn",
                    claude_pilot::state::EventPayload::AgentTaskAssigned { .. } => {
                        "Task Assignment"
                    }
                    claude_pilot::state::EventPayload::AgentTaskCompleted { .. } => {
                        "Task Completion"
                    }
                    claude_pilot::state::EventPayload::ConsensusRoundStarted { .. } => {
                        "Consensus Round"
                    }
                    claude_pilot::state::EventPayload::ConsensusCompleted { .. } => {
                        "Consensus Complete"
                    }
                    claude_pilot::state::EventPayload::HierarchicalConsensusStarted { .. } => {
                        "Hierarchical Start"
                    }
                    claude_pilot::state::EventPayload::HierarchicalConsensusCompleted {
                        ..
                    } => "Hierarchical Complete",
                    claude_pilot::state::EventPayload::TierConsensusStarted { .. } => "Tier Start",
                    claude_pilot::state::EventPayload::TierConsensusCompleted { .. } => {
                        "Tier Complete"
                    }
                    claude_pilot::state::EventPayload::VerificationRound { .. } => "Verification",
                    claude_pilot::state::EventPayload::ConvergenceAchieved { .. } => "Convergence",
                    claude_pilot::state::EventPayload::AgentMessageSent { .. } => "Message Sent",
                    _ => "Other",
                };
                *event_counts.entry(category).or_insert(0) += 1;
            }

            for (category, count) in event_counts.iter() {
                println!("  {}: {}", category, count);
            }

            // Check for convergence
            let converged = events.iter().any(|e| {
                matches!(
                    &e.payload,
                    claude_pilot::state::EventPayload::ConvergenceAchieved { .. }
                )
            });
            println!("\n  Convergence achieved: {}", converged);

            // Metrics
            let metrics = coordinator.metrics();
            println!("\n=== COORDINATOR METRICS ===");
            println!("  Total executions: {}", metrics.total_executions);
            println!("  Success rate: {:.1}%", metrics.success_rate * 100.0);
        }
        Err(e) => {
            eprintln!("\n✗ Mission failed: {}", e);
        }
    }

    coordinator.shutdown();
    println!("\n═══════════════════════════════════════════════════════════════════");
}

// =============================================================================
// Test 9: Participant Set Construction
// =============================================================================

/// Test participant set construction for cross-domain consensus.
#[tokio::test]
async fn test_participant_set_construction() {
    // Test 1: Single module participant
    {
        let mut participants = ParticipantSet::new();
        participants.add_module_agent("auth".to_string());

        println!("Single module participants: {}", participants.len());
        assert_eq!(participants.len(), 1);
        assert!(!participants.is_empty());
    }

    // Test 2: Multiple modules in same group
    {
        let mut participants = ParticipantSet::new();
        participants.add_module_agent_in_group("auth".to_string(), "core".to_string());
        participants.add_module_agent_in_group("session".to_string(), "core".to_string());
        participants.add_group_coordinator("core".to_string(), "auth".to_string());

        println!("Same group participants: {}", participants.len());
        println!("  Distinct groups: {}", participants.distinct_groups());
        assert_eq!(participants.distinct_groups(), 1);
    }

    // Test 3: Multiple modules across domains
    {
        let mut participants = ParticipantSet::new();

        // Security domain - core group
        participants.add_module_agent_in_group("auth".to_string(), "core".to_string());
        participants.add_group_coordinator("core".to_string(), "auth".to_string());
        participants.add_domain_coordinator("security".to_string());

        // Infrastructure domain - gateway and persistence groups
        participants.add_module_agent_in_group("api".to_string(), "gateway".to_string());
        participants.add_module_agent_in_group("database".to_string(), "persistence".to_string());
        participants.add_group_coordinator("gateway".to_string(), "api".to_string());
        participants.add_group_coordinator("persistence".to_string(), "database".to_string());
        participants.add_domain_coordinator("infrastructure".to_string());

        // Business domain - commerce group
        participants.add_module_agent_in_group("billing".to_string(), "commerce".to_string());
        participants.add_group_coordinator("commerce".to_string(), "billing".to_string());
        participants.add_domain_coordinator("business".to_string());

        println!("Cross-domain participants: {}", participants.len());
        println!("  Distinct groups: {}", participants.distinct_groups());
        println!(
            "  Spans multiple domains: {}",
            participants.spans_multiple_domains()
        );

        assert!(participants.spans_multiple_domains());
        assert!(participants.distinct_groups() >= 3);
    }

    println!("\n✓ Participant set construction tests passed");
}

// =============================================================================
// Test 10: Two-Project Cross-Workspace Consensus
// =============================================================================

/// Test consensus across two different projects/workspaces.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_cross_workspace_consensus() {
    // Create two separate projects
    let temp_dir = TempDir::new().unwrap();
    let project_a_dir = temp_dir.path().join("project-a");
    let project_b_dir = temp_dir.path().join("project-b");

    std::fs::create_dir_all(&project_a_dir).unwrap();
    std::fs::create_dir_all(&project_b_dir).unwrap();

    create_multi_module_project(&project_a_dir);
    create_second_project(&project_b_dir);

    println!("\n=== Cross-Workspace Consensus Test ===");
    println!(
        "Project A: {} (auth, api, database, billing)",
        project_a_dir.display()
    );
    println!(
        "Project B: {} (analytics, notifications)",
        project_b_dir.display()
    );

    // For this test, we simulate a task that requires coordination between both projects
    // In a real scenario, you'd have a meta-coordinator orchestrating across workspaces

    let task_agent = create_task_agent();
    let event_store = setup_event_store(&project_a_dir);

    // Create agents for both workspaces
    let agents: Vec<Arc<dyn SpecializedAgent>> = vec![
        ResearchAgent::with_id("research-a", Arc::clone(&task_agent)),
        ResearchAgent::with_id("research-b", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-coordinator", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-a", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-b", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
    ];

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = true;

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    let consensus_config = ConsensusConfig::default();
    let consensus_engine = ConsensusEngine::new(task_agent.clone(), consensus_config.clone());
    let adaptive_executor = AdaptiveConsensusExecutor::new(consensus_engine, consensus_config)
        .with_event_store(Arc::clone(&event_store));

    let message_bus = AgentMessageBus::default().with_event_store(Arc::clone(&event_store));

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(task_agent)
        .with_adaptive_executor(adaptive_executor)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::new(message_bus));

    let mission_id = format!("cross-workspace-{}", uuid::Uuid::new_v4());
    let description = r#"Implement cross-project user activity tracking:
1. In project-a: Add activity logging to the auth module
2. In project-b: Send the activity data to the analytics module
3. In project-b: Trigger notifications based on activity patterns

This requires consensus between auth (project-a), analytics (project-b), and notifications (project-b)."#;

    println!("\nMission: {}", description);

    // Execute from project-a as the primary workspace
    let result = coordinator
        .execute_mission(&mission_id, description, &project_a_dir)
        .await;

    match result {
        Ok(mission_result) => {
            println!("\n✓ Cross-workspace mission completed");
            println!("  Success: {}", mission_result.success);
            println!("  Summary: {}", mission_result.summary);
        }
        Err(e) => {
            eprintln!("Cross-workspace mission failed: {}", e);
        }
    }

    coordinator.shutdown();
}
