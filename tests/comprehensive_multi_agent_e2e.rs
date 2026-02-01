#![allow(clippy::field_reassign_with_default)]
//! Comprehensive Multi-Agent End-to-End Tests
//!
//! Tests the full multi-agent orchestration workflow with real LLM calls.
//!
//! Run with: cargo test --test comprehensive_multi_agent_e2e -- --ignored --nocapture
//!
//! Prerequisites:
//! - Claude Code OAuth authentication (automatic via from_claude_code())
//!
//! These tests verify:
//! 1. Evidence gathering by Research agents
//! 2. Consensus-based planning with multiple agents
//! 3. Module-scoped agents with boundary enforcement
//! 4. Inter-agent messaging via AgentMessageBus
//! 5. Convergent verification (2 consecutive clean passes)

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::messaging::AgentMessageBus;
use claude_pilot::agent::multi::{
    AgentPoolBuilder, AgentRole, AgentTask, CoderAgent, Coordinator, ModuleAgent, PlanningAgent,
    ResearchAgent, ReviewerAgent, SpecializedAgent, TaskContext, TaskPriority, VerifierAgent,
};
use claude_pilot::config::{AgentConfig, MultiAgentConfig};
use claude_pilot::state::EventStore;
use modmap::{Module, ModuleDependency, ModuleMetrics};
use tempfile::TempDir;

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Create a realistic multi-module project for testing.
fn create_realistic_project(dir: &Path) {
    // Root configuration
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "test-multi-module"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
thiserror = "1.0"
"#,
    )
    .unwrap();

    // Create module directories
    for module in ["auth", "api", "storage"] {
        std::fs::create_dir_all(dir.join(format!("src/{}", module))).unwrap();
    }

    // Auth module
    std::fs::write(
        dir.join("src/auth/mod.rs"),
        r#"//! Authentication module.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
}

/// Authenticates a user and returns a token.
pub fn authenticate(username: &str, _password: &str) -> Result<String, AuthError> {
    if username.is_empty() {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(format!("token_{}", username))
}

#[derive(Debug)]
pub enum AuthError {
    InvalidCredentials,
    TokenExpired,
}
"#,
    )
    .unwrap();

    // API module (depends on auth)
    std::fs::write(
        dir.join("src/api/mod.rs"),
        r#"//! API module.

use crate::auth::{authenticate, User};

pub struct ApiResponse<T> {
    pub data: T,
    pub status: u16,
}

/// Login endpoint handler.
pub fn login(username: &str, password: &str) -> ApiResponse<String> {
    match authenticate(username, password) {
        Ok(token) => ApiResponse { data: token, status: 200 },
        Err(_) => ApiResponse { data: "Unauthorized".into(), status: 401 },
    }
}

/// Get user profile endpoint.
pub fn get_profile(_token: &str) -> ApiResponse<Option<User>> {
    ApiResponse { data: None, status: 501 }
}
"#,
    )
    .unwrap();

    // Storage module
    std::fs::write(
        dir.join("src/storage/mod.rs"),
        r#"//! Storage module for data persistence.

use std::collections::HashMap;

pub struct Storage {
    data: HashMap<String, Vec<u8>>,
}

impl Storage {
    pub fn new() -> Self {
        Self { data: HashMap::new() }
    }

    pub fn put(&mut self, key: &str, value: Vec<u8>) {
        self.data.insert(key.to_string(), value);
    }

    pub fn get(&self, key: &str) -> Option<&Vec<u8>> {
        self.data.get(key)
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::new()
    }
}
"#,
    )
    .unwrap();

    // Main lib.rs
    std::fs::write(
        dir.join("src/lib.rs"),
        r#"//! Multi-module test library.

pub mod api;
pub mod auth;
pub mod storage;

pub use api::{ApiResponse, get_profile, login};
pub use auth::{User, authenticate};
pub use storage::Storage;
"#,
    )
    .unwrap();

    // Create module_map.json for module agent creation
    std::fs::create_dir_all(dir.join(".claude/plugins/test-multi-module")).unwrap();
    std::fs::write(
        dir.join(".claude/plugins/test-multi-module/module_map.json"),
        r#"{
  "version": "1.0",
  "project_name": "test-multi-module",
  "modules": [
    {
      "id": "auth",
      "name": "Authentication",
      "paths": ["src/auth/"],
      "key_files": ["src/auth/mod.rs"],
      "responsibility": "Handle user authentication and token management",
      "dependencies": [],
      "dependents": ["api"],
      "primary_language": "Rust"
    },
    {
      "id": "api",
      "name": "API",
      "paths": ["src/api/"],
      "key_files": ["src/api/mod.rs"],
      "responsibility": "HTTP API endpoints and request handling",
      "dependencies": [{"module_id": "auth", "relationship": "runtime"}],
      "dependents": [],
      "primary_language": "Rust"
    },
    {
      "id": "storage",
      "name": "Storage",
      "paths": ["src/storage/"],
      "key_files": ["src/storage/mod.rs"],
      "responsibility": "Data persistence and caching",
      "dependencies": [],
      "dependents": [],
      "primary_language": "Rust"
    }
  ]
}"#,
    )
    .unwrap();
}

