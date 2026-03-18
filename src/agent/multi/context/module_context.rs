//! Module-based context builder using modmap::Module as the single source of truth.
//!
//! Implements the layered context architecture:
//! - Layer 0: Base Agent (WHO) - Required
//! - Layer 1: modmap Module (WHAT) - Primary module context
//! - Layer 2: Manifest Context (WHAT) - Per-module rules, conventions, issues from manifest

use modmap::{Module, ModuleContext as ManifestModuleContext};

/// Builds agent context from modmap::Module with optional enhancements.
///
/// The builder follows the progressive enhancement principle:
/// - modmap Module alone provides sufficient context for collaboration
/// - Manifest context adds per-module rule paths, conventions, issues
pub struct ModuleContextBuilder<'a> {
    module: Option<&'a Module>,
    manifest_context: Option<&'a ManifestModuleContext>,
}

impl<'a> ModuleContextBuilder<'a> {
    /// Create a builder with a module as the primary context source.
    pub fn new(module: &'a Module) -> Self {
        Self {
            module: Some(module),
            manifest_context: None,
        }
    }

    /// Create an empty builder for fallback cases.
    pub fn empty() -> Self {
        Self {
            module: None,
            manifest_context: None,
        }
    }

    /// Add manifest context (per-module rules, conventions, issues).
    pub fn with_manifest_context(mut self, ctx: &'a ManifestModuleContext) -> Self {
        self.manifest_context = Some(ctx);
        self
    }

    /// Build the complete system prompt.
    pub fn build_system_prompt(&self) -> String {
        let capacity = self.estimate_capacity();
        let mut prompt = String::with_capacity(capacity);

        // Layer 1: Module base context (primary)
        if let Some(module) = self.module {
            prompt.push_str(&self.build_module_section(module));
        }

        // Layer 2: Manifest context (per-module supplementary data)
        if let Some(ctx) = self.manifest_context
            && !ctx.is_empty()
        {
            prompt.push_str("\n\n---\n\n");
            prompt.push_str(&self.build_manifest_context_section(ctx));
        }

        prompt
    }

    /// Check if this builder has a module context.
    pub fn has_module(&self) -> bool {
        self.module.is_some()
    }

    /// Get the module ID if available.
    pub fn module_id(&self) -> Option<&str> {
        self.module.map(|m| m.id.as_str())
    }

    fn estimate_capacity(&self) -> usize {
        let module_size = self
            .module
            .map(|m| {
                m.responsibility.len()
                    + m.paths.iter().map(|p| p.len()).sum::<usize>()
                    + m.conventions.len() * 50
                    + m.known_issues.len() * 100
                    + m.evidence.len() * 50
                    + 1024 // fixed overhead
            })
            .unwrap_or(0);

        let manifest_size = self
            .manifest_context
            .map(|ctx| {
                ctx.rules.len() * 50
                    + ctx.conventions.iter().map(|c| c.len()).sum::<usize>()
                    + ctx.issues.iter().map(|i| i.len()).sum::<usize>()
                    + 256
            })
            .unwrap_or(0);

        module_size + manifest_size + 256
    }

    fn build_manifest_context_section(&self, ctx: &ManifestModuleContext) -> String {
        let mut section = String::new();
        section.push_str("# Module Context (Manifest)\n\n");

        if !ctx.rules.is_empty() {
            section.push_str("## Associated Rule Files\n");
            for rule in &ctx.rules {
                section.push_str(&format!("- {}\n", rule));
            }
            section.push('\n');
        }

        if !ctx.conventions.is_empty() {
            section.push_str("## Additional Conventions\n");
            for conv in &ctx.conventions {
                section.push_str(&format!("- {}\n", conv));
            }
            section.push('\n');
        }

        if !ctx.issues.is_empty() {
            section.push_str("## Additional Known Issues\n");
            for issue in &ctx.issues {
                section.push_str(&format!("- {}\n", issue));
            }
        }

        section
    }

    fn build_module_section(&self, module: &Module) -> String {
        format!(
            r#"# Module: {name}

## Responsibility
{responsibility}

## Scope
Files under: {paths}

## Dependencies
{dependencies}

## Conventions
{conventions}

## Known Issues
{known_issues}

## Evidence References
{evidence}

## CRITICAL: Scope Enforcement
You are ONLY authorized to modify files within this module's scope.
If changes require files outside your scope, FLAG IT with:
```
OUT_OF_SCOPE: file="path" reason="why" module="owner"
```
"#,
            name = module.name,
            responsibility = module.responsibility,
            paths = module.paths.join(", "),
            dependencies = self.format_dependencies(module),
            conventions = self.format_conventions(module),
            known_issues = self.format_known_issues(module),
            evidence = self.format_evidence(module),
        )
    }

