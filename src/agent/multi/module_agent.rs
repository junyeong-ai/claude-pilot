//! Module-specialized agent using modmap::Module as the single source of truth.
//!
//! Key features:
//! - Uses `modmap::Module` directly for module context
//! - Integrates with `ModuleContextBuilder` for layered context composition
//! - Scope enforcement to prevent unauthorized cross-module changes
//! - Priority scoring based on business value and risk

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use modmap::{Module, ModuleContext as ManifestModuleContext};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::context::ModuleContextBuilder;
use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact, extract_files_from_output,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::error::Result;

/// Scope validation result for proposed changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeValidation {
    pub valid: bool,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub warnings: Vec<String>,
}

/// Module-specialized agent with scope enforcement.
///
/// Uses `modmap::Module` as the single source of truth for module information.
/// The agent's context is built using `ModuleContextBuilder` which supports
/// the layered context architecture (Module → ManifestContext → Rules → Skills).
pub struct ModuleAgent {
    core: AgentCore,
    module: Arc<Module>,
    manifest_context: Option<ManifestModuleContext>,
    /// Cached system prompt built from module context
    system_prompt: String,
}

impl ModuleAgent {
    /// Create a new ModuleAgent from a modmap::Module.
    pub fn new(module: Arc<Module>, task_agent: Arc<TaskAgent>) -> Self {
        let role = AgentRole::module(&module.id);
        let id = format!("module-{}-0", module.id);
        let system_prompt = ModuleContextBuilder::new(&module).build_system_prompt();

        Self {
            core: AgentCore::new(id, role, task_agent),
            module,
            manifest_context: None,
            system_prompt,
        }
    }

    /// Create a ModuleAgent with manifest context for enhanced prompts.
    pub fn with_manifest_context(
        module: Arc<Module>,
        manifest_context: ManifestModuleContext,
        task_agent: Arc<TaskAgent>,
    ) -> Self {
        let role = AgentRole::module(&module.id);
        let id = format!("module-{}-0", module.id);
        let system_prompt = ModuleContextBuilder::new(&module)
            .with_manifest_context(&manifest_context)
            .build_system_prompt();

        Self {
            core: AgentCore::new(id, role, task_agent),
            module,
            manifest_context: Some(manifest_context),
            system_prompt,
        }
    }

    /// Create a ModuleAgent with a specific ID.
    pub fn with_id(id: &str, module: Arc<Module>, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        let role = AgentRole::module(&module.id);
        let system_prompt = ModuleContextBuilder::new(&module).build_system_prompt();

        Arc::new(Self {
            core: AgentCore::new(id.to_string(), role, task_agent),
            module,
            manifest_context: None,
            system_prompt,
        })
    }

    /// Create a ModuleAgent with a specific ID and manifest context.
    pub fn with_id_and_context(
        id: &str,
        module: Arc<Module>,
        manifest_context: ManifestModuleContext,
        task_agent: Arc<TaskAgent>,
    ) -> Arc<Self> {
        let role = AgentRole::module(&module.id);
        let system_prompt = ModuleContextBuilder::new(&module)
            .with_manifest_context(&manifest_context)
            .build_system_prompt();

        Arc::new(Self {
            core: AgentCore::new(id.to_string(), role, task_agent),
            module,
            manifest_context: Some(manifest_context),
            system_prompt,
        })
    }

    /// Get the manifest context if available.
    pub fn manifest_context(&self) -> Option<&ManifestModuleContext> {
        self.manifest_context.as_ref()
    }

    /// Get the underlying module.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Get the module ID.
    pub fn module_id(&self) -> &str {
        &self.module.id
    }

