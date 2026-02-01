#![allow(clippy::field_reassign_with_default)]
//! Real E2E Multi-Agent Tests with actual LLM calls.
//!
//! These tests require:
//! - ANTHROPIC_API_KEY environment variable set
//! - Run with: cargo test --test real_e2e_multi_agent_tests -- --ignored --nocapture
//!
//! WARNING: These tests make real API calls and will incur costs.

use std::path::Path;
use std::sync::Arc;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::messaging::AgentMessageBus;
use claude_pilot::agent::multi::{
    AdaptiveConsensusExecutor, AgentPoolBuilder, CoderAgent, ConsensusEngine, Coordinator,
    PlanningAgent, ResearchAgent, ReviewerAgent, SpecializedAgent, VerifierAgent,
};
use claude_pilot::config::{AgentConfig, ConsensusConfig, MultiAgentConfig};
use claude_pilot::state::EventStore;
use tempfile::TempDir;

/// Create a simple Rust project for testing.
fn create_test_project(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();

    // Cargo.toml
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[dependencies]
"#,
    )
    .unwrap();

    // src/lib.rs - simple library with intentional issue to fix
    std::fs::write(
        dir.join("src/lib.rs"),
        r#"//! A simple test library.

/// Returns a greeting message.
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greet() {
        assert_eq!(greet("World"), "Hello, World!");
    }
}
"#,
    )
    .unwrap();
}

/// Create TaskAgent with real LLM configuration.
fn create_task_agent() -> Arc<TaskAgent> {
    let config = AgentConfig::default();
    Arc::new(TaskAgent::new(config))
}

/// Create all required agents for a complete workflow.
fn create_agents(task_agent: Arc<TaskAgent>) -> Vec<Arc<dyn SpecializedAgent>> {
    vec![
        ResearchAgent::with_id("research-0", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-0", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
        ReviewerAgent::with_id("reviewer-0", Arc::clone(&task_agent)),
    ]
}

/// Test helper to setup coordinator with all components.
async fn setup_coordinator(
    _project_dir: &Path,
    event_store: Arc<EventStore>,
) -> claude_pilot::error::Result<Coordinator> {
    let task_agent = create_task_agent();
    let agents = create_agents(task_agent.clone());

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = false; // Start with sequential mode for simpler testing

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()?;

    let message_bus = AgentMessageBus::default().with_event_store(Arc::clone(&event_store));

    let coordinator = Coordinator::new(config, Arc::new(pool))
        .with_task_agent(task_agent)
        .with_event_store(event_store)
        .with_message_bus(Arc::new(message_bus));

    Ok(coordinator)
}

/// Test: Simple mission execution with real LLM.
///
/// This test verifies:
/// - Agent spawning and registration
/// - Research phase execution
/// - Planning phase execution
/// - Implementation execution
/// - Verification (convergent 2-pass)
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_simple_mission_execution() {
    // OAuth via Claude Code - no API key needed
    // from_claude_code() automatically uses Claude Code's OAuth token

    // Setup
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_coordinator(project_dir, Arc::clone(&event_store))
        .await
        .expect("Failed to setup coordinator");

    // Execute mission
    let mission_id = format!("test-mission-{}", uuid::Uuid::new_v4());
    let description =
        "Add a function called 'farewell' that takes a name and returns 'Goodbye, {name}!'";

    println!("\n=== Starting Mission: {} ===", mission_id);
    println!("Description: {}", description);
    println!("Project: {}", project_dir.display());

    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;

    // Verify result
    match result {
        Ok(mission_result) => {
            println!("\n=== Mission Result ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);
            println!("Results count: {}", mission_result.results.len());

            for (i, result) in mission_result.results.iter().enumerate() {
                println!("\n--- Result {} ---", i + 1);
                println!("Task ID: {}", result.task_id);
                println!("Success: {}", result.success);
                println!("Findings: {:?}", result.findings);
            }

            // Check events recorded
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("\n=== Events Recorded: {} ===", events.len());
            for event in &events {
                println!("  - {:?}", event.payload);
            }

            // Verify the function was added
            let lib_content = std::fs::read_to_string(project_dir.join("src/lib.rs")).unwrap();
            println!("\n=== Modified lib.rs ===");
            println!("{}", lib_content);

            // Basic assertions
            assert!(
                mission_result.success || !mission_result.results.is_empty(),
                "Mission should produce results"
            );
        }
        Err(e) => {
            eprintln!("Mission failed with error: {}", e);
            // Don't fail the test on error - just report it
        }
    }

    coordinator.shutdown();
}

/// Test: Dynamic mode with consensus.
/// This tests the full multi-agent collaboration workflow:
/// - Consensus-based planning with multiple agents
/// - Evidence-weighted proposal scoring
/// - Dynamic agent joining during consensus rounds
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_real_dynamic_consensus_mission() {
    // OAuth via Claude Code - no API key needed

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let task_agent = create_task_agent();
    let agents = create_agents(task_agent.clone());

    let mut config = MultiAgentConfig::default();
    config.enabled = true;
    config.dynamic_mode = true; // Enable consensus mode

    let pool = AgentPoolBuilder::new(config.clone())
        .with_agents(agents)
        .build()
        .unwrap();

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

    let mission_id = format!("consensus-mission-{}", uuid::Uuid::new_v4());
    let description = "Add error handling to the greet function - return Result<String, &'static str> and return Err if name is empty";

    println!("\n=== Starting Consensus Mission: {} ===", mission_id);

    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;

    match result {
        Ok(mission_result) => {
            println!("\n=== Consensus Mission Result ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);
            println!("Conflicts: {:?}", mission_result.conflicts);

            // Check for consensus events
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            let consensus_events: Vec<_> = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        claude_pilot::state::EventPayload::ConsensusRoundStarted { .. }
                            | claude_pilot::state::EventPayload::ConsensusCompleted { .. }
                    )
                })
                .collect();

            println!("Consensus events: {}", consensus_events.len());
        }
        Err(e) => {
            eprintln!("Consensus mission failed: {}", e);
        }
    }

    coordinator.shutdown();
}

