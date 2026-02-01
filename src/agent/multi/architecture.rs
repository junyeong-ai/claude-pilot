//! Architecture agent for runtime boundary enforcement and convention validation.
//!
//! # Distinction from ArchitectAgent
//!
//! - **ArchitectureAgent** (this): Runtime enforcement agent that detects boundary violations,
//!   convention breaches, and layer dependency issues based on project-specific rules.
//!   Uses `Architecture` role category.
//!
//! - **ArchitectAgent**: High-level design advisor that validates architectural decisions
//!   for consistency, cohesion, and maintainability. Participates in consensus as `Advisor` role.

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureViolation {
    pub violation_type: ViolationType,
    pub file: PathBuf,
    pub description: String,
    pub severity: ViolationSeverity,
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
pub struct FileChange {
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

pub struct ArchitectureAgent {
    core: AgentCore,
    modules: Vec<Module>,
    system_prompt: String,
    module_boundaries: HashMap<String, ModuleBoundary>,
}

#[derive(Debug, Clone)]
struct ModuleBoundary {
    files: HashSet<PathBuf>,
    allowed_dependencies: HashSet<String>,
}

impl ArchitectureAgent {
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

    pub fn validate_changes(&self, changes: &[FileChange]) -> ArchitectureValidation {
        let mut violations = Vec::new();
        let mut warnings = Vec::new();

        violations.extend(self.check_boundary_violations(changes));
        violations.extend(self.check_dependency_violations(changes));

        if let Some(circular) = self.detect_circular_dependencies(changes) {
            violations.push(circular);
        }

        let cross_module = self.identify_cross_module_changes(changes);
        if cross_module.len() > 1 {
            warnings.push(format!(
                "Changes span {} modules: {}. Ensure coordination.",
                cross_module.len(),
                cross_module.join(", ")
            ));
        }

        let passed = violations
            .iter()
            .all(|v| v.severity < ViolationSeverity::Error);

        let summary = if passed {
            if violations.is_empty() && warnings.is_empty() {
                "Architecture validation passed with no issues.".to_string()
            } else {
                format!(
                    "Architecture validation passed with {} warnings.",
                    violations.len() + warnings.len()
                )
            }
        } else {
            let error_count = violations
                .iter()
                .filter(|v| v.severity >= ViolationSeverity::Error)
                .count();
            format!(
                "Architecture validation FAILED: {} errors, {} warnings.",
                error_count,
                violations.len() - error_count
            )
        };

        ArchitectureValidation {
            passed,
            violations,
            warnings,
            summary,
        }
    }

    fn check_boundary_violations(&self, changes: &[FileChange]) -> Vec<ArchitectureViolation> {
        let mut violations = Vec::new();

        for change in changes {
            if let Some((module_name, boundary)) = change
                .module
                .as_ref()
                .and_then(|m| self.module_boundaries.get(m).map(|b| (m, b)))
            {
                if !self.is_file_in_boundary(&change.path, boundary) {
                    violations.push(ArchitectureViolation {
                        violation_type: ViolationType::BoundaryViolation,
                        file: change.path.clone(),
                        description: format!(
                            "Module '{}' attempted to modify file outside its boundary",
                            module_name
                        ),
                        severity: ViolationSeverity::Error,
                        suggested_fix: Some(format!(
                            "Move this change to the module that owns '{}'",
                            change.path.display()
                        )),
                        affected_modules: vec![module_name.clone()],
                    });
                }
            } else if change.module.is_none() {
                let owning_modules: Vec<_> = self
                    .module_boundaries
                    .iter()
                    .filter(|(_, boundary)| self.is_file_in_boundary(&change.path, boundary))
                    .map(|(name, _)| name.clone())
                    .collect();

                if owning_modules.len() > 1 {
                    violations.push(ArchitectureViolation {
                        violation_type: ViolationType::BoundaryViolation,
                        file: change.path.clone(),
                        description: format!(
                            "File belongs to multiple modules: {}",
                            owning_modules.join(", ")
                        ),
                        severity: ViolationSeverity::Warning,
                        suggested_fix: Some(
                            "Clarify module ownership or move file to single module".to_string(),
                        ),
                        affected_modules: owning_modules,
                    });
                }
            }
        }

        violations
    }

    fn check_dependency_violations(&self, changes: &[FileChange]) -> Vec<ArchitectureViolation> {
        let mut violations = Vec::new();

        let mut module_changes: HashMap<String, Vec<&FileChange>> = HashMap::new();
        for change in changes {
            if let Some(module) = &change.module {
                module_changes
                    .entry(module.clone())
                    .or_default()
                    .push(change);
            }
        }

        for (module_name, module_files) in &module_changes {
            if let Some(boundary) = self.module_boundaries.get(module_name) {
                for change in module_files {
                    for (other_module, other_boundary) in &self.module_boundaries {
                        if other_module != module_name
                            && self.is_file_in_boundary(&change.path, other_boundary)
                            && !boundary.allowed_dependencies.contains(other_module)
                        {
                            violations.push(ArchitectureViolation {
                                violation_type: ViolationType::UnauthorizedDependency,
                                file: change.path.clone(),
                                description: format!(
                                    "Module '{}' modified file from '{}' without declared dependency",
                                    module_name, other_module
                                ),
                                severity: ViolationSeverity::Error,
                                suggested_fix: Some(format!(
                                    "Add '{}' to {}'s dependencies or coordinate with {} agent",
                                    other_module, module_name, other_module
                                )),
                                affected_modules: vec![module_name.clone(), other_module.clone()],
                            });
                        }
                    }
                }
            }
        }

        violations
    }

    fn detect_circular_dependencies(
        &self,
        changes: &[FileChange],
    ) -> Option<ArchitectureViolation> {
        let mut graph: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (name, boundary) in &self.module_boundaries {
            let deps: HashSet<&str> = boundary
                .allowed_dependencies
                .iter()
                .map(|s| s.as_str())
                .collect();
            graph.insert(name.as_str(), deps);
        }

        let mut inferred_deps: HashMap<String, HashSet<String>> = HashMap::new();
        for change in changes {
            if let Some(source_module) = &change.module {
                for (target_module, boundary) in &self.module_boundaries {
                    if target_module != source_module
                        && self.is_file_in_boundary(&change.path, boundary)
                    {
                        inferred_deps
                            .entry(source_module.clone())
                            .or_default()
                            .insert(target_module.clone());
                    }
                }
            }
        }

        let mut combined_graph: HashMap<String, HashSet<String>> = HashMap::new();

        for (name, deps) in &graph {
            let deps_owned: HashSet<String> = deps.iter().map(|s| s.to_string()).collect();
            combined_graph.insert(name.to_string(), deps_owned);
        }

        for (source, targets) in inferred_deps {
            combined_graph.entry(source).or_default().extend(targets);
        }

        let modules: Vec<String> = combined_graph.keys().cloned().collect();
        for start in &modules {
            let mut visited = HashSet::new();
            let mut path = Vec::new();
            if self.has_cycle_owned(&combined_graph, start, &mut visited, &mut path) {
                let affected: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                let is_new_cycle = !self.has_preexisting_cycle(&graph, &path);

                return Some(ArchitectureViolation {
                    violation_type: ViolationType::CircularDependency,
                    file: PathBuf::new(),
                    description: if is_new_cycle {
                        format!(
                            "Changes introduce circular dependency: {}",
                            affected.join(" → ")
                        )
                    } else {
                        format!(
                            "Existing circular dependency detected: {}",
                            affected.join(" → ")
                        )
                    },
                    severity: ViolationSeverity::Critical,
                    suggested_fix: Some(
                        "Break the cycle by removing or inverting a dependency".to_string(),
                    ),
                    affected_modules: affected,
                });
            }
        }

        None
    }

    fn has_cycle_owned(
        &self,
        graph: &HashMap<String, HashSet<String>>,
        node: &str,
        visited: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> bool {
        if path.contains(&node.to_string()) {
            path.push(node.to_string());
            return true;
        }
        if visited.contains(node) {
            return false;
        }

        visited.insert(node.to_string());
        path.push(node.to_string());

        if let Some(neighbors) = graph.get(node) {
            for neighbor in neighbors {
                if self.has_cycle_owned(graph, neighbor, visited, path) {
                    return true;
                }
            }
        }

        path.pop();
        false
    }

    fn has_preexisting_cycle(
        &self,
        graph: &HashMap<&str, HashSet<&str>>,
        cycle_path: &[String],
    ) -> bool {
        if cycle_path.len() < 2 {
            return false;
        }

        for i in 0..cycle_path.len() - 1 {
            let from = cycle_path[i].as_str();
            let to = cycle_path[i + 1].as_str();

            if let Some(deps) = graph.get(from) {
                if !deps.contains(to) {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }

    fn identify_cross_module_changes(&self, changes: &[FileChange]) -> Vec<String> {
        let mut modules = HashSet::new();

        for change in changes {
            if let Some(module) = &change.module {
                modules.insert(module.clone());
            } else {
                for (name, boundary) in &self.module_boundaries {
                    if self.is_file_in_boundary(&change.path, boundary) {
                        modules.insert(name.clone());
                        break;
                    }
                }
            }
        }

        modules.into_iter().collect()
    }

    fn is_file_in_boundary(&self, path: &Path, boundary: &ModuleBoundary) -> bool {
        if boundary.files.is_empty() {
            return false;
        }

        let files: Vec<PathBuf> = boundary.files.iter().cloned().collect();
        modmap::is_path_in_scope(path, &files)
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
                    "critical" => ViolationSeverity::Critical,
                    "error" => ViolationSeverity::Error,
                    "warning" => ViolationSeverity::Warning,
                    _ => ViolationSeverity::Info,
                })
                .unwrap_or(ViolationSeverity::Warning);

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
impl SpecializedAgent for ArchitectureAgent {
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

                        if v.severity >= ViolationSeverity::Error {
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
                    .all(|v| v.severity < ViolationSeverity::Error);
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

#[cfg(test)]
mod tests {
    use super::*;
    use modmap::{DetectedLanguage, ModuleDependency, ModuleMetrics};

    fn create_test_modules() -> (Vec<Module>, Vec<DetectedLanguage>, WorkspaceType) {
        let modules = vec![
            Module {
                id: "auth".into(),
                name: "auth".into(),
                paths: vec!["src/auth/".into()],
                key_files: vec![],
                dependencies: vec![ModuleDependency::new("db")],
                dependents: vec![],
                responsibility: "Authentication".into(),
                primary_language: "Rust".into(),
                metrics: ModuleMetrics::default(),
                conventions: vec![],
                known_issues: vec![],
                evidence: vec![],
            },
            Module {
                id: "db".into(),
                name: "db".into(),
                paths: vec!["src/db/".into()],
                key_files: vec![],
                dependencies: vec![],
                dependents: vec![],
                responsibility: "Database".into(),
                primary_language: "Rust".into(),
                metrics: ModuleMetrics::default(),
                conventions: vec![],
                known_issues: vec![],
                evidence: vec![],
            },
            Module {
                id: "api".into(),
                name: "api".into(),
                paths: vec!["src/api/".into()],
                key_files: vec![],
                dependencies: vec![ModuleDependency::new("auth"), ModuleDependency::new("db")],
                dependents: vec![],
                responsibility: "API Layer".into(),
                primary_language: "Rust".into(),
                metrics: ModuleMetrics::default(),
                conventions: vec![],
                known_issues: vec![],
                evidence: vec![],
            },
        ];
        let languages =
            vec![DetectedLanguage::new("Rust").with_marker_files(vec!["Cargo.toml".into()])];
        (modules, languages, WorkspaceType::SinglePackage)
    }

    fn create_test_agent() -> ArchitectureAgent {
        let (modules, languages, ws_type) = create_test_modules();
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        ArchitectureAgent::from_modules(modules, languages, ws_type, task_agent)
    }

    #[test]
    fn test_architecture_agent_role() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = ArchitectureAgent::from_modules(
            vec![],
            vec![],
            WorkspaceType::SinglePackage,
            task_agent,
        );
        assert_eq!(agent.role(), &AgentRole::architect());
    }

    #[test]
    fn test_validate_changes_no_violations() {
        let agent = create_test_agent();

        let changes = vec![FileChange {
            path: PathBuf::from("src/auth/login.rs"),
            change_type: ChangeType::Modified,
            module: Some("auth".into()),
        }];

        let validation = agent.validate_changes(&changes);
        assert!(validation.passed);
        assert!(validation.violations.is_empty());
    }

    #[test]
    fn test_validate_changes_boundary_violation() {
        let agent = create_test_agent();

        let changes = vec![FileChange {
            path: PathBuf::from("src/db/pool.rs"),
            change_type: ChangeType::Modified,
            module: Some("auth".into()),
        }];

        let validation = agent.validate_changes(&changes);
        assert!(!validation.passed);
        assert!(!validation.violations.is_empty());
        assert_eq!(
            validation.violations[0].violation_type,
            ViolationType::BoundaryViolation
        );
    }

    #[test]
    fn test_validate_changes_unauthorized_dependency() {
        let agent = create_test_agent();

        let changes = vec![FileChange {
            path: PathBuf::from("src/api/routes.rs"),
            change_type: ChangeType::Modified,
            module: Some("db".into()),
        }];

        let validation = agent.validate_changes(&changes);
        assert!(!validation.passed);

        let unauthorized: Vec<_> = validation
            .violations
            .iter()
            .filter(|v| v.violation_type == ViolationType::UnauthorizedDependency)
            .collect();
        assert!(!unauthorized.is_empty());
    }

    #[test]
    fn test_cross_module_detection() {
        let agent = create_test_agent();

        let changes = vec![
            FileChange {
                path: PathBuf::from("src/auth/login.rs"),
                change_type: ChangeType::Modified,
                module: Some("auth".into()),
            },
            FileChange {
                path: PathBuf::from("src/db/pool.rs"),
                change_type: ChangeType::Modified,
                module: Some("db".into()),
            },
        ];

        let validation = agent.validate_changes(&changes);
        assert!(validation.warnings.iter().any(|w| w.contains("span")));
    }

    #[test]
    fn test_parse_violations_from_output() {
        let output = r#"
Found issues:
VIOLATION: type="boundary_violation" | severity="error" | file="src/auth/login.rs" | description="Auth modified db file" | fix="Move to db module"
VIOLATION: type="convention_violation" | severity="warning" | file="src/api/routes.rs" | description="Missing error handling"
"#;

        let violations = ArchitectureAgent::parse_violations_from_output(output);
        assert_eq!(violations.len(), 2);
        assert_eq!(
            violations[0].violation_type,
            ViolationType::BoundaryViolation
        );
        assert_eq!(violations[0].severity, ViolationSeverity::Error);
        assert_eq!(violations[1].severity, ViolationSeverity::Warning);
    }

    #[test]
    fn test_system_prompt_contains_key_sections() {
        let agent = create_test_agent();
        let prompt = agent.system_prompt();

        assert!(prompt.contains("Boundary Enforcement"));
        assert!(prompt.contains("Dependency Direction"));
        assert!(prompt.contains("VIOLATION:"));
        assert!(prompt.contains("auth"));
        assert!(prompt.contains("db"));
        assert!(prompt.contains("api"));
    }

    #[test]
    fn test_is_file_in_boundary() {
        let agent = create_test_agent();
        let auth_boundary = &agent.module_boundaries["auth"];

        assert!(agent.is_file_in_boundary(Path::new("src/auth/login.rs"), auth_boundary));
        assert!(agent.is_file_in_boundary(Path::new("src/auth/session/token.rs"), auth_boundary));
        assert!(!agent.is_file_in_boundary(Path::new("src/db/pool.rs"), auth_boundary));
    }

    #[test]
    fn test_is_file_in_boundary_empty_files() {
        let agent = create_test_agent();

        let empty_boundary = ModuleBoundary {
            files: HashSet::new(),
            allowed_dependencies: HashSet::new(),
        };

        assert!(!agent.is_file_in_boundary(Path::new("src/auth/login.rs"), &empty_boundary));
        assert!(!agent.is_file_in_boundary(Path::new("src/db/pool.rs"), &empty_boundary));
        assert!(!agent.is_file_in_boundary(Path::new("any/random/path.rs"), &empty_boundary));
    }

    #[test]
    fn test_detect_circular_dependency_no_cycle() {
        let agent = create_test_agent();

        let changes = vec![FileChange {
            path: PathBuf::from("src/auth/login.rs"),
            change_type: ChangeType::Modified,
            module: Some("auth".into()),
        }];

        let cycle = agent.detect_circular_dependencies(&changes);
        assert!(
            cycle.is_none(),
            "Should not detect cycle in valid dependency graph"
        );
    }

    #[test]
    fn test_detect_circular_dependency_with_inferred_cycle() {
        let agent = create_test_agent();

        let changes = vec![FileChange {
            path: PathBuf::from("src/auth/session.rs"),
            change_type: ChangeType::Modified,
            module: Some("db".into()),
        }];

        let cycle = agent.detect_circular_dependencies(&changes);
        assert!(
            cycle.is_some(),
            "Should detect cycle when db modifies auth (creating auth → db → auth)"
        );

        let violation = cycle.unwrap();
        assert_eq!(violation.violation_type, ViolationType::CircularDependency);
        assert_eq!(violation.severity, ViolationSeverity::Critical);
        assert!(violation.description.contains("introduce"));
    }

    #[test]
    fn test_has_preexisting_cycle() {
        let agent = create_test_agent();

        let mut graph: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (name, boundary) in &agent.module_boundaries {
            let deps: HashSet<&str> = boundary
                .allowed_dependencies
                .iter()
                .map(|s| s.as_str())
                .collect();
            graph.insert(name.as_str(), deps);
        }

        let path_exists = vec!["auth".to_string(), "db".to_string()];
        assert!(agent.has_preexisting_cycle(&graph, &path_exists));

        let path_not_exists = vec!["db".to_string(), "auth".to_string()];
        assert!(!agent.has_preexisting_cycle(&graph, &path_not_exists));
    }
}
