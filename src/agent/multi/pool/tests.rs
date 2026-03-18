use std::path::Path;
use std::sync::Arc;

use super::*;
use super::super::traits::{AgentTask, LoadTracker, TaskContext, TaskPriority};

struct MockAgent {
    id: String,
    role: AgentRole,
    load: LoadTracker,
}

impl MockAgent {
    fn new(id: &str, role: AgentRole) -> Self {
        Self {
            id: id.to_string(),
            role,
            load: LoadTracker::new(),
        }
    }
}

#[async_trait::async_trait]
impl SpecializedAgent for MockAgent {
    fn role(&self) -> &AgentRole {
        &self.role
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn system_prompt(&self) -> &str {
        "Mock agent"
    }

    async fn execute(
        &self,
        task: &AgentTask,
        _working_dir: &Path,
    ) -> crate::error::Result<super::super::core::AgentTaskResult> {
        self.load.increment();
        let result =
            super::super::core::AgentTaskResult::success(&task.id, format!("Executed by {}", self.id));
        self.load.decrement();
        Ok(result)
    }

    fn current_load(&self) -> u32 {
        self.load.current()
    }
}

#[test]
fn test_pool_registration() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    let agent = Arc::new(MockAgent::new("test-1", AgentRole::core_coder()));
    pool.register(agent).unwrap();

    assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);
    assert_eq!(pool.total_agents(), 1);
}

#[test]
fn test_duplicate_registration_fails() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new("dup-1", AgentRole::core_coder())))
        .unwrap();
    let result = pool.register(Arc::new(MockAgent::new("dup-1", AgentRole::core_coder())));
    assert!(result.is_err());
}

#[test]
fn test_agent_selection() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
        .unwrap();
    pool.register(Arc::new(MockAgent::new("coder-2", AgentRole::core_coder())))
        .unwrap();

    let agent = pool.select(&AgentRole::core_coder());
    assert!(agent.is_some());

    let no_agent = pool.select(&AgentRole::core_research());
    assert!(no_agent.is_none());
}

#[test]
fn test_module_agent_selection() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new(
        "module-auth-0",
        AgentRole::module("auth"),
    )))
    .unwrap();

    assert!(pool.select(&AgentRole::module("auth")).is_some());
    assert!(pool.select(&AgentRole::module("api")).is_none());
}

#[test]
fn test_role_determination_uses_explicit_role() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    let task = AgentTask {
        id: "1".to_string(),
        description: "Some task".to_string(),
        context: TaskContext::default(),
        priority: TaskPriority::Normal,
        role: Some(AgentRole::core_research()),
    };
    assert_eq!(pool.determine_role(&task), AgentRole::core_research());
}

#[test]
fn test_role_determination_defaults_to_coder() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    let task = AgentTask {
        id: "1".to_string(),
        description: "Analyze codebase for patterns".to_string(),
        context: TaskContext::default(),
        priority: TaskPriority::Normal,
        role: None,
    };
    assert_eq!(pool.determine_role(&task), AgentRole::core_coder());
}

#[test]
fn test_pool_builder() {
    let config = MultiAgentConfig::default();
    let pool = AgentPoolBuilder::new(config)
        .with_agent(Arc::new(MockAgent::new("r1", AgentRole::core_research())))
        .with_agent(Arc::new(MockAgent::new("c1", AgentRole::core_coder())))
        .build()
        .unwrap();

    assert_eq!(pool.total_agents(), 2);
    assert_eq!(pool.agent_count(&AgentRole::core_research()), 1);
    assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);
}

#[test]
fn test_shutdown() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new("test-1", AgentRole::core_coder())))
        .unwrap();
    assert!(!pool.is_shutdown());

    pool.shutdown();
    assert!(pool.is_shutdown());

    let result = pool.register(Arc::new(MockAgent::new("test-2", AgentRole::core_coder())));
    assert!(result.is_err());
}

#[test]
fn test_metrics() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    let metrics = pool.metrics();
    assert_eq!(metrics.total_executions, 0);
    assert_eq!(metrics.success_rate, 0.0);
}

#[test]
fn test_unregister() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new(
        "to-remove",
        AgentRole::core_coder(),
    )))
    .unwrap();
    assert_eq!(pool.agent_count(&AgentRole::core_coder()), 1);

    assert!(pool.unregister("to-remove"));
    assert_eq!(pool.agent_count(&AgentRole::core_coder()), 0);

    assert!(!pool.unregister("nonexistent"));
}