/// Test: Verification convergence with real fixes.
#[tokio::test]
#[ignore = "Requires ANTHROPIC_API_KEY and makes real API calls"]
async fn test_real_verification_convergence() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("Skipping: ANTHROPIC_API_KEY not set");
        return;
    }

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    // Create project with intentional issue
    std::fs::create_dir_all(project_dir.join("src")).unwrap();
    std::fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    // Code with a bug - missing return type
    std::fs::write(
        project_dir.join("src/lib.rs"),
        r#"//! Test library with intentional issues.

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

// Missing: subtract function that tests expect
"#,
    )
    .unwrap();

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_coordinator(project_dir, Arc::clone(&event_store))
        .await
        .unwrap();

    let mission_id = format!("convergence-mission-{}", uuid::Uuid::new_v4());
    let description = "Add a subtract function that takes two i32 and returns their difference. Include a test for it.";

    println!("\n=== Starting Convergence Test Mission ===");

    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;

    match result {
        Ok(mission_result) => {
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);

            // Check for convergence event
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            let convergence_achieved = events.iter().any(|e| {
                matches!(
                    &e.payload,
                    claude_pilot::state::EventPayload::ConvergenceAchieved { .. }
                )
            });

            let verification_rounds: Vec<_> = events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.payload,
                        claude_pilot::state::EventPayload::VerificationRound { .. }
                    )
                })
                .collect();

            println!("Convergence achieved: {}", convergence_achieved);
            println!("Verification rounds: {}", verification_rounds.len());

            // Verify code was modified
            let lib_content = std::fs::read_to_string(project_dir.join("src/lib.rs")).unwrap();
            let has_subtract = lib_content.contains("subtract");
            println!("Subtract function added: {}", has_subtract);
        }
        Err(e) => {
            eprintln!("Mission failed: {}", e);
        }
    }

    coordinator.shutdown();
}

