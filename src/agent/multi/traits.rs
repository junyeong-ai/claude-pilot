//! Specialized agent trait for multi-agent architecture.
//!
//! This module re-exports types from core/ and shared/ modules.
//! All types are now defined in their respective SSOT locations.

// Re-export shared types (SSOT) — pub(crate) since canonical path is agent::multi::shared::*
pub(crate) use super::shared::{AgentId, TaskPriority};

// Re-export core types
pub use super::core::{
    AgentCore, AgentMetrics, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult,
    AgentConvergenceResult, ArtifactType, BoxedAgent, ExecutionOutcome, LoadTracker, AgentExecutionMetrics,
    SpecializedAgent, TaskArtifact, TaskContext, VerificationVerdict,
    calculate_priority_score, extract_field, extract_files_from_output,
};

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::core::extract_file_path;

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
        let files = extract_files_from_output(output, 10, None);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"tests/test.rs".to_string()));
    }
}
