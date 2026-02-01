#![allow(clippy::field_reassign_with_default)]
//! Comprehensive Real E2E Multi-Agent Tests with actual LLM calls via Claude Code OAuth.
//!
//! These tests verify the complete multi-agent workflow:
//! - Evidence gathering by ResearchAgent
//! - Consensus-based planning with multiple agents
//! - Message exchange between agents
//! - Implementation by CoderAgent
//! - Review by ReviewerAgent
//! - Verification by VerifierAgent
//!
//! Run with: cargo test --test real_multi_agent_e2e_comprehensive -- --ignored --nocapture
//!
//! WARNING: These tests make real API calls and will incur costs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::{
    AdaptiveConsensusExecutor, AgentId, AgentMessage, AgentMessageBus, AgentPoolBuilder, AgentTask,
    ArchitectAgent, CoderAgent, ConsensusEngine, ConsensusResult, Coordinator, MessagePayload,
    PlanningAgent, ResearchAgent, ReviewerAgent, SpecializedAgent, TaskContext, VerifierAgent,
};
use claude_pilot::config::{AgentConfig, ConsensusConfig, MultiAgentConfig};
use claude_pilot::state::{EventPayload, EventStore};
use claude_pilot::workspace::Workspace;
use parking_lot::Mutex;
use serde_json::json;
use tempfile::TempDir;

/// Test project structure for comprehensive multi-agent testing.
fn create_comprehensive_test_project(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "multi-module-test"
version = "0.1.0"
edition = "2021"

[dependencies]
thiserror = "1.0"
"#,
    )
    .unwrap();

    std::fs::create_dir_all(dir.join("src/auth")).unwrap();
    std::fs::create_dir_all(dir.join("src/api")).unwrap();
    std::fs::create_dir_all(dir.join("src/utils")).unwrap();

    std::fs::write(
        dir.join("src/lib.rs"),
        r#"//! Multi-module test library.

pub mod auth;
pub mod api;
pub mod utils;

pub use auth::{AuthError, User, authenticate};
pub use api::{ApiResponse, handle_request};
"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("src/auth/mod.rs"),
        r#"//! Authentication module.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Token expired")]
    TokenExpired,
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub email: String,
}