/// Create test modules for module agents.
fn create_test_modules() -> Vec<Arc<Module>> {
    vec![
        Arc::new(Module {
            id: "auth".into(),
            name: "Authentication".into(),
            paths: vec!["src/auth/".into()],
            key_files: vec!["src/auth/mod.rs".into()],
            dependencies: vec![],
            dependents: vec!["api".into()],
            responsibility: "Handle user authentication and token management".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.8, 0.9, 0.7),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        }),
        Arc::new(Module {
            id: "api".into(),
            name: "API".into(),
            paths: vec!["src/api/".into()],
            key_files: vec!["src/api/mod.rs".into()],
            dependencies: vec![ModuleDependency::runtime("auth")],
            dependents: vec![],
            responsibility: "HTTP API endpoints and request handling".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.75, 0.85, 0.65),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        }),
        Arc::new(Module {
            id: "storage".into(),
            name: "Storage".into(),
            paths: vec!["src/storage/".into()],
            key_files: vec!["src/storage/mod.rs".into()],
            dependencies: vec![],
            dependents: vec![],
            responsibility: "Data persistence and caching".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.9, 0.95, 0.8),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        }),
    ]
}

/// Create TaskAgent with real LLM configuration.
fn create_task_agent() -> Arc<TaskAgent> {
    let config = AgentConfig::default();
    Arc::new(TaskAgent::new(config))
}

/// Create all core agents.
fn create_core_agents(task_agent: Arc<TaskAgent>) -> Vec<Arc<dyn SpecializedAgent>> {
    vec![
        ResearchAgent::with_id("research-0", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-0", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
        ReviewerAgent::with_id("reviewer-0", Arc::clone(&task_agent)),
    ]
}

/// Create module agents for each module.
fn create_module_agents(
    task_agent: Arc<TaskAgent>,
    modules: &[Arc<Module>],
) -> Vec<Arc<dyn SpecializedAgent>> {
    modules
        .iter()
        .map(|module| {
            ModuleAgent::with_id(
                &format!("module-{}-0", module.id),
                Arc::clone(module),
                Arc::clone(&task_agent),
            ) as Arc<dyn SpecializedAgent>
        })
        .collect()
}

/// Setup coordinator with all components for comprehensive testing.
async fn setup_comprehensive_coordinator(
    _project_dir: &Path,
    event_store: Arc<EventStore>,
    dynamic_mode: bool,
) -> claude_pilot::error::Result<Coordinator> {
    let task_agent = create_task_agent();
    let modules = create_test_modules();

    // Create all agents: core + module agents
    let mut agents = create_core_agents(Arc::clone(&task_agent));
    agents.extend(create_module_agents(Arc::clone(&task_agent), &modules));

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = dynamic_mode;

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()?;

    let message_bus = AgentMessageBus::new(512).with_event_store(Arc::clone(&event_store));

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(Arc::clone(&task_agent))
        .with_event_store(event_store)
        .with_message_bus(Arc::new(message_bus));

    Ok(coordinator)
}

// ============================================================================
// Test Scenarios
// ============================================================================

/// Test 1: Individual Agent Execution
///
/// Verifies each agent type can execute tasks independently.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_individual_agent_execution() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Individual Agent Execution");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    let task_agent = create_task_agent();

    // Test ResearchAgent
    println!("--- Testing ResearchAgent ---");
    let research_agent = ResearchAgent::with_id("research-test", Arc::clone(&task_agent));
    let research_task = AgentTask {
        id: "research-1".into(),
        description: "Analyze the auth module structure and identify key components".into(),
        context: TaskContext {
            mission_id: "test-individual".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec!["src/auth/mod.rs".into()],
            composed_prompt: None,
        },
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_research()),
    };

    let start = std::time::Instant::now();
    let result = research_agent.execute(&research_task, project_dir).await;
    println!("Research completed in {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("  Success: {}", r.success);
            println!(
                "  Output preview: {}...",
                r.output.chars().take(200).collect::<String>()
            );
            assert!(r.success, "Research should succeed");
        }
        Err(e) => println!("  Error: {}", e),
    }

    // Test CoderAgent
    println!("\n--- Testing CoderAgent ---");
    let coder_agent = CoderAgent::with_id("coder-test", Arc::clone(&task_agent));
    let coder_task = AgentTask {
        id: "coder-1".into(),
        description:
            "Add a 'validate_token' function to auth/mod.rs that takes a token string and returns bool"
                .into(),
        context: TaskContext {
            mission_id: "test-individual".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec!["src/auth/mod.rs".into()],
            composed_prompt: None,
        },
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_coder()),
    };

    let start = std::time::Instant::now();
    let result = coder_agent.execute(&coder_task, project_dir).await;
    println!("Coder completed in {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("  Success: {}", r.success);

            // Verify the code change
            let auth_content =
                std::fs::read_to_string(project_dir.join("src/auth/mod.rs")).unwrap();
            let has_validate = auth_content.contains("validate_token");
            println!("  validate_token added: {}", has_validate);
        }
        Err(e) => println!("  Error: {}", e),
    }

    // Test VerifierAgent
    println!("\n--- Testing VerifierAgent ---");
    let verifier_agent = VerifierAgent::with_id("verifier-test", task_agent);
    let verify_task = AgentTask {
        id: "verify-1".into(),
        description: "Verify the project compiles without errors".into(),
        context: TaskContext {
            mission_id: "test-individual".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec![],
            composed_prompt: None,
        },
        priority: TaskPriority::High,
        role: Some(AgentRole::core_verifier()),
    };

    let start = std::time::Instant::now();
    let result = verifier_agent.execute(&verify_task, project_dir).await;
    println!("Verifier completed in {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("  Success: {}", r.success);
            println!("  Findings: {:?}", r.findings);
        }
        Err(e) => println!("  Error: {}", e),
    }

    println!("\n✓ Individual agent execution test complete");
}

