#![allow(clippy::field_reassign_with_default)]
//! Consensus Protocol E2E Tests
//!
//! Tests the multi-agent consensus protocol with real LLM calls.
//!
//! Run with: cargo test --test consensus_protocol_e2e -- --ignored --nocapture

use std::path::Path;
use std::sync::Arc;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::consensus::{AgentProposal, ConsensusEngine, ConsensusResult};
use claude_pilot::agent::multi::messaging::AgentMessageBus;
use claude_pilot::agent::multi::shared::AgentId;
use claude_pilot::agent::multi::{
    AgentPoolBuilder, AgentRole, AgentTask, CoderAgent, PlanningAgent, ResearchAgent,
    ReviewerAgent, SpecializedAgent, TaskContext, TaskPriority, VerifierAgent,
};
use claude_pilot::config::{AgentConfig, ConsensusConfig, MultiAgentConfig};
use claude_pilot::state::EventStore;
use tempfile::TempDir;

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

/// Create a simple test project.
fn create_test_project(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        r#"[package]
name = "consensus-test"
version = "0.1.0"
edition = "2021"
"#,
    )
    .unwrap();

    std::fs::write(
        dir.join("src/lib.rs"),
        r#"//! Simple test library.

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#,
    )
    .unwrap();
}

/// Test 1: Direct ConsensusEngine execution with real LLM proposals.
///
/// This test directly exercises the consensus engine with agent proposals.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_consensus_engine_with_proposals() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Consensus Engine with Real LLM Proposals");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let task_agent = create_task_agent();
    let message_bus =
        Arc::new(AgentMessageBus::new(256).with_event_store(Arc::clone(&event_store)));

    // Create ConsensusEngine with event store and message bus
    let consensus_config = ConsensusConfig {
        max_rounds: 3,
        convergence_threshold: 0.6,
        ..Default::default()
    };

    let consensus_engine = ConsensusEngine::new(Arc::clone(&task_agent), consensus_config)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::clone(&message_bus));

    // First, let agents gather evidence
    println!("--- Phase 1: Gathering Agent Proposals via LLM ---");

    let context = TaskContext {
        mission_id: "consensus-test-1".into(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec!["src/lib.rs".into()],
        composed_prompt: None,
    };

    // Create research task to gather initial proposal
    let research_agent = ResearchAgent::with_id("research-0", Arc::clone(&task_agent));
    let research_task = AgentTask {
        id: "research-proposal".into(),
        description: "Analyze src/lib.rs and propose how to add a subtract function. \
                       Provide a specific implementation proposal."
            .into(),
        context: context.clone(),
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_research()),
    };

    println!("  Running Research Agent to gather proposal...");
    let start = std::time::Instant::now();
    let research_result = research_agent.execute(&research_task, project_dir).await;
    println!("  Research completed in {:?}", start.elapsed());

    let research_proposal = match research_result {
        Ok(r) => {
            println!("  Research Success: {}", r.success);
            r.output
        }
        Err(e) => {
            println!("  Research Error: {}", e);
            "Add subtract function: pub fn subtract(a: i32, b: i32) -> i32 { a - b }".into()
        }
    };

    // Create coder task to get implementation proposal
    let coder_agent = CoderAgent::with_id("coder-0", Arc::clone(&task_agent));
    let coder_task = AgentTask {
        id: "coder-proposal".into(),
        description: "Propose how to add a subtract function to src/lib.rs. \
                       Provide the exact code that should be added."
            .into(),
        context: context.clone(),
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_coder()),
    };

    println!("\n  Running Coder Agent to gather proposal...");
    let start = std::time::Instant::now();
    let coder_result = coder_agent.execute(&coder_task, project_dir).await;
    println!("  Coder completed in {:?}", start.elapsed());

    let coder_proposal = match coder_result {
        Ok(r) => {
            println!("  Coder Success: {}", r.success);
            r.output
        }
        Err(e) => {
            println!("  Coder Error: {}", e);
            "Add: pub fn subtract(a: i32, b: i32) -> i32 { a - b }".into()
        }
    };

    // Now run consensus with collected proposals
    println!("\n--- Phase 2: Running Consensus Protocol ---");

    // NOTE: AgentPool registers agents by role_id (e.g., "research", "coder"),
    // NOT by individual agent_id (e.g., "research-0", "coder-0").
    // So participant_ids must use role_id for consensus to find agents in the pool.
    let proposals = [
        AgentProposal {
            agent_id: "research".into(), // Use role_id, not agent_id
            role: AgentRole::core_research(),
            content: research_proposal.chars().take(500).collect(),
        },
        AgentProposal {
            agent_id: "coder".into(), // Use role_id, not agent_id
            role: AgentRole::core_coder(),
            content: coder_proposal.chars().take(500).collect(),
        },
    ];

    println!("  Proposals collected: {}", proposals.len());
    for (i, p) in proposals.iter().enumerate() {
        println!(
            "  Proposal {}: {} ({} chars)",
            i + 1,
            p.agent_id,
            p.content.len()
        );
    }

    // Create agent pool for consensus
    let agents = create_core_agents(Arc::clone(&task_agent));
    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config)
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    // Use role_id (not agent_id) to match how AgentPool stores agents
    let participant_ids: Vec<AgentId> = vec![AgentId::new("research"), AgentId::new("coder")];
    println!("\n  Pool registered with role_ids: {:?}", participant_ids);

    println!("\n  Running consensus with dynamic joining...");
    let start = std::time::Instant::now();

    let consensus_result = consensus_engine
        .run_with_dynamic_joining(
            "Add subtract function to the library",
            &context,
            &participant_ids,
            &pool,
            |_requests| vec![], // No additional agents for this test
        )
        .await;

    println!("  Consensus completed in {:?}", start.elapsed());

    match consensus_result {
        Ok(result) => {
            println!("\n=== CONSENSUS RESULT ===");
            match result {
                ConsensusResult::Agreed {
                    plan,
                    tasks,
                    rounds,
                    respondent_count,
                } => {
                    println!("✓ CONSENSUS REACHED!");
                    println!("  Rounds: {}", rounds);
                    println!("  Respondents: {}", respondent_count);
                    println!("  Tasks generated: {}", tasks.len());
                    println!(
                        "  Plan preview: {}...",
                        plan.chars().take(300).collect::<String>()
                    );
                }
                ConsensusResult::PartialAgreement {
                    plan,
                    dissents,
                    unresolved_conflicts,
                    respondent_count,
                } => {
                    println!("△ PARTIAL AGREEMENT");
                    println!("  Respondents: {}", respondent_count);
                    println!("  Dissents: {:?}", dissents);
                    println!("  Unresolved conflicts: {}", unresolved_conflicts.len());
                    println!(
                        "  Plan preview: {}...",
                        plan.chars().take(300).collect::<String>()
                    );
                }
                ConsensusResult::NoConsensus {
                    summary,
                    blocking_conflicts,
                    respondent_count,
                } => {
                    println!("✗ NO CONSENSUS");
                    println!("  Respondents: {}", respondent_count);
                    println!("  Blocking conflicts: {}", blocking_conflicts.len());
                    println!("  Summary: {}", summary);
                }
            }
        }
        Err(e) => {
            println!("Consensus failed with error: {}", e);
        }
    }

    // Query and display events
    println!("\n=== EVENT SOURCING ANALYSIS ===");
    let events = event_store
        .query("consensus-test-1", 0)
        .await
        .unwrap_or_default();
    println!("Total events recorded: {}", events.len());

    let mut consensus_events = 0;
    let mut vote_events = 0;
    let mut conflict_events = 0;
    let mut proposal_events = 0;
    let mut agent_events = 0;

    println!("\n--- All Events (chronological) ---");
    for (i, event) in events.iter().enumerate() {
        let event_type = event.payload.event_type();
        println!("  [{}] {} - {}", i + 1, event_type, event.timestamp);

        // Count by category using lowercase matching
        if event_type.contains("consensus") {
            consensus_events += 1;
        }
        if event_type.contains("vote") {
            vote_events += 1;
        }
        if event_type.contains("conflict") {
            conflict_events += 1;
        }
        if event_type.contains("proposal") {
            proposal_events += 1;
        }
        if event_type.contains("agent") {
            agent_events += 1;
        }
    }

    println!("\n--- Event Summary ---");
    println!("  Total events: {}", events.len());
    println!("  Consensus events: {}", consensus_events);
    println!("  Vote events: {}", vote_events);
    println!("  Proposal events: {}", proposal_events);
    println!("  Conflict events: {}", conflict_events);
    println!("  Agent events: {}", agent_events);

    println!("\n✓ Consensus protocol test complete");
}