#[test]
fn test_has_agent() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new("agent-1", AgentRole::core_coder())))
        .unwrap();

    assert!(pool.has_agent("agent-1"));
    assert!(!pool.has_agent("agent-2"));
}

#[test]
fn test_statistics() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new("agent-1", AgentRole::core_coder())))
        .unwrap();

    let stats = pool.statistics();
    assert_eq!(stats.agent_count, 1);
    assert!(!stats.is_shutdown);
    assert_eq!(stats.metrics.total_executions, 0);
}

#[tokio::test]
async fn test_module_agent_fallback_to_coder() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    // Register only core_coder, no module agents
    pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
        .unwrap();

    // Create task with module role that doesn't exist
    let task = AgentTask {
        id: "task-1".to_string(),
        description: "Implement auth feature".to_string(),
        context: TaskContext::default(),
        priority: TaskPriority::Normal,
        role: Some(AgentRole::module("auth")),
    };

    // Should fallback to coder instead of failing
    let result = pool.execute(&task, std::path::Path::new(".")).await;
    assert!(
        result.is_ok(),
        "Should fallback to coder when module agent unavailable"
    );
}

#[test]
fn test_select_by_qualified_id() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new(
        "research-0",
        AgentRole::core_research(),
    )))
    .unwrap();
    pool.register(Arc::new(MockAgent::new(
        "research-1",
        AgentRole::core_research(),
    )))
    .unwrap();

    // Should find by qualified ID
    let agent = pool.select_by_qualified_id("research-0");
    assert!(agent.is_some());
    assert_eq!(agent.unwrap().id(), "research-0");

    let agent = pool.select_by_qualified_id("research-1");
    assert!(agent.is_some());
    assert_eq!(agent.unwrap().id(), "research-1");

    // Should not find nonexistent
    assert!(pool.select_by_qualified_id("research-99").is_none());
}

#[test]
fn test_select_by_qualified_id_fallback() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    // Register agents with simple IDs
    pool.register(Arc::new(MockAgent::new("coder-0", AgentRole::core_coder())))
        .unwrap();
    pool.register(Arc::new(MockAgent::new("coder-1", AgentRole::core_coder())))
        .unwrap();

    // Exact match should work
    let agent = pool.select_by_qualified_id("coder-0");
    assert!(agent.is_some());
    assert_eq!(agent.unwrap().id(), "coder-0");

    // Cross-workspace fallback: "ws-a:auth:coder-0" -> "coder-0"
    let agent = pool.select_by_qualified_id("ws-a:auth:coder-0");
    assert!(agent.is_some(), "Should fallback to role-only match");
    assert_eq!(agent.unwrap().id(), "coder-0");

    // Cross-workspace fallback with different format
    let agent = pool.select_by_qualified_id("project-b:payments:coder-1");
    assert!(agent.is_some(), "Should fallback to role-only match");
    assert_eq!(agent.unwrap().id(), "coder-1");

    // Nonexistent should still return None
    assert!(
        pool.select_by_qualified_id("ws-x:mod:nonexistent-99")
            .is_none()
    );
}

#[test]
fn test_select_instances() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new(
        "planning-0",
        AgentRole::core_planning(),
    )))
    .unwrap();
    pool.register(Arc::new(MockAgent::new(
        "planning-1",
        AgentRole::core_planning(),
    )))
    .unwrap();
    pool.register(Arc::new(MockAgent::new("coder-0", AgentRole::core_coder())))
        .unwrap();

    // Should get all planning instances
    let planning_instances = pool.select_instances("planning");
    assert_eq!(planning_instances.len(), 2);

    // Should get coder instance
    let coder_instances = pool.select_instances("coder");
    assert_eq!(coder_instances.len(), 1);

    // Should get empty for nonexistent
    let none_instances = pool.select_instances("nonexistent");
    assert!(none_instances.is_empty());
}

#[test]
fn test_identifiers_for_pool() {
    let config = MultiAgentConfig::default();
    let pool = AgentPool::new(config);

    pool.register(Arc::new(MockAgent::new(
        "verifier-0",
        AgentRole::core_verifier(),
    )))
    .unwrap();
    pool.register(Arc::new(MockAgent::new(
        "verifier-1",
        AgentRole::core_verifier(),
    )))
    .unwrap();

    let identifiers = pool.identifiers_for_pool("verifier");
    assert_eq!(identifiers.len(), 2);

    // Verify they are proper identifiers
    let ids: Vec<String> = identifiers.iter().map(|id| id.qualified_id()).collect();
    assert!(ids.contains(&"verifier-0".to_string()));
    assert!(ids.contains(&"verifier-1".to_string()));
}