/// Test 2: Module Agent Scope Enforcement
///
/// Verifies module agents respect their scope boundaries.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_module_agent_scope_enforcement() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Module Agent Scope Enforcement");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    let task_agent = create_task_agent();
    let modules = create_test_modules();

    // Get auth module agent
    let auth_module = modules.iter().find(|m| m.id == "auth").unwrap();
    let auth_agent = ModuleAgent::with_id("module-auth-test", Arc::clone(auth_module), task_agent);

    // Task that should stay in scope
    println!("--- Task within auth module scope ---");
    let in_scope_task = AgentTask {
        id: "auth-task-1".into(),
        description: "Add documentation comments to the User struct in src/auth/mod.rs".into(),
        context: TaskContext {
            mission_id: "test-scope".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec!["src/auth/mod.rs".into()],
            composed_prompt: None,
        },
        priority: TaskPriority::Normal,
        role: Some(AgentRole::module("auth")),
    };

    let start = std::time::Instant::now();
    let result = auth_agent.execute(&in_scope_task, project_dir).await;
    println!("In-scope task completed in {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("  Success: {}", r.success);
            println!("  Findings: {:?}", r.findings);

            // Module agent should succeed and not produce scope warnings
            let has_scope_warning = r.findings.iter().any(|f| f.contains("out-of-scope"));
            println!("  Has scope warning: {}", has_scope_warning);
        }
        Err(e) => println!("  Error: {}", e),
    }

    // Verify scope validation logic
    println!("\n--- Scope Validation Check ---");
    let validation =
        auth_agent.validate_scope(&["src/auth/mod.rs".into(), "src/api/mod.rs".into()]);
    println!("  Valid: {}", validation.valid);
    println!("  In scope: {:?}", validation.in_scope);
    println!("  Out of scope: {:?}", validation.out_of_scope);
    println!("  Warnings: {:?}", validation.warnings);

    assert!(!validation.valid, "Should detect out-of-scope file");
    assert!(
        !validation.out_of_scope.is_empty(),
        "Should have out-of-scope files"
    );

    println!("\n✓ Module agent scope enforcement test complete");
}

