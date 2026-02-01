//! Specialized agent trait for multi-agent architecture.
//!
//! This module re-exports types from core/ and shared/ modules.
//! All types are now defined in their respective SSOT locations.

// Re-export shared types (SSOT)
pub use super::shared::{AgentId, PermissionProfile, RoleCategory, TaskPriority};

// Re-export core types
pub use super::core::{
    AgentCore, AgentMetrics, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult,
    ArtifactType, BoxedAgent, ConvergenceResult, LoadGuard, LoadTracker, MetricsSnapshot,
    SpecializedAgent, TaskArtifact, TaskContext, TaskStatus, VerificationVerdict,
    calculate_priority_score, extract_field, extract_file_path, extract_files_from_output,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_generation() {
        assert_eq!(
            AgentId::module_with_instance("auth", 0).as_str(),
            "module-auth-0"
        );
        assert_eq!(
            AgentId::module_with_instance("Auth", 0).as_str(),
            "module-auth-0"
        );
        assert_eq!(
            AgentId::module_with_instance("user-service", 1).as_str(),
            "module-user-service-1"
        );
        assert_eq!(AgentId::reviewer(0).as_str(), "reviewer-0");
        assert_eq!(AgentId::core("planning", 0).as_str(), "planning-0");
        assert_eq!(AgentId::architecture(0).as_str(), "architecture-0");
    }

    #[test]
    fn test_agent_role_display() {
        assert_eq!(AgentRole::core_research().to_string(), "research");
        assert_eq!(AgentRole::core_planning().to_string(), "planning");
        assert_eq!(AgentRole::core_coder().to_string(), "coder");
        assert_eq!(AgentRole::core_verifier().to_string(), "verifier");
        assert_eq!(AgentRole::module("auth").to_string(), "module-auth");
        assert_eq!(AgentRole::reviewer().to_string(), "reviewer");
        assert_eq!(AgentRole::architect().to_string(), "architect");
    }

    #[test]
    fn test_role_category() {
        assert!(AgentRole::core_coder().is_core());
        assert!(AgentRole::module("api").is_module());
        assert!(!AgentRole::core_coder().is_module());
        assert_eq!(AgentRole::architect().category, RoleCategory::Architecture);
    }

    #[test]
    fn test_role_equality() {
        assert_eq!(AgentRole::core_research(), AgentRole::core_research());
        assert_ne!(AgentRole::core_research(), AgentRole::core_coder());
        assert_ne!(AgentRole::module("auth"), AgentRole::module("api"));
    }

    #[test]
    fn test_load_tracker() {
        let tracker = LoadTracker::new();
        assert_eq!(tracker.current(), 0);

        tracker.increment();
        assert_eq!(tracker.current(), 1);

        tracker.increment();
        assert_eq!(tracker.current(), 2);

        tracker.decrement();
        assert_eq!(tracker.current(), 1);
    }

    #[test]
    fn test_task_priority_ordering() {
        assert!(TaskPriority::Low < TaskPriority::Normal);
        assert!(TaskPriority::Normal < TaskPriority::High);
        assert!(TaskPriority::High < TaskPriority::Critical);
    }

    #[test]
    fn test_extract_field_quoted() {
        let line = r#"task: "implement auth" | module: "auth" | deps: "db""#;
        assert_eq!(extract_field(line, "task:"), Some("implement auth".into()));
        assert_eq!(extract_field(line, "module:"), Some("auth".into()));
        assert_eq!(extract_field(line, "deps:"), Some("db".into()));
    }

    #[test]
    fn test_extract_file_path_basic() {
        assert_eq!(
            extract_file_path("src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            extract_file_path("./config.toml"),
            Some("./config.toml".to_string())
        );
    }

    #[test]
    fn test_extract_files_from_output_multiple() {
        let output = "Modified:\n  src/main.rs\n  src/lib.rs\n  tests/test.rs";
        let files = extract_files_from_output(output, 10);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"tests/test.rs".to_string()));
    }
}