/// Authenticate a user with username and password.
pub fn authenticate(username: &str, _password: &str) -> Result<User, AuthError> {
    if username.is_empty() {
        return Err(AuthError::InvalidCredentials);
    }

    Ok(User {
        id: 1,
        username: username.to_string(),
        email: format!("{}@example.com", username),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authenticate_valid() {
        let result = authenticate("testuser", "password123");
        assert!(result.is_ok());
    }

    #[test]
    fn test_authenticate_empty_username() {
        let result = authenticate("", "password");
        assert!(result.is_err());
    }
}
"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("src/api/mod.rs"),
        r#"//! API handling module.

use crate::auth::User;

#[derive(Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub body: String,
}

impl ApiResponse {
    pub fn ok(body: impl Into<String>) -> Self {
        Self { status: 200, body: body.into() }
    }

    pub fn unauthorized() -> Self {
        Self { status: 401, body: "Unauthorized".to_string() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self { status: 400, body: message.into() }
    }
}

/// Handle an API request with optional authentication.
pub fn handle_request(path: &str, user: Option<&User>) -> ApiResponse {
    match path {
        "/health" => ApiResponse::ok("OK"),
        "/profile" => {
            if let Some(u) = user {
                ApiResponse::ok(format!("User: {}", u.username))
            } else {
                ApiResponse::unauthorized()
            }
        }
        _ => ApiResponse::bad_request("Unknown endpoint"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_endpoint() {
        let response = handle_request("/health", None);
        assert_eq!(response.status, 200);
    }

    #[test]
    fn test_profile_unauthorized() {
        let response = handle_request("/profile", None);
        assert_eq!(response.status, 401);
    }
}
"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("src/utils/mod.rs"),
        r#"//! Utility functions.

/// Validate an email address format.
pub fn validate_email(email: &str) -> bool {
    email.contains('@') && email.contains('.')
}

/// Hash a password (placeholder).
pub fn hash_password(password: &str) -> String {
    format!("hashed_{}", password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_email() {
        assert!(validate_email("user@example.com"));
        assert!(!validate_email("invalid"));
    }
}
"#,
    )
    .unwrap();

    println!("✓ Created comprehensive test project at: {}", dir.display());
}

/// Create a Workspace from a test project for enabling adaptive consensus.
async fn create_workspace_from_test_project(dir: &Path) -> Arc<Workspace> {
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
                "name": "multi-module-test",
                "workspace": {"workspace_type": "single_package"},
                "tech_stack": {"primary_language": "rust"},
                "languages": [{"name": "rust", "marker_files": ["Cargo.toml"]}],
                "total_files": 6
            },
            "modules": [
                {
                    "id": "auth",
                    "name": "Authentication",
                    "paths": ["src/auth/mod.rs"],
                    "key_files": ["src/auth/mod.rs"],
                    "dependencies": [],
                    "dependents": ["api"],
                    "responsibility": "User authentication and session management",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 1, "loc": 50, "cyclomatic_complexity": 1.0, "change_frequency": 0.5},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                },
                {
                    "id": "api",
                    "name": "API Gateway",
                    "paths": ["src/api/mod.rs"],
                    "key_files": ["src/api/mod.rs"],
                    "dependencies": [{"module_id": "auth", "dependency_type": "runtime"}],
                    "dependents": [],
                    "responsibility": "HTTP API handling",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 1, "loc": 40, "cyclomatic_complexity": 1.0, "change_frequency": 0.5},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                },
                {
                    "id": "utils",
                    "name": "Utilities",
                    "paths": ["src/utils/mod.rs"],
                    "key_files": ["src/utils/mod.rs"],
                    "dependencies": [],
                    "dependents": ["auth"],
                    "responsibility": "Shared utility functions",
                    "primary_language": "Rust",
                    "metrics": {"file_count": 1, "loc": 25, "cyclomatic_complexity": 1.0, "change_frequency": 0.3},
                    "conventions": [],
                    "known_issues": [],
                    "evidence": []
                }
            ],
            "groups": [
                {
                    "id": "core",
                    "name": "Core Services",
                    "module_ids": ["auth", "utils"],
                    "responsibility": "Core authentication and utilities",
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
                    "group_ids": ["gateway"],
                    "responsibility": "Infrastructure services",
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

fn create_task_agent() -> Arc<TaskAgent> {
    Arc::new(TaskAgent::new(AgentConfig::default()))
}

fn create_full_agent_set(task_agent: Arc<TaskAgent>) -> Vec<Arc<dyn SpecializedAgent>> {
    vec![
        ResearchAgent::with_id("research-0", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-1", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
        ReviewerAgent::with_id("reviewer-0", Arc::clone(&task_agent)),
        ArchitectAgent::with_id("architect-0", Arc::clone(&task_agent)),
    ]
}

async fn collect_messages(bus: Arc<AgentMessageBus>, duration: Duration) -> Vec<AgentMessage> {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let messages_clone = Arc::clone(&messages);
    let mut receiver = bus.subscribe("message-collector");

    let collect_task = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await {
                Ok(Ok(Some(msg))) => messages_clone.lock().push(msg),
                Ok(Ok(None)) => break,
                _ => continue,
            }
        }
    });

    tokio::time::sleep(duration).await;
    collect_task.abort();

    match Arc::try_unwrap(messages) {
        Ok(mutex) => mutex.into_inner(),
        Err(arc) => arc.lock().clone(),
    }
}

// ============================================================================
// Test: Full Multi-Agent Workflow with Real LLM
// ============================================================================

#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth - run with --ignored --nocapture"]
async fn test_comprehensive_multi_agent_workflow() {
    println!("\n");
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║   COMPREHENSIVE MULTI-AGENT WORKFLOW TEST                     ║");
    println!("║   Real LLM calls via Claude Code OAuth                        ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_comprehensive_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let event_store = Arc::new(EventStore::new(events_dir.join("events.db")).unwrap());

    let task_agent = create_task_agent();
    let agents = create_full_agent_set(task_agent.clone());

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = true;

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    let consensus_config = ConsensusConfig {
        max_rounds: 5,
        min_participants: 2,
        ..Default::default()
    };
    let consensus_engine = ConsensusEngine::new(task_agent.clone(), consensus_config.clone());
    let adaptive_executor = AdaptiveConsensusExecutor::new(consensus_engine, consensus_config)
        .with_event_store(Arc::clone(&event_store));

    let message_bus =
        Arc::new(AgentMessageBus::default().with_event_store(Arc::clone(&event_store)));

    // Create workspace for adaptive consensus mode
    let workspace = create_workspace_from_test_project(project_dir).await;

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(task_agent)
        .with_workspace(workspace) // Enable adaptive consensus
        .with_adaptive_executor(adaptive_executor)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::clone(&message_bus));

    // Verify adaptive mode is enabled
    assert!(
        coordinator.hierarchy().is_some(),
        "Workspace must be configured for adaptive consensus"
    );

    let mission_id = format!("comprehensive-test-{}", uuid::Uuid::new_v4());
    let description = r#"
Implement proper password validation in the authentication module:

1. Add a `validate_password` function in src/utils/mod.rs that:
   - Checks minimum length (8 characters)
   - Requires at least one uppercase letter
   - Requires at least one digit
   - Returns Result<(), &'static str> with appropriate error messages

2. Update the `authenticate` function in src/auth/mod.rs to:
   - Use the password validation before authenticating
   - Return a new AuthError::WeakPassword variant for invalid passwords

3. Add appropriate tests for all new functionality
"#;

    println!("📋 Mission ID: {}", mission_id);
    println!("📝 Description: {}", description.trim());
    println!();

    let bus_clone = Arc::clone(&message_bus);
    let message_collection =
        tokio::spawn(async move { collect_messages(bus_clone, Duration::from_secs(120)).await });

    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 1: Starting Multi-Agent Mission Execution");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let elapsed = start.elapsed();

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 2: Analyzing Results (elapsed: {:?})", elapsed);
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let messages = message_collection.await.unwrap_or_default();

    match result {
        Ok(mission_result) => {
            println!("✅ Mission Completed!");
            println!("   Success: {}", mission_result.success);
            println!("   Summary: {}", mission_result.summary);
            println!("   Results count: {}", mission_result.results.len());

            for (i, res) in mission_result.results.iter().enumerate() {
                println!();
                println!("   --- Agent Result {} ---", i + 1);
                println!("   Task ID: {}", res.task_id);
                println!("   Success: {}", res.success);
                if !res.findings.is_empty() {
                    println!("   Findings:");
                    for finding in &res.findings {
                        println!("     • {}", finding);
                    }
                }
            }

            if !mission_result.conflicts.is_empty() {
                println!();
                println!("   ⚠️ Conflicts encountered:");
                for conflict in &mission_result.conflicts {
                    println!("     - Topic: {}", conflict.topic);
                    println!("       Agents: {:?}", conflict.agents);
                }
            }
        }
        Err(e) => println!("❌ Mission Failed: {}", e),
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 3: Analyzing Message Bus Communication");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    println!("📨 Total messages exchanged: {}", messages.len());

    let mut message_types: HashMap<String, usize> = HashMap::new();
    for msg in &messages {
        *message_types
            .entry(format!("{:?}", msg.message_type()))
            .or_insert(0) += 1;
    }

    println!("   Message type breakdown:");
    for (msg_type, count) in &message_types {
        println!("     {}: {}", msg_type, count);
    }

    if !messages.is_empty() {
        println!();
        println!("   Sample messages (first 5):");
        for (i, msg) in messages.iter().take(5).enumerate() {
            println!(
                "     [{}] {} → {}: {:?}",
                i + 1,
                msg.from,
                msg.to,
                msg.message_type()
            );
        }
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 4: Analyzing Event Store");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
    println!("📊 Total events recorded: {}", events.len());

    let mut consensus_events = 0;
    let mut hierarchical_events = 0;
    let mut tier_events = 0;
    let mut agent_events = 0;
    let mut task_events = 0;
    let mut verification_events = 0;

    for event in &events {
        match &event.payload {
            EventPayload::ConsensusRoundStarted { .. }
            | EventPayload::ConsensusVoteReceived { .. }
            | EventPayload::ConsensusRoundCompleted { .. }
            | EventPayload::ConsensusCompleted { .. } => consensus_events += 1,
            EventPayload::HierarchicalConsensusStarted { .. }
            | EventPayload::HierarchicalConsensusCompleted { .. } => hierarchical_events += 1,
            EventPayload::TierConsensusStarted { .. }
            | EventPayload::TierConsensusCompleted { .. } => tier_events += 1,
            EventPayload::AgentTaskAssigned { .. }
            | EventPayload::AgentTaskCompleted { .. }
            | EventPayload::AgentMessageSent { .. }
            | EventPayload::AgentMessageReceived { .. } => agent_events += 1,
            EventPayload::TaskStarted { .. }
            | EventPayload::TaskCompleted { .. }
            | EventPayload::TaskFailed { .. } => task_events += 1,
            EventPayload::VerificationRound { .. } | EventPayload::ConvergenceAchieved { .. } => {
                verification_events += 1
            }
            _ => {}
        }
    }

    println!("   Event breakdown:");
    println!("     Consensus events: {}", consensus_events);
    println!(
        "     Hierarchical consensus events: {}",
        hierarchical_events
    );
    println!("     Tier consensus events: {}", tier_events);
    println!("     Agent events: {}", agent_events);
    println!("     Task events: {}", task_events);
    println!("     Verification events: {}", verification_events);

    // Verify hierarchical consensus was used (not sequential fallback)
    if hierarchical_events > 0 {
        println!();
        println!("   ✅ Hierarchical consensus mode was used");
    } else {
        println!();
        println!("   ⚠️ Sequential mode was used (no hierarchical events)");
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 5: Verifying Code Changes");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let utils_content =
        std::fs::read_to_string(project_dir.join("src/utils/mod.rs")).unwrap_or_default();
    let auth_content =
        std::fs::read_to_string(project_dir.join("src/auth/mod.rs")).unwrap_or_default();

    println!("📁 Utils module (src/utils/mod.rs):");
    println!(
        "   Contains validate_password: {}",
        utils_content.contains("validate_password")
    );
    println!();
    println!("📁 Auth module (src/auth/mod.rs):");
    println!(
        "   Contains WeakPassword error: {}",
        auth_content.contains("WeakPassword")
    );
    println!(
        "   Uses validate_password: {}",
        auth_content.contains("validate_password")
    );

    println!();
    println!("🔧 Running cargo check...");
    let check_result = std::process::Command::new("cargo")
        .args(["check"])
        .current_dir(project_dir)
        .output();

    match check_result {
        Ok(output) if output.status.success() => println!("   ✅ Code compiles successfully!"),
        Ok(output) => {
            println!("   ⚠️ Compilation warnings/errors:");
            println!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Err(e) => println!("   ❌ Failed to run cargo check: {}", e),
    }

    println!();
    println!("🧪 Running cargo test...");
    let test_result = std::process::Command::new("cargo")
        .args(["test", "--", "--nocapture"])
        .current_dir(project_dir)
        .output();

    match test_result {
        Ok(output) if output.status.success() => println!("   ✅ All tests pass!"),
        Ok(output) => {
            println!("   ⚠️ Some tests failed:");
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Err(e) => println!("   ❌ Failed to run cargo test: {}", e),
    }

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  TEST COMPLETE");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    println!("📊 Test Summary:");
    println!("   - Events recorded: {}", events.len());
    println!("   - Messages exchanged: {}", messages.len());
    println!("   - Consensus events: {}", consensus_events);
    println!("   - Execution time: {:?}", elapsed);

    coordinator.shutdown();
}

// ============================================================================
// Test: Direct Agent Task Execution with Message Exchange
// ============================================================================

#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth - run with --ignored --nocapture"]
async fn test_agent_direct_execution_with_messaging() {
    println!("\n");
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║   DIRECT AGENT EXECUTION WITH MESSAGING TEST                  ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_comprehensive_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let event_store = Arc::new(EventStore::new(events_dir.join("events.db")).unwrap());

    let task_agent = create_task_agent();
    let message_bus =
        Arc::new(AgentMessageBus::default().with_event_store(Arc::clone(&event_store)));

    let research_agent = ResearchAgent::with_id("research-0", Arc::clone(&task_agent));
    let coder_agent = CoderAgent::with_id("coder-0", Arc::clone(&task_agent));

    let mut coordinator_rx = message_bus.subscribe("coordinator");

    let context = TaskContext::new("test-mission", project_dir.to_path_buf());

    // Phase 1: Research
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 1: Research Agent Evidence Gathering");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let research_task = AgentTask::new(
        "research-1",
        "Analyze the authentication module in src/auth/mod.rs. Identify security concerns and improvement opportunities."
    ).with_context(context.clone());

    println!("📋 Research Task: {}", research_task.description);
    println!();

    let start = std::time::Instant::now();
    let research_result = research_agent.execute(&research_task, project_dir).await;
    println!("   Execution time: {:?}", start.elapsed());

    match research_result {
        Ok(result) => {
            println!("   ✅ Research completed successfully");
            println!("   Findings ({}):", result.findings.len());
            for finding in &result.findings {
                println!("     • {}", finding);
            }

            let findings_msg = AgentMessage::broadcast(
                "research-0",
                MessagePayload::EvidenceShare {
                    evidence: result.findings.clone(),
                    quality_score: 0.85,
                },
            );
            message_bus.try_send(findings_msg).ok();
        }
        Err(e) => println!("   ❌ Research failed: {}", e),
    }

    // Phase 2: Implementation
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  PHASE 2: Coder Agent Implementation");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let coder_task = AgentTask::new(
        "coder-1",
        "Add a WeakPassword error variant to AuthError enum in src/auth/mod.rs and create a password validation helper function."
    ).with_context(context);

    println!("📋 Coder Task: {}", coder_task.description);
    println!();

    let start = std::time::Instant::now();
    let coder_result = coder_agent.execute(&coder_task, project_dir).await;
    println!("   Execution time: {:?}", start.elapsed());

    match coder_result {
        Ok(result) => {
            println!("   ✅ Implementation completed successfully");
            println!("   Artifacts ({}):", result.artifacts.len());
            for artifact in &result.artifacts {
                println!("     • {} ({:?})", artifact.name, artifact.artifact_type);
            }

            let completion_msg = AgentMessage::new(
                "coder-0",
                "coordinator",
                MessagePayload::TaskResult { result },
            );
            message_bus.try_send(completion_msg).ok();
        }
        Err(e) => println!("   ❌ Implementation failed: {}", e),
    }

    // Message analysis
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  MESSAGE BUS ANALYSIS");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let mut received_messages = Vec::new();
    while let Ok(Some(msg)) = coordinator_rx.try_recv() {
        received_messages.push(msg);
    }

    println!(
        "📨 Messages received by coordinator: {}",
        received_messages.len()
    );
    for msg in &received_messages {
        println!("   {} → {}: {:?}", msg.from, msg.to, msg.message_type());
    }

    // Verify changes
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  CODE VERIFICATION");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let auth_content =
        std::fs::read_to_string(project_dir.join("src/auth/mod.rs")).unwrap_or_default();

    println!("📁 Auth module changes:");
    println!(
        "   WeakPassword added: {}",
        auth_content.contains("WeakPassword")
    );

    println!();
    println!("📄 Modified src/auth/mod.rs:");
    println!("────────────────────────────────────────────────────────────────");
    for (i, line) in auth_content.lines().take(50).enumerate() {
        println!("{:4} │ {}", i + 1, line);
    }
    if auth_content.lines().count() > 50 {
        println!("     │ ... (truncated)");
    }
    println!("────────────────────────────────────────────────────────────────");

    println!();
    println!("✅ Direct agent execution test complete");
}

// ============================================================================
// Test: Consensus-Based Multi-Agent Planning
// ============================================================================

#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth - run with --ignored --nocapture"]
async fn test_consensus_based_planning() {
    println!("\n");
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║   CONSENSUS-BASED MULTI-AGENT PLANNING TEST                   ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_comprehensive_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let event_store = Arc::new(EventStore::new(events_dir.join("events.db")).unwrap());

    let task_agent = create_task_agent();

    let research_agent = ResearchAgent::with_id("research-0", Arc::clone(&task_agent));
    let planning_agent = PlanningAgent::with_id("planning-0", Arc::clone(&task_agent));
    let coder_agent = CoderAgent::with_id("coder-0", Arc::clone(&task_agent));

    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config.clone())
        .with_agent(research_agent)
        .with_agent(planning_agent)
        .with_agent(coder_agent)
        .build()
        .unwrap();

    let consensus_config = ConsensusConfig {
        max_rounds: 3,
        min_participants: 2,
        ..Default::default()
    };

    let consensus_engine = ConsensusEngine::new(task_agent.clone(), consensus_config);
    let message_bus =
        Arc::new(AgentMessageBus::default().with_event_store(Arc::clone(&event_store)));

    let context =
        TaskContext::new("consensus-test", project_dir.to_path_buf()).with_findings(vec![
            "Auth module lacks password complexity validation".to_string(),
            "API module doesn't validate input data".to_string(),
        ]);

    let participant_role_ids = vec![
        AgentId::new("research-0"),
        AgentId::new("planning-0"),
        AgentId::new("coder-0"),
    ];

    println!(
        "📋 Running consensus with {} participants",
        participant_role_ids.len()
    );
    println!("   Participants: {:?}", participant_role_ids);
    println!();

    let collected = Arc::new(Mutex::new(Vec::new()));
    let collected_clone = Arc::clone(&collected);
    let mut collector_rx = message_bus.subscribe("collector");

    let collector_task = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_millis(100), collector_rx.recv()).await {
                Ok(Ok(Some(msg))) => collected_clone.lock().push(msg),
                _ => tokio::task::yield_now().await,
            }
        }
    });

    println!("═══════════════════════════════════════════════════════════════");
    println!("  RUNNING CONSENSUS PROTOCOL");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let start = std::time::Instant::now();
    let result = consensus_engine
        .run_with_dynamic_joining(
            "Implement comprehensive input validation for the authentication and API modules",
            &context,
            &participant_role_ids,
            &pool,
            |_| vec![],
        )
        .await;
    let elapsed = start.elapsed();

    collector_task.abort();
    let messages = collected.lock().clone();

    println!("   Execution time: {:?}", elapsed);
    println!("   Messages exchanged: {}", messages.len());
    println!();

    match result {
        Ok(ConsensusResult::Agreed {
            rounds,
            respondent_count,
            plan,
            tasks,
        }) => {
            println!("✅ Consensus Reached!");
            println!("   Rounds: {}", rounds);
            println!("   Respondents: {}", respondent_count);
            println!("   Tasks generated: {}", tasks.len());
            println!();
            println!("📋 Agreed Plan (first 20 lines):");
            for line in plan.lines().take(20) {
                println!("   {}", line);
            }
            println!();
            println!("📋 Generated Tasks:");
            for task in &tasks {
                println!(
                    "   - [{}] {} (module: {:?})",
                    task.id, task.description, task.assigned_module
                );
            }
        }
        Ok(ConsensusResult::PartialAgreement {
            plan,
            dissents,
            unresolved_conflicts,
            respondent_count,
        }) => {
            println!("📝 Partial Agreement");
            println!("   Respondents: {}", respondent_count);
            println!("   Dissents: {}", dissents.len());
            println!("   Unresolved conflicts: {}", unresolved_conflicts.len());
            println!();
            println!("📋 Plan (first 10 lines):");
            for line in plan.lines().take(10) {
                println!("   {}", line);
            }
        }
        Ok(ConsensusResult::NoConsensus {
            summary,
            blocking_conflicts,
            respondent_count,
        }) => {
            println!("⚠️ No Consensus Reached");
            println!("   Respondents: {}", respondent_count);
            println!("   Summary: {}", summary);
            println!("   Blocking conflicts: {}", blocking_conflicts.len());
            for conflict in &blocking_conflicts {
                println!("   - {}: {:?}", conflict.topic, conflict.agents);
            }
        }
        Err(e) => println!("❌ Consensus failed: {}", e),
    }

    // Event analysis
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  EVENT ANALYSIS");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let events = event_store
        .query("consensus-test", 0)
        .await
        .unwrap_or_default();
    println!("📊 Events recorded: {}", events.len());

    let consensus_starts = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ConsensusRoundStarted { .. }))
        .count();
    let votes = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ConsensusVoteReceived { .. }))
        .count();
    let completions = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ConsensusRoundCompleted { .. }))
        .count();

    println!("   Rounds started: {}", consensus_starts);
    println!("   Votes received: {}", votes);
    println!("   Rounds completed: {}", completions);

    println!();
    println!("✅ Consensus-based planning test complete");
}