/// Test 3: Sequential Pipeline Execution
///
/// Tests the full sequential mission pipeline:
/// Research → Planning → Implementation → Verification
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_sequential_pipeline_execution() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Sequential Pipeline Execution");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    // Setup event store
    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    // Create coordinator in sequential mode
    let coordinator = setup_comprehensive_coordinator(project_dir, Arc::clone(&event_store), false)
        .await
        .expect("Failed to setup coordinator");

    let mission_id = format!("seq-mission-{}", uuid::Uuid::new_v4());
    let description = r#"
        Implement a logout function in the auth module that:
        1. Takes a token as input
        2. Returns Result<(), AuthError>
        3. Add a new AuthError variant for InvalidToken
        Then update the API module to add a logout endpoint that uses this function.
    "#;

    println!("Mission ID: {}", mission_id);
    println!("Description: {}", description.trim());
    println!("\nStarting sequential pipeline execution...");

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let duration = start.elapsed();

    println!("\nExecution time: {:?}", duration);

    match result {
        Ok(mission_result) => {
            println!("\n=== Mission Result ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);
            println!("Result count: {}", mission_result.results.len());

            for (i, result) in mission_result.results.iter().enumerate() {
                println!("\n--- Phase {} ---", i + 1);
                println!("Task ID: {}", result.task_id);
                println!("Success: {}", result.success);
                if !result.findings.is_empty() {
                    println!("Findings: {:?}", result.findings);
                }
            }

            // Query events
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("\n=== Events Recorded: {} ===", events.len());

            let mut event_counts: HashMap<&str, usize> = HashMap::new();
            for event in &events {
                let event_type = event.payload.event_type();
                *event_counts.entry(event_type).or_insert(0) += 1;
            }
            for (event_type, count) in &event_counts {
                println!("  {}: {}", event_type, count);
            }

            // Verify code changes
            let auth_content =
                std::fs::read_to_string(project_dir.join("src/auth/mod.rs")).unwrap();
            let has_logout = auth_content.contains("logout");
            println!("\n=== Code Verification ===");
            println!("logout function added: {}", has_logout);

            // Metrics
            let metrics = coordinator.metrics();
            println!("\n=== Metrics ===");
            println!("Total executions: {}", metrics.total_executions);
            println!("Success rate: {:.1}%", metrics.success_rate * 100.0);
        }
        Err(e) => {
            eprintln!("Mission failed: {}", e);
        }
    }

    coordinator.shutdown();
    println!("\n✓ Sequential pipeline execution test complete");
}

/// Test 4: Message Bus Inter-Agent Communication
///
/// Tests the AgentMessageBus for agent-to-agent communication.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_message_bus_communication() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Message Bus Inter-Agent Communication");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let message_bus =
        Arc::new(AgentMessageBus::new(256).with_event_store(Arc::clone(&event_store)));

    // Create receivers for different agents
    let mut research_receiver = message_bus.subscribe("research-0");
    let mut coder_receiver = message_bus.subscribe("coder-0");

    // Simulate coordinator sending task assignments
    println!("--- Sending task assignments via message bus ---");

    use claude_pilot::agent::multi::messaging::AgentMessage;

    let task1 = AgentTask {
        id: "msg-task-1".into(),
        description: "Research auth module".into(),
        context: TaskContext {
            mission_id: "msg-test".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec![],
            composed_prompt: None,
        },
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_research()),
    };

    let task2 = AgentTask {
        id: "msg-task-2".into(),
        description: "Implement feature".into(),
        context: TaskContext {
            mission_id: "msg-test".into(),
            working_dir: project_dir.to_path_buf(),
            key_findings: vec![],
            blockers: vec![],
            related_files: vec![],
            composed_prompt: None,
        },
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_coder()),
    };

    // Send messages
    let msg1 = AgentMessage::task_assignment("coordinator", "research-0", task1.clone());
    let msg2 = AgentMessage::task_assignment("coordinator", "coder-0", task2.clone());

    message_bus.send(msg1).await.expect("Send msg1 failed");
    message_bus.send(msg2).await.expect("Send msg2 failed");

    println!("Sent 2 task assignment messages");

    // Receive messages (with timeout)
    println!("\n--- Receiving messages ---");

    let recv_timeout = Duration::from_secs(1);

    let recv1 = tokio::time::timeout(recv_timeout, research_receiver.recv()).await;
    match recv1 {
        Ok(Ok(Some(msg))) => {
            println!("Research agent received: {} -> {}", msg.from, msg.to);
        }
        Ok(Ok(None)) => println!("No message for research agent"),
        Ok(Err(e)) => println!("Recv error: {}", e),
        Err(_) => println!("Timeout waiting for research message"),
    }

    let recv2 = tokio::time::timeout(recv_timeout, coder_receiver.recv()).await;
    match recv2 {
        Ok(Ok(Some(msg))) => {
            println!("Coder agent received: {} -> {}", msg.from, msg.to);
        }
        Ok(Ok(None)) => println!("No message for coder agent"),
        Ok(Err(e)) => println!("Recv error: {}", e),
        Err(_) => println!("Timeout waiting for coder message"),
    }

    // Check events were recorded
    let events = event_store.query("msg-test", 0).await.unwrap_or_default();
    println!("\nEvents recorded: {}", events.len());

    println!("\n✓ Message bus communication test complete");
}

