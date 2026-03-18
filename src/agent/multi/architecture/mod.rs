//! Architecture agent for runtime boundary enforcement and convention validation.
//!
//! # Distinction from ArchitectAgent
//!
//! - **BoundaryEnforcementAgent** (this): Runtime enforcement agent that detects boundary violations,
//!   convention breaches, and layer dependency issues based on project-specific rules.
//!   Uses `Architecture` role category.
//!
//! - **ArchitectAgent**: High-level design advisor that validates architectural decisions
//!   for consistency, cohesion, and maintainability. Participates in consensus as `Advisor` role.

mod validation;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use modmap::{Module, WorkspaceType};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact, extract_field,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::domain::Severity;
use crate::error::Result;
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationType {
    BoundaryViolation,
    CircularDependency,
    ConventionViolation,
    LayerViolation,
    UnauthorizedDependency,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureViolation {
    pub violation_type: ViolationType,
    pub file: PathBuf,
    pub description: String,
    pub severity: Severity,
    pub suggested_fix: Option<String>,
    pub affected_modules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureValidation {
    pub passed: bool,
    pub violations: Vec<ArchitectureViolation>,
    pub warnings: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct ArchitecturalFileChange {
    pub path: PathBuf,
    pub change_type: ChangeType,
    pub module: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    Created,
    Modified,
    Deleted,
}

pub struct BoundaryEnforcementAgent {
    core: AgentCore,
    modules: Vec<Module>,
    system_prompt: String,
    pub(super) module_boundaries: HashMap<String, ModuleBoundary>,
}

#[derive(Debug, Clone)]
pub(super) struct ModuleBoundary {
    pub(super) files: HashSet<PathBuf>,
    pub(super) allowed_dependencies: HashSet<String>,
}

impl BoundaryEnforcementAgent {
    pub fn from_workspace(workspace: &Workspace, task_agent: Arc<TaskAgent>) -> Self {
        let modules = workspace.modules().to_vec();
        let languages = workspace.languages().to_vec();
        let workspace_type = workspace.workspace_type().clone();
        Self::from_modules(modules, languages, workspace_type, task_agent)
    }

    pub fn from_modules(
        modules: Vec<Module>,
        languages: Vec<modmap::DetectedLanguage>,
        workspace_type: WorkspaceType,
        task_agent: Arc<TaskAgent>,
    ) -> Self {
        let system_prompt = Self::build_system_prompt(&modules, &languages, &workspace_type);
        let module_boundaries = Self::build_module_boundaries(&modules);

        Self {
            core: AgentCore::new("architect-0", AgentRole::architect(), task_agent),
            modules,
            system_prompt,
            module_boundaries,
        }
    }

    fn build_module_boundaries(modules: &[Module]) -> HashMap<String, ModuleBoundary> {
        let mut boundaries = HashMap::new();

        for module in modules {
            let files: HashSet<PathBuf> = module.paths.iter().map(PathBuf::from).collect();
            let allowed_deps: HashSet<String> = module
                .dependencies
                .iter()
                .map(|d| d.module_id.clone())
                .collect();

            boundaries.insert(
                module.name.clone(),
                ModuleBoundary {
                    files,
                    allowed_dependencies: allowed_deps,
                },
            );
        }

        boundaries
    }

    fn build_system_prompt(
        modules: &[Module],
        languages: &[modmap::DetectedLanguage],
        workspace_type: &WorkspaceType,
    ) -> String {
        let modules_desc: Vec<String> = modules
            .iter()
            .map(|m| {
                let deps = if m.dependencies.is_empty() {
                    "none".to_string()
                } else {
                    m.dependencies
                        .iter()
                        .map(|d| d.module_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("- **{}**: {} (deps: {})", m.name, m.responsibility, deps)
            })
            .collect();

        let language_names: Vec<String> = languages.iter().map(|l| l.name.clone()).collect();

        let ws_type = match workspace_type {
            WorkspaceType::Monorepo => "Monorepo",
            WorkspaceType::Microservices => "Microservices",
            WorkspaceType::MultiPackage => "Multi-Package",
            WorkspaceType::SinglePackage => "Single Package",
        };

        let mut dep_graph = String::new();
        for module in modules {
            if !module.dependencies.is_empty() {
                let dep_names: Vec<&str> = module
                    .dependencies
                    .iter()
                    .map(|d| d.module_id.as_str())
                    .collect();
                dep_graph.push_str(&format!("{} → {}\n", module.name, dep_names.join(", ")));
            }
        }

        format!(
            r#"# Architecture Agent

## Project Overview
- **Type**: {ws_type}
- **Languages**: {langs}

## Modules
{modules}

## Dependency Graph
```
{dep_graph}
```

## Your Role
You are the guardian of architectural integrity. Your responsibilities:

1. **Boundary Enforcement**: Ensure each module only modifies files within its scope
2. **Dependency Direction**: Verify dependencies flow in the correct direction
3. **Convention Compliance**: Check that changes follow project-wide conventions
4. **Layer Separation**: Ensure proper separation between layers (API, Business, Data)
5. **Cross-Module Impact**: Identify when changes affect multiple modules

## Validation Checklist
When reviewing changes, check for:
- [ ] Files modified only by their owning module
- [ ] No circular dependencies introduced
- [ ] Public APIs maintained or properly versioned
- [ ] Conventions followed (naming, structure, patterns)
- [ ] Layer boundaries respected

## Violation Reporting Format
When you find violations, report them in this format:
```
VIOLATION: type="boundary_violation" | severity="error" | file="path/to/file" | description="Module A modified file owned by Module B" | fix="Move change to Module B agent"
```

## CRITICAL RULES
1. **No Backdoor Dependencies**: Module A cannot use Module B's internals without going through public APIs
2. **Explicit Over Implicit**: All cross-module dependencies must be declared
3. **Single Responsibility**: Each module owns specific concerns
4. **Interface Stability**: Public interfaces require coordination before changes"#,
            langs = language_names.join(", "),
            modules = modules_desc.join("\n"),
            dep_graph = if dep_graph.is_empty() {
                "(no dependencies)".to_string()
            } else {
                dep_graph
            },
        )
    }

    pub fn modules(&self) -> &[Module] {
        &self.modules
    }

    pub fn parse_violations_from_output(output: &str) -> Vec<ArchitectureViolation> {
        let mut violations = Vec::new();

        for line in output.lines() {
            if !line.trim().to_uppercase().starts_with("VIOLATION:") {
                continue;
            }

            let line = line.trim();

            let violation_type = extract_field(line, "type:")
                .map(|t| match t.to_lowercase().as_str() {
                    "boundary_violation" => ViolationType::BoundaryViolation,
                    "circular_dependency" => ViolationType::CircularDependency,
                    "convention_violation" => ViolationType::ConventionViolation,
                    "layer_violation" => ViolationType::LayerViolation,
                    "unauthorized_dependency" => ViolationType::UnauthorizedDependency,
                    _ => ViolationType::ConventionViolation,
                })
                .unwrap_or(ViolationType::ConventionViolation);

            let severity = extract_field(line, "severity:")
                .map(|s| match s.to_lowercase().as_str() {
                    "critical" => Severity::Critical,
                    "error" => Severity::Error,
                    "warning" => Severity::Warning,
                    _ => Severity::Info,
                })
                .unwrap_or(Severity::Warning);

            let file = extract_field(line, "file:")
                .map(PathBuf::from)
                .unwrap_or_default();

            let description = extract_field(line, "description:")
                .unwrap_or_else(|| "Architecture violation".to_string());

            let fix = extract_field(line, "fix:");

            violations.push(ArchitectureViolation {
                violation_type,
                file,
                description,
                severity,
                suggested_fix: fix,
                affected_modules: vec![],
            });
        }

        violations
    }
}

#[async_trait]
impl SpecializedAgent for BoundaryEnforcementAgent {
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

        debug!(task_id = %task.id, "Architecture agent executing");

        let prompt = AgentPromptBuilder::new(&self.system_prompt, "Architecture", task)
            .with_context(task)
            .with_related_files(task)
            .with_blockers(task)
            .build();

        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                let violations = Self::parse_violations_from_output(&output);
                let mut findings = Vec::new();

                if !violations.is_empty() {
                    for v in &violations {
                        let finding = format!(
                            "[{:?}] {} - {} (file: {})",
                            v.severity,
                            format!("{:?}", v.violation_type).replace("Violation", ""),
                            v.description,
                            v.file.display()
                        );
                        findings.push(finding);

                        if v.severity >= Severity::Error {
                            warn!(
                                violation_type = ?v.violation_type,
                                file = %v.file.display(),
                                "Architecture violation detected"
                            );
                        }
                    }
                }

                let success = violations
                    .iter()
                    .all(|v| v.severity < Severity::Error);
                let result = if success {
                    AgentTaskResult::success(&task.id, output.clone())
                } else {
                    AgentTaskResult::failure(&task.id, output.clone())
                };
                Ok(result
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("architecture-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Report,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}