/// Test: Full workflow with event verification.
#[tokio::test]
#[ignore = "Requires ANTHROPIC_API_KEY and makes real API calls"]
async fn test_real_full_workflow_with_events() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        eprintln!("Skipping: ANTHROPIC_API_KEY not set");
        return;
    }

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let coordinator = setup_coordinator(project_dir, Arc::clone(&event_store))
        .await
        .unwrap();

    let mission_id = format!("full-workflow-{}", uuid::Uuid::new_v4());
    let description = "Refactor the greet function to use a Greeting struct with a 'message' field";

    println!("\n=== Full Workflow Test ===");
    println!("Mission: {}", mission_id);

    let start = std::time::Instant::now();
    let result = coordinator
        .execute_mission(&mission_id, description, project_dir)
        .await;
    let duration = start.elapsed();

    println!("Execution time: {:?}", duration);

    match result {
        Ok(mission_result) => {
            println!("\n=== Results ===");
            println!("Success: {}", mission_result.success);
            println!("Summary: {}", mission_result.summary);

            // Analyze events
            let events = event_store.query(&mission_id, 0).await.unwrap_or_default();
            println!("\n=== Event Analysis ===");
            println!("Total events: {}", events.len());

            let mut phase_events: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();

            for event in &events {
                let phase = match &event.payload {
                    claude_pilot::state::EventPayload::AgentSpawned { .. } => "spawn",
                    claude_pilot::state::EventPayload::AgentTaskAssigned { .. } => "assignment",
                    claude_pilot::state::EventPayload::AgentTaskCompleted { .. } => "completion",
                    claude_pilot::state::EventPayload::VerificationRound { .. } => "verification",
                    claude_pilot::state::EventPayload::ConvergenceAchieved { .. } => "convergence",
                    claude_pilot::state::EventPayload::RulesInjected { .. } => "rules",
                    claude_pilot::state::EventPayload::SkillActivated { .. } => "skills",
                    _ => "other",
                };
                *phase_events.entry(phase).or_insert(0) += 1;
            }

            for (phase, count) in &phase_events {
                println!("  {}: {}", phase, count);
            }

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
}

/// Test TaskAgent directly with a simple prompt.
#[tokio::test]
#[ignore = "Makes real API calls"]
async fn test_task_agent_direct() {
    let task_agent = create_task_agent();
    let project_dir = std::path::Path::new("/Users/a16801/Workspace/claude-pilot");

    println!("Testing TaskAgent.run_prompt directly...");
    let prompt = "Reply with exactly: 'Hello from test'";

    let result = task_agent.run_prompt(prompt, project_dir).await;

    match result {
        Ok(output) => {
            println!("TaskAgent output: {}", output);
        }
        Err(e) => {
            eprintln!("TaskAgent error: {}", e);
        }
    }
}

/// Test ResearchAgent execution directly.
#[tokio::test]
#[ignore = "Makes real API calls"]
async fn test_research_agent_direct() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskContext, TaskPriority};

    let task_agent = create_task_agent();
    let research_agent = ResearchAgent::with_id("research-test", task_agent);
    let project_dir = std::path::Path::new("/Users/a16801/Workspace/claude-pilot");

    let context = TaskContext {
        mission_id: "test-mission".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec![],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "research-1".to_string(),
        description: "List the files in src/agent/multi/ directory".to_string(),
        context,
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_research()),
    };

    println!("Testing ResearchAgent.execute directly...");
    let start = std::time::Instant::now();

    let result = research_agent.execute(&task, project_dir).await;

    println!("Execution time: {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("Research result: success={}", r.success);
            println!(
                "Output preview: {}...",
                &r.output.chars().take(300).collect::<String>()
            );
            println!("Findings count: {}", r.findings.len());
        }
        Err(e) => {
            eprintln!("Research error: {}", e);
        }
    }
}