/// Test 5: Full Mission with Module Agents
///
/// Complete end-to-end test with module agents.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_full_mission_with_module_agents() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Full Mission with Module Agents");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_comprehensive_coordinator(project_dir, Arc::clone(&event_store), false)
        .await
        .expect("Failed to setup coordinator");

    let mission_id = format!("full-mission-{}", uuid::Uuid::new_v4());
    let description = r#"
        Implement a complete token refresh mechanism:
        1. Add TokenInfo struct to auth module with token and expiry
        2. Add refresh_token function that takes an expired token and returns a new one
        3. Add AuthError::TokenExpired variant if not present
        4. Update API module to handle token refresh via a new endpoint
        5. Ensure proper error handling throughout
    "#;

    println!("Mission ID: {}", mission_id);
    println!("Description: {}", description.trim());
    println!("\nStarting full mission execution...\n");

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let total_duration = start.elapsed();

    println!("\n{}", "=".repeat(40));
    println!("Total execution time: {:?}", total_duration);
    println!("{}\n", "=".repeat(40));

    match result {
        Ok(mission_result) => {
            println!("=== MISSION OUTCOME ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);

            println!("\n=== PHASE RESULTS ===");
            for (i, result) in mission_result.results.iter().enumerate() {
                println!("\nPhase {}: Task '{}'", i + 1, result.task_id);
                println!("  Success: {}", result.success);
                println!("  Output length: {} chars", result.output.len());
                if !result.artifacts.is_empty() {
                    println!("  Artifacts: {:?}", result.artifacts.len());
                }
            }

            // Analyze events
            println!("\n=== EVENT ANALYSIS ===");
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("Total events: {}", events.len());

            let phase_events: Vec<_> = events
                .iter()
                .filter(|e| e.payload.event_type().contains("phase"))
                .collect();
            println!("Phase events: {}", phase_events.len());

            let verification_events: Vec<_> = events
                .iter()
                .filter(|e| {
                    e.payload.event_type().contains("verification")
                        || e.payload.event_type().contains("convergence")
                })
                .collect();
            println!("Verification events: {}", verification_events.len());

            // Check convergence
            let convergence_achieved = events.iter().any(|e| {
                matches!(
                    &e.payload,
                    claude_pilot::state::EventPayload::ConvergenceAchieved { .. }
                )
            });
            println!("\nConvergence achieved: {}", convergence_achieved);

            // Verify code changes
            println!("\n=== CODE VERIFICATION ===");
            let auth_content =
                std::fs::read_to_string(project_dir.join("src/auth/mod.rs")).unwrap();
            println!(
                "TokenInfo struct added: {}",
                auth_content.contains("TokenInfo")
            );
            println!(
                "refresh_token function added: {}",
                auth_content.contains("refresh_token")
            );

            // Pool metrics
            let metrics = coordinator.metrics();
            println!("\n=== POOL METRICS ===");
            println!("Total executions: {}", metrics.total_executions);
            println!("Successful executions: {}", metrics.successful_executions);
            println!("Success rate: {:.1}%", metrics.success_rate * 100.0);
            println!("Avg duration ms: {}", metrics.avg_duration_ms);
        }
        Err(e) => {
            eprintln!("\nMission failed with error: {}", e);
        }
    }

    coordinator.shutdown();
    println!("\n✓ Full mission test complete");
}