    /// Validate if proposed changes are within this module's scope.
    pub fn validate_scope(&self, proposed_files: &[String]) -> ScopeValidation {
        let mut in_scope = Vec::new();
        let mut out_of_scope = Vec::new();
        let mut warnings = Vec::new();

        for file in proposed_files {
            if self.module.contains_file(file) {
                in_scope.push(file.clone());
            } else {
                out_of_scope.push(file.clone());

                // Check if it's a known dependency
                for dep in &self.module.dependencies {
                    if file.contains(&dep.module_id) {
                        warnings.push(format!(
                            "File '{}' belongs to dependency '{}'. Coordinate with that module.",
                            file, dep.module_id
                        ));
                    }
                }
            }
        }

        // Generate warnings for out-of-scope files
        if !out_of_scope.is_empty() {
            warnings.insert(
                0,
                format!(
                    "Module '{}' should not modify files outside its scope: {}",
                    self.module.name,
                    out_of_scope.join(", ")
                ),
            );
        }

        ScopeValidation {
            valid: out_of_scope.is_empty(),
            in_scope,
            out_of_scope,
            warnings,
        }
    }
}

#[async_trait]
impl SpecializedAgent for ModuleAgent {
    fn role(&self) -> &AgentRole {
        self.core.role()
    }

    fn id(&self) -> &str {
        self.core.id()
    }

    fn identifier(&self) -> AgentIdentifier {
        self.core.identifier().clone()
    }

    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(
            module = %self.module.id,
            task_id = %task.id,
            "Module agent executing"
        );

        // Use composed_prompt from task context if available (set by Coordinator)
        // This includes: Module context + optional Rules + optional Skill
        let effective_prompt = task
            .context
            .composed_prompt
            .as_deref()
            .unwrap_or(&self.system_prompt);

        let prompt = AgentPromptBuilder::new(effective_prompt, "Module", task)
            .with_context(task)
            .with_related_files(task)
            .with_blockers(task)
            .build();

        let output = self
            .core
            .task_agent
            .run_with_composed_context(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                // Validate scope of proposed changes
                // Limit extraction to prevent DoS from malformed output
                let max_files = (self.module.paths.len() * 2).clamp(20, 100);
                let proposed_files = extract_files_from_output(&output, max_files);
                let validation = self.validate_scope(&proposed_files);

                let mut findings = Vec::new();

                // Add scope validation warnings
                if !validation.valid {
                    warn!(
                        module = %self.module.id,
                        out_of_scope = ?validation.out_of_scope,
                        "Module agent proposed out-of-scope changes"
                    );
                    findings.extend(validation.warnings.clone());
                }

                let result = if validation.valid {
                    AgentTaskResult::success(&task.id, output.clone())
                } else {
                    AgentTaskResult::failure(&task.id, output.clone())
                };
                Ok(result
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("module-{}-{}.md", self.module.id, task.id),
                        content: output,
                        artifact_type: ArtifactType::Code,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modmap::{
        Convention, EvidenceLocation, IssueCategory, IssueSeverity, KnownIssue, ModuleDependency,
        ModuleMetrics,
    };

    fn create_test_module() -> Module {
        Module {
            id: "auth".into(),
            name: "Authentication".into(),
            paths: vec!["src/auth/".into(), "src/auth/login.rs".into()],
            key_files: vec!["src/auth/mod.rs".into()],
            dependencies: vec![ModuleDependency::runtime("db")],
            dependents: vec!["api".into()],
            responsibility: "Handle authentication and authorization".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.85, 0.9, 0.7),
            conventions: vec![Convention::new(
                "error-types",
                "Use Result types for all public APIs",
            )],
            known_issues: vec![KnownIssue::new(
                "token-refresh",
                "Token refresh may fail under load",
                IssueSeverity::Medium,
                IssueCategory::Performance,
            )],
            evidence: vec![EvidenceLocation::new_range("src/auth/mod.rs", 1, 50)],
        }
    }

    #[test]
    fn test_module_agent_role() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);