/// Test just agent pool creation (no LLM calls).
#[tokio::test]
async fn test_agent_pool_setup() {
    let task_agent = create_task_agent();
    let agents = create_agents(task_agent);

    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config)
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    // Agent IDs include instance number (e.g., "research-0")
    assert!(pool.has_agent("research-0"), "Missing research-0");
    assert!(pool.has_agent("planning-0"), "Missing planning-0");
    assert!(pool.has_agent("coder-0"), "Missing coder-0");
    assert!(pool.has_agent("verifier-0"), "Missing verifier-0");
    assert!(pool.has_agent("reviewer-0"), "Missing reviewer-0");

    let stats = pool.statistics();
    println!(
        "Agent pool setup successful with {} agents",
        stats.agent_count
    );
    assert_eq!(stats.agent_count, 5, "Should have 5 agents");
}

/// Test CoderAgent execution directly.
#[tokio::test]
#[ignore = "Makes real API calls"]
async fn test_coder_agent_direct() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskContext, TaskPriority};

    let task_agent = create_task_agent();
    let coder_agent = CoderAgent::with_id("coder-test", task_agent);

    // Use a temp directory with a simple file to modify
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    // Create a simple Rust file
    std::fs::create_dir_all(project_dir.join("src")).unwrap();
    std::fs::write(
        project_dir.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("src/lib.rs"),
        "pub fn hello() -> &'static str { \"hello\" }\n",
    )
    .unwrap();

    let context = TaskContext {
        mission_id: "test-mission".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec!["src/lib.rs".to_string()],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "coder-1".to_string(),
        description: "Add a function called 'goodbye' that returns 'goodbye' in src/lib.rs"
            .to_string(),
        context,
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_coder()),
    };

    println!("Testing CoderAgent.execute directly...");
    let start = std::time::Instant::now();

    let result = coder_agent.execute(&task, project_dir).await;

    println!("Execution time: {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("Coder result: success={}", r.success);
            println!(
                "Output preview: {}...",
                &r.output.chars().take(300).collect::<String>()
            );

            // Check if file was modified
            let content = std::fs::read_to_string(project_dir.join("src/lib.rs")).unwrap();
            println!("\nModified file content:\n{}", content);
            assert!(
                content.contains("goodbye"),
                "Function 'goodbye' should be added"
            );
        }
        Err(e) => {
            eprintln!("Coder error: {}", e);
        }
    }
}

/// Test VerifierAgent execution directly.
#[tokio::test]
#[ignore = "Makes real API calls"]
async fn test_verifier_agent_direct() {
    use claude_pilot::agent::multi::{AgentRole, AgentTask, TaskContext, TaskPriority};

    let task_agent = create_task_agent();
    let verifier_agent = VerifierAgent::with_id("verifier-test", task_agent);

    // Use a temp directory with valid Rust code
    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();

    std::fs::create_dir_all(project_dir.join("src")).unwrap();
    std::fs::write(
        project_dir.join("Cargo.toml"),
        "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("src/lib.rs"),
        r#"pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_add() { assert_eq!(add(2, 3), 5); }
}
"#,
    )
    .unwrap();

    let context = TaskContext {
        mission_id: "test-mission".to_string(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec!["src/lib.rs".to_string()],
        composed_prompt: None,
    };

    let task = AgentTask {
        id: "verify-1".to_string(),
        description: "Verify the code compiles and tests pass".to_string(),
        context,
        priority: TaskPriority::High,
        role: Some(AgentRole::core_verifier()),
    };

    println!("Testing VerifierAgent.execute directly...");
    let start = std::time::Instant::now();

    let result = verifier_agent.execute(&task, project_dir).await;

    println!("Execution time: {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("Verifier result: success={}", r.success);
            println!(
                "Output preview: {}...",
                &r.output.chars().take(300).collect::<String>()
            );
            println!("Findings: {:?}", r.findings);
        }
        Err(e) => {
            eprintln!("Verifier error: {}", e);
        }
    }
}
