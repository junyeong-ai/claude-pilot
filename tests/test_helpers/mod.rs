use std::path::Path;
use std::sync::Arc;

use claude_pilot::agent::TaskAgent;
use claude_pilot::agent::multi::{
    CoderAgent, PlanningAgent, ResearchAgent, ReviewerAgent, SpecializedAgent, VerifierAgent,
};
use claude_pilot::config::AgentConfig;
use claude_pilot::state::EventStore;

pub fn create_task_agent() -> Arc<TaskAgent> {
    Arc::new(TaskAgent::new(AgentConfig::default()))
}

pub fn create_core_agents(task_agent: Arc<TaskAgent>) -> Vec<Arc<dyn SpecializedAgent>> {
    vec![
        ResearchAgent::with_id("research-0", Arc::clone(&task_agent)),
        PlanningAgent::with_id("planning-0", Arc::clone(&task_agent)),
        CoderAgent::with_id("coder-0", Arc::clone(&task_agent)),
        VerifierAgent::with_id("verifier-0", Arc::clone(&task_agent)),
        ReviewerAgent::with_id("reviewer-0", Arc::clone(&task_agent)),
    ]
}

pub fn setup_event_store(project_dir: &Path) -> Arc<EventStore> {
    let events_dir = project_dir.join(".pilot/events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let db_path = events_dir.join("events.db");
    Arc::new(EventStore::new(&db_path).unwrap())
}