        assert_eq!(agent.role(), &AgentRole::module("auth"));
        assert!(agent.system_prompt().contains("Authentication"));
        assert!(agent.system_prompt().contains("Scope Enforcement"));
    }

    #[test]
    fn test_module_agent_id() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);

        assert_eq!(agent.id(), "module-auth-0");
        assert_eq!(agent.module_id(), "auth");
    }

    #[test]
    fn test_scope_validation_in_scope() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);

        let validation =
            agent.validate_scope(&["src/auth/login.rs".into(), "src/auth/session.rs".into()]);

        assert!(validation.valid);
        assert_eq!(validation.in_scope.len(), 2);
        assert!(validation.out_of_scope.is_empty());
        assert!(validation.warnings.is_empty());
    }

    #[test]
    fn test_scope_validation_out_of_scope() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);

        let validation = agent.validate_scope(&[
            "src/auth/login.rs".into(),
            "src/api/routes.rs".into(), // out of scope
        ]);

        assert!(!validation.valid);
        assert_eq!(validation.in_scope.len(), 1);
        assert_eq!(validation.out_of_scope.len(), 1);
        assert!(!validation.warnings.is_empty());
    }

    #[test]
    fn test_scope_validation_warns_about_dependencies() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);

        let validation = agent.validate_scope(&[
            "src/db/pool.rs".into(), // in dependencies
        ]);

        assert!(!validation.valid);
        // Should have warning about coordinating with db module
        assert!(validation.warnings.iter().any(|w| w.contains("db")));
    }

    #[test]
    fn test_system_prompt_contains_key_sections() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::new(module, task_agent);
        let prompt = agent.system_prompt();

        // Check for key sections
        assert!(prompt.contains("# Module: Authentication"));
        assert!(prompt.contains("Responsibility"));
        assert!(prompt.contains("Scope"));
        assert!(prompt.contains("Dependencies"));
        assert!(prompt.contains("Conventions"));
        assert!(prompt.contains("Known Issues"));
        assert!(prompt.contains("CRITICAL: Scope Enforcement"));
        assert!(prompt.contains("OUT_OF_SCOPE"));

        // Check for evidence references
        assert!(prompt.contains("@src/auth/mod.rs:1-50"));
    }

    #[test]
    fn test_with_id() {
        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ModuleAgent::with_id("custom-auth-0", module, task_agent);

        assert_eq!(agent.id(), "custom-auth-0");
        assert_eq!(agent.module_id(), "auth");
    }

    #[test]
    fn test_with_manifest_context() {
        use modmap::ModuleContext as ManifestModuleContext;

        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let manifest_ctx = ManifestModuleContext::new()
            .with_rules(vec!["rules/modules/auth.md".into()])
            .with_conventions(vec!["Use bcrypt for password hashing".into()])
            .with_issues(vec!["[HIGH] Rate limiting needed".into()]);

        let agent = ModuleAgent::with_manifest_context(module, manifest_ctx, task_agent);
        let prompt = agent.system_prompt();

        // Should have module section
        assert!(prompt.contains("# Module: Authentication"));

        // Should have manifest context section
        assert!(prompt.contains("# Module Context (Manifest)"));
        assert!(prompt.contains("rules/modules/auth.md"));
        assert!(prompt.contains("Use bcrypt for password hashing"));
        assert!(prompt.contains("[HIGH] Rate limiting needed"));

        // Should have manifest_context accessible
        assert!(agent.manifest_context().is_some());
    }

    #[test]
    fn test_with_id_and_context() {
        use modmap::ModuleContext as ManifestModuleContext;

        let module = Arc::new(create_test_module());
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let manifest_ctx =
            ManifestModuleContext::new().with_rules(vec!["rules/modules/auth.md".into()]);

        let agent =
            ModuleAgent::with_id_and_context("custom-auth-1", module, manifest_ctx, task_agent);

        assert_eq!(agent.id(), "custom-auth-1");
        assert!(agent.system_prompt().contains("rules/modules/auth.md"));
        assert!(agent.manifest_context().is_some());
    }
}