    fn format_dependencies(&self, module: &Module) -> String {
        if module.dependencies.is_empty() {
            return "None".to_string();
        }
        module
            .dependencies
            .iter()
            .map(|d| format!("- {} ({:?})", d.module_id, d.dependency_type))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_conventions(&self, module: &Module) -> String {
        if module.conventions.is_empty() {
            return "Follow project-wide conventions".to_string();
        }
        module
            .conventions
            .iter()
            .map(|c| {
                if let Some(rationale) = &c.rationale {
                    format!("- **{}**: {} ({})", c.name, c.pattern, rationale)
                } else {
                    format!("- **{}**: {}", c.name, c.pattern)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_known_issues(&self, module: &Module) -> String {
        if module.known_issues.is_empty() {
            return "None documented".to_string();
        }
        module
            .known_issues
            .iter()
            .map(|i| {
                let mut line = format!("- [{}] {}: {}", i.severity, i.id, i.description);
                if let Some(prevention) = &i.prevention {
                    line.push_str(&format!(" → Prevention: {}", prevention));
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn format_evidence(&self, module: &Module) -> String {
        if module.evidence.is_empty() {
            return "None".to_string();
        }
        module
            .evidence
            .iter()
            .map(|e| format!("- @{}", e.to_reference()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Built context containing the system prompt and metadata.
#[derive(Debug, Clone)]
pub struct ModuleContext {
    pub system_prompt: String,
    pub module_id: Option<String>,
}

impl ModuleContext {
    pub fn from_builder(builder: &ModuleContextBuilder<'_>) -> Self {
        Self {
            system_prompt: builder.build_system_prompt(),
            module_id: builder.module_id().map(String::from),
        }
    }

    pub fn empty() -> Self {
        Self {
            system_prompt: String::new(),
            module_id: None,
        }
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
            paths: vec!["src/auth/".into(), "src/security/".into()],
            key_files: vec!["src/auth/mod.rs".into()],
            dependencies: vec![
                ModuleDependency::runtime("db"),
                ModuleDependency::optional("cache"),
            ],
            dependents: vec!["api".into()],
            responsibility: "Handle user authentication and session management".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::new(0.85, 0.9, 0.7),
            conventions: vec![
                Convention::new("error-handling", "Return Result types for all public APIs")
                    .with_rationale("Enables proper error propagation"),
                Convention::new("logging", "Log all auth attempts"),
            ],
            known_issues: vec![
                KnownIssue::new(
                    "token-refresh",
                    "Token refresh may fail under high load",
                    IssueSeverity::Medium,
                    IssueCategory::Performance,
                )
                .with_prevention("Implement retry with backoff"),
            ],
            evidence: vec![
                EvidenceLocation::new_range("src/auth/mod.rs", 1, 50),
                EvidenceLocation::new("src/auth/session.rs", 100),
            ],
        }
    }

    #[test]
    fn test_build_module_context() {
        let module = create_test_module();
        let builder = ModuleContextBuilder::new(&module);
        let prompt = builder.build_system_prompt();

        assert!(prompt.contains("# Module: Authentication"));
        assert!(prompt.contains("Handle user authentication"));
        assert!(prompt.contains("src/auth/"));
        assert!(prompt.contains("src/security/"));
        assert!(prompt.contains("db"));
        assert!(prompt.contains("cache"));
        assert!(prompt.contains("error-handling"));
        assert!(prompt.contains("Return Result types"));
        assert!(prompt.contains("token-refresh"));
        assert!(prompt.contains("MEDIUM"));
        assert!(prompt.contains("retry with backoff"));
        assert!(prompt.contains("@src/auth/mod.rs:1-50"));
        assert!(prompt.contains("@src/auth/session.rs:100"));
        assert!(prompt.contains("CRITICAL: Scope Enforcement"));
        assert!(prompt.contains("OUT_OF_SCOPE"));
    }

    #[test]
    fn test_empty_builder() {
        let builder = ModuleContextBuilder::empty();
        let prompt = builder.build_system_prompt();

        assert!(prompt.is_empty());
        assert!(!builder.has_module());
        assert!(builder.module_id().is_none());
    }

    #[test]
    fn test_module_context_from_builder() {
        let module = create_test_module();
        let builder = ModuleContextBuilder::new(&module);
        let ctx = ModuleContext::from_builder(&builder);

        assert_eq!(ctx.module_id, Some("auth".into()));
        assert!(!ctx.system_prompt.is_empty());
    }

    #[test]
    fn test_format_dependencies_empty() {
        let mut module = create_test_module();
        module.dependencies.clear();

        let builder = ModuleContextBuilder::new(&module);
        let deps = builder.format_dependencies(&module);

        assert_eq!(deps, "None");
    }

    #[test]
    fn test_format_conventions_empty() {
        let mut module = create_test_module();
        module.conventions.clear();

        let builder = ModuleContextBuilder::new(&module);
        let convs = builder.format_conventions(&module);

        assert_eq!(convs, "Follow project-wide conventions");
    }

    #[test]
    fn test_builder_with_manifest_context() {
        let module = create_test_module();
        let manifest_ctx = ManifestModuleContext::new()
            .with_rules(vec!["rules/modules/auth.md".into()])
            .with_conventions(vec!["Use bcrypt for password hashing".into()])
            .with_issues(vec!["[HIGH] Rate limiting needed".into()]);

        let builder = ModuleContextBuilder::new(&module).with_manifest_context(&manifest_ctx);
        let prompt = builder.build_system_prompt();

        assert!(prompt.contains("# Module: Authentication"));
        assert!(prompt.contains("# Module Context (Manifest)"));
        assert!(prompt.contains("rules/modules/auth.md"));
        assert!(prompt.contains("Use bcrypt for password hashing"));
        assert!(prompt.contains("[HIGH] Rate limiting needed"));
    }

    #[test]
    fn test_builder_with_empty_manifest_context() {
        let module = create_test_module();
        let manifest_ctx = ManifestModuleContext::new();

        let builder = ModuleContextBuilder::new(&module).with_manifest_context(&manifest_ctx);
        let prompt = builder.build_system_prompt();

        assert!(prompt.contains("# Module: Authentication"));
        assert!(!prompt.contains("# Module Context (Manifest)"));
    }
}