// ============================================================================
// Test: Health Monitoring
// ============================================================================

#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth - run with --ignored --nocapture"]
async fn test_health_monitoring() {
    println!("\n");
    println!("╔════════════════════════════════════════════════════════════════╗");
    println!("║   HEALTH MONITORING TEST                                      ║");
    println!("╚════════════════════════════════════════════════════════════════╝");
    println!();

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_comprehensive_test_project(project_dir);

    let task_agent = create_task_agent();
    let agents = create_full_agent_set(task_agent.clone());

    let config = MultiAgentConfig::default();
    let pool = Arc::new(
        AgentPoolBuilder::new(config)
            .with_agents(agents)
            .build()
            .unwrap(),
    );

    use claude_pilot::agent::multi::health::HealthMonitor;
    let health_monitor = HealthMonitor::new(Arc::clone(&pool));

    println!("═══════════════════════════════════════════════════════════════");
    println!("  INITIAL HEALTH CHECK");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let report = health_monitor.check_health();
    println!("📊 Health Status: {:?}", report.status);
    println!("   Overall score: {:.2}", report.overall_score);

    if !report.role_health.is_empty() {
        println!();
        println!("   Role Health:");
        for (role, health) in &report.role_health {
            println!(
                "     {:?}: {} agents, status: {:?}",
                role, health.agent_count, health.status
            );
        }
    }

    if !report.alerts.is_empty() {
        println!();
        println!("   ⚠️ Alerts:");
        for alert in &report.alerts {
            println!("     - {:?}: {}", alert.severity, alert.message);
        }
    }

    // Execute a task to see how health changes
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  EXECUTING SAMPLE TASK");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let context = TaskContext::new("health-test", project_dir.to_path_buf());
    let coder = CoderAgent::with_id("coder-health", Arc::clone(&task_agent));

    let task =
        AgentTask::new("health-task-1", "Analyze the auth module structure").with_context(context);

    println!("📋 Executing task: {}", task.description);
    let start = std::time::Instant::now();
    let result = coder.execute(&task, project_dir).await;
    println!("   Execution time: {:?}", start.elapsed());

    match result {
        Ok(res) => println!("   ✅ Task completed, findings: {}", res.findings.len()),
        Err(e) => println!("   ❌ Task failed: {}", e),
    }

    // Check health again
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  POST-TASK HEALTH CHECK");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    let report = health_monitor.check_health();
    println!("📊 Health Status: {:?}", report.status);
    println!("   Overall score: {:.2}", report.overall_score);

    println!();
    println!("✅ Health monitoring test complete");
}