/// Test 2: Multi-round consensus with voting.
///
/// Tests multiple rounds of consensus with evidence-weighted voting.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_multi_round_consensus_voting() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Multi-Round Consensus with Voting");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let task_agent = create_task_agent();
    let message_bus =
        Arc::new(AgentMessageBus::new(256).with_event_store(Arc::clone(&event_store)));

    // Configure for multiple rounds
    let consensus_config = ConsensusConfig {
        max_rounds: 5,
        convergence_threshold: 0.8, // Higher threshold to potentially trigger multiple rounds
        ..Default::default()
    };

    let consensus_engine = ConsensusEngine::new(Arc::clone(&task_agent), consensus_config)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::clone(&message_bus));

    let context = TaskContext {
        mission_id: "multi-round-consensus".into(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec!["src/lib.rs".into()],
        composed_prompt: None,
    };

    // Create multiple agents with different perspectives
    let agents = create_core_agents(Arc::clone(&task_agent));
    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config)
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    // Use role IDs (NOT agent IDs) - AgentPool stores by role_id
    let participant_ids: Vec<AgentId> = vec![
        AgentId::new("research"),
        AgentId::new("planning"),
        AgentId::new("coder"),
        AgentId::new("reviewer"),
    ];

    println!("Participant roles: {:?}", participant_ids);
    println!("Topic: Implement error handling for the add function");

    println!("\n--- Running Multi-Round Consensus ---");
    let start = std::time::Instant::now();

    let result = consensus_engine
        .run_with_dynamic_joining(
            "Add error handling to the add function - return Result<i32, &'static str> \
             and return Err if either argument is negative",
            &context,
            &participant_ids,
            &pool,
            |_| vec![],
        )
        .await;

    println!("Completed in {:?}", start.elapsed());

    match result {
        Ok(r) => {
            println!("\n=== RESULT ===");
            match r {
                ConsensusResult::Agreed {
                    rounds,
                    respondent_count,
                    plan,
                    tasks,
                } => {
                    println!(
                        "✓ Agreed after {} rounds with {} participants",
                        rounds, respondent_count
                    );
                    println!("Tasks: {}", tasks.len());
                    println!("Plan: {}...", plan.chars().take(200).collect::<String>());
                }
                ConsensusResult::PartialAgreement {
                    respondent_count,
                    dissents,
                    ..
                } => {
                    println!("△ Partial agreement with {} participants", respondent_count);
                    println!("Dissents: {:?}", dissents);
                }
                ConsensusResult::NoConsensus {
                    summary,
                    respondent_count,
                    ..
                } => {
                    println!(
                        "✗ No consensus with {} participants: {}",
                        respondent_count, summary
                    );
                }
            }
        }
        Err(e) => println!("Error: {}", e),
    }

    // Analyze events
    println!("\n=== EVENT ANALYSIS ===");
    let events = event_store
        .query("multi-round-consensus", 0)
        .await
        .unwrap_or_default();
    println!("Total events: {}", events.len());

    let mut by_type: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for event in &events {
        let t = event.payload.event_type().to_string();
        *by_type.entry(t).or_insert(0) += 1;
    }

    println!("\nEvents by type:");
    for (t, count) in by_type.iter() {
        println!("  {}: {}", t, count);
    }

    // Show consensus-specific events in detail
    println!("\nDetailed consensus events:");
    for event in events
        .iter()
        .filter(|e| e.payload.event_type().contains("consensus"))
    {
        println!("  {:?}", event.payload);
    }

    println!("\n✓ Multi-round consensus test complete");
}