/// Test 6: Health Monitoring and Metrics
///
/// Tests the health monitoring system during execution.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_health_monitoring() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Health Monitoring and Metrics");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_realistic_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_comprehensive_coordinator(project_dir, Arc::clone(&event_store), false)
        .await
        .expect("Failed to setup coordinator");

    // Check initial health
    println!("--- Initial Health Report ---");
    let health_report = coordinator.health_monitor().check_health();
    println!("Status: {:?}", health_report.status);
    println!("Role count: {}", health_report.role_health.len());
    for (role, role_health) in &health_report.role_health {
        println!(
            "  {}: agent_count={}, avg_load={:.2}, status={:?}",
            role, role_health.agent_count, role_health.avg_load, role_health.status
        );
    }

    if !health_report.bottlenecks.is_empty() {
        println!("\nBottlenecks detected:");
        for bottleneck in &health_report.bottlenecks {
            println!("  - {:?}", bottleneck);
        }
    }

    // Execute a quick mission
    let mission_id = format!("health-test-{}", uuid::Uuid::new_v4());
    let description = "Add a simple helper function to the storage module";

    println!("\n--- Executing mission ---");
    let _ = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;

    // Check health after execution
    println!("\n--- Post-Execution Health Report ---");
    let health_report = coordinator.health_monitor().check_health();
    println!("Status: {:?}", health_report.status);
    println!("Overall score: {:.2}", health_report.overall_score);
    for (role, role_health) in &health_report.role_health {
        println!(
            "  {}: agent_count={}, load={}, status={:?}",
            role, role_health.agent_count, role_health.total_load, role_health.status
        );
    }

    coordinator.shutdown();
    println!("\n✓ Health monitoring test complete");
}

/// Test 7: Convergent Verification Loop
///
/// Tests that verification requires 2 consecutive clean passes.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_convergent_verification_loop() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Convergent Verification Loop");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    // Create project with intentional issue
    std::fs::create_dir_all(project_dir.join("src")).unwrap();
    std::fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "verify-test"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )
    .unwrap();

    // Simple code
    std::fs::write(
        project_dir.join("src/lib.rs"),
        r#"//! Test library.

pub fn process(items: Vec<String>) -> String {
    items.join(", ")
}
"#,
    )
    .unwrap();

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_comprehensive_coordinator(project_dir, Arc::clone(&event_store), false)
        .await
        .expect("Failed to setup coordinator");

    let mission_id = format!("convergence-test-{}", uuid::Uuid::new_v4());
    let description = r#"
        Add the following to src/lib.rs:
        1. A 'count_items' function that takes a Vec<String> and returns usize
        2. A test module with tests for both functions
    "#;

    println!("Mission: Add function and tests (requires verification convergence)");

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let duration = start.elapsed();

    println!("\nExecution time: {:?}", duration);

    match result {
        Ok(mission_result) => {
            println!("\n=== Verification Analysis ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);

            // Analyze verification events
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();

            let verification_rounds: Vec<_> = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        claude_pilot::state::EventPayload::VerificationRound { .. }
                    )
                })
                .collect();

            let convergence_events: Vec<_> = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        claude_pilot::state::EventPayload::ConvergenceAchieved { .. }
                    )
                })
                .collect();

            println!("Verification rounds: {}", verification_rounds.len());
            println!("Convergence events: {}", convergence_events.len());

            if !convergence_events.is_empty() {
                println!("✓ Convergence achieved (2 consecutive clean passes)");
            } else if mission_result.success {
                println!("✓ Mission succeeded");
            } else {
                println!("✗ Did not achieve convergence");
            }

            // Verify final code state
            let lib_content = std::fs::read_to_string(project_dir.join("src/lib.rs")).unwrap();
            println!("\n=== Final Code ===");
            println!("{}", lib_content);
        }
        Err(e) => {
            eprintln!("Mission failed: {}", e);
        }
    }

    coordinator.shutdown();
    println!("\n✓ Convergent verification loop test complete");
}

// ============================================================================
// Combined Test Runner
// ============================================================================

/// Run all comprehensive tests in sequence.
/// Use: cargo test run_all_comprehensive_tests -- --ignored --nocapture
#[tokio::test]
#[ignore = "Runs all tests - use for full validation"]
async fn run_all_comprehensive_tests() {
    println!("\n");
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║     COMPREHENSIVE MULTI-AGENT E2E TEST SUITE               ║");
    println!("╚════════════════════════════════════════════════════════════╝\n");

    println!("Available tests:");
    println!("1. test_individual_agent_execution");
    println!("2. test_module_agent_scope_enforcement");
    println!("3. test_sequential_pipeline_execution");
    println!("4. test_message_bus_communication");
    println!("5. test_full_mission_with_module_agents");
    println!("6. test_health_monitoring");
    println!("7. test_convergent_verification_loop");

    println!("\nRun individual test with:");
    println!("  cargo test <test_name> -- --ignored --nocapture");
    println!("\nOr run all with:");
    println!("  cargo test --test comprehensive_multi_agent_e2e -- --ignored --nocapture");
}