/// Test 3: Consensus with message bus communication.
///
/// Verifies that agents communicate via the message bus during consensus.
/// Uses role IDs (not agent IDs) to match pool lookup behavior.
#[tokio::test]
#[ignore = "Makes real API calls via Claude Code OAuth"]
async fn test_consensus_with_message_bus() {
    println!("\n{}", "=".repeat(60));
    println!("TEST: Consensus with Message Bus Communication");
    println!("{}\n", "=".repeat(60));

    let temp_dir = TempDir::new().unwrap();
    let project_dir = temp_dir.path();
    create_test_project(project_dir);

    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    let event_store = Arc::new(EventStore::new(&db_path).unwrap());

    let task_agent = create_task_agent();
    let message_bus =
        Arc::new(AgentMessageBus::new(512).with_event_store(Arc::clone(&event_store)));

    // Subscribe to see messages BEFORE consensus starts
    let mut coordinator_receiver = message_bus.subscribe("coordinator");

    let consensus_config = ConsensusConfig::default();
    let consensus_engine = ConsensusEngine::new(Arc::clone(&task_agent), consensus_config)
        .with_event_store(Arc::clone(&event_store))
        .with_message_bus(Arc::clone(&message_bus));

    let context = TaskContext {
        mission_id: "msg-bus-consensus".into(),
        working_dir: project_dir.to_path_buf(),
        key_findings: vec![],
        blockers: vec![],
        related_files: vec!["src/lib.rs".into()],
        composed_prompt: None,
    };

    let agents = create_core_agents(Arc::clone(&task_agent));
    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config)
        .with_agents(agents)
        .build()
        .expect("Failed to build agent pool");

    // Use role IDs (NOT agent IDs) - this matches how AgentPool stores agents
    // The pool uses role_id as key, so "research", "coder", "planning" are valid
    let participant_role_ids: Vec<AgentId> = vec![
        AgentId::new("research"),
        AgentId::new("coder"),
        AgentId::new("planning"),
    ];
    println!("Participant roles: {:?}", participant_role_ids);

    // Spawn a task to collect messages with proper synchronization
    let collected_messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let collector_messages = Arc::clone(&collected_messages);

    let message_collector = tokio::spawn(async move {
        let timeout = std::time::Duration::from_secs(180);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                coordinator_receiver.recv(),
            )
            .await
            {
                Ok(Ok(Some(msg))) => {
                    println!(
                        "  📨 Message received: {} -> {} ({:?})",
                        msg.from,
                        msg.to,
                        msg.message_type()
                    );
                    collector_messages.lock().unwrap().push(msg);
                }
                Ok(Ok(None)) => break,    // Channel closed
                Ok(Err(_)) | Err(_) => {} // Timeout or error, continue
            }
        }
    });

    println!("\n--- Running Consensus with 3 Roles ---");
    println!("This demonstrates: Research, Coder, Planning roles reaching consensus\n");
    let start = std::time::Instant::now();

    let result = consensus_engine
        .run_with_dynamic_joining(
            "Add a multiply function to the library with proper documentation",
            &context,
            &participant_role_ids,
            &pool,
            |_| vec![],
        )
        .await;

    println!("\nConsensus completed in {:?}", start.elapsed());

    // Give message collector time to process remaining messages
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    message_collector.abort();

    // Get collected messages
    let messages = collected_messages.lock().unwrap().clone();

    // Display collected messages
    println!("\n=== MESSAGE BUS COMMUNICATION ===");
    println!("Messages collected during consensus: {}", messages.len());
    for (i, msg) in messages.iter().enumerate() {
        println!(
            "  [{}] {} -> {}: {:?}",
            i + 1,
            msg.from,
            msg.to,
            msg.message_type()
        );
    }

    // Categorize messages by type
    let consensus_requests: Vec<_> = messages
        .iter()
        .filter(|m| {
            matches!(
                m.message_type(),
                claude_pilot::agent::multi::MessageType::ConsensusRequest
            )
        })
        .collect();
    let consensus_votes: Vec<_> = messages
        .iter()
        .filter(|m| {
            matches!(
                m.message_type(),
                claude_pilot::agent::multi::MessageType::ConsensusVote
            )
        })
        .collect();
    let conflict_alerts: Vec<_> = messages
        .iter()
        .filter(|m| {
            matches!(
                m.message_type(),
                claude_pilot::agent::multi::MessageType::ConflictAlert
            )
        })
        .collect();

    println!("\nMessage Type Summary:");
    println!("  ConsensusRequest: {}", consensus_requests.len());
    println!("  ConsensusVote: {}", consensus_votes.len());
    println!("  ConflictAlert: {}", conflict_alerts.len());

    match result {
        Ok(r) => {
            println!("\n=== CONSENSUS RESULT ===");
            match r {
                ConsensusResult::Agreed {
                    rounds,
                    respondent_count,
                    plan,
                    tasks,
                } => {
                    println!(
                        "✓ AGREED in {} rounds with {} participants",
                        rounds, respondent_count
                    );
                    println!("  Tasks generated: {}", tasks.len());
                    println!("  Plan: {}...", plan.chars().take(200).collect::<String>());
                }
                ConsensusResult::PartialAgreement {
                    respondent_count,
                    dissents,
                    ..
                } => {
                    println!("△ PARTIAL AGREEMENT with {} participants", respondent_count);
                    println!("  Dissents: {:?}", dissents);
                }
                ConsensusResult::NoConsensus {
                    summary,
                    respondent_count,
                    ..
                } => {
                    println!("✗ NO CONSENSUS with {} participants", respondent_count);
                    println!("  Summary: {}", summary);
                }
            }
        }
        Err(e) => println!("Error: {}", e),
    }

    // Check events for full activity
    println!("\n=== EVENT SOURCING ANALYSIS ===");
    let events = event_store
        .query("msg-bus-consensus", 0)
        .await
        .unwrap_or_default();

    println!("Total events recorded: {}", events.len());

    // Count by type
    let mut event_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for event in &events {
        *event_counts.entry(event.payload.event_type()).or_insert(0) += 1;
    }

    println!("\nEvents by type:");
    let mut sorted_counts: Vec<_> = event_counts.iter().collect();
    sorted_counts.sort_by(|a, b| b.1.cmp(a.1));
    for (event_type, count) in sorted_counts {
        println!("  {}: {}", event_type, count);
    }

    // Check if message events were recorded
    let message_event_count = events
        .iter()
        .filter(|e| e.payload.event_type().contains("message"))
        .count();
    println!("\nMessage events in EventStore: {}", message_event_count);

    println!("\n✓ Message bus consensus test complete");
}
