//! Architectural validation: boundary checks, dependency analysis, cycle detection.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;

use super::{
    ArchitecturalFileChange, ArchitectureValidation, ArchitectureViolation, BoundaryEnforcementAgent,
    ModuleBoundary, ViolationType,
};
use crate::domain::Severity;

impl BoundaryEnforcementAgent {
    pub fn validate_changes(&self, changes: &[ArchitecturalFileChange]) -> ArchitectureValidation {
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
            .all(|v| v.severity < Severity::Error);

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
                .filter(|v| v.severity >= Severity::Error)
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

    fn check_boundary_violations(
        &self,
        changes: &[ArchitecturalFileChange],
    ) -> Vec<ArchitectureViolation> {
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
                        severity: Severity::Error,
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
                        severity: Severity::Warning,
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

    fn check_dependency_violations(
        &self,
        changes: &[ArchitecturalFileChange],
    ) -> Vec<ArchitectureViolation> {
        let mut violations = Vec::new();

        let mut module_changes: HashMap<String, Vec<&ArchitecturalFileChange>> = HashMap::new();
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
                                severity: Severity::Error,
                                suggested_fix: Some(format!(
                                    "Add '{}' to {}'s dependencies or coordinate with {} agent",
                                    other_module, module_name, other_module
                                )),
                                affected_modules: vec![
                                    module_name.clone(),
                                    other_module.clone(),
                                ],
                            });
                        }
                    }
                }
            }
        }

        violations
    }

    pub(super) fn detect_circular_dependencies(
        &self,
        changes: &[ArchitecturalFileChange],
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
                    severity: Severity::Critical,
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

    pub(super) fn has_preexisting_cycle(
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

    fn identify_cross_module_changes(&self, changes: &[ArchitecturalFileChange]) -> Vec<String> {
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

    pub(super) fn is_file_in_boundary(&self, path: &Path, boundary: &ModuleBoundary) -> bool {
        if boundary.files.is_empty() {
            return false;
        }

        let files: Vec<PathBuf> = boundary.files.iter().cloned().collect();
        modmap::is_path_in_scope(path, &files)
    }
}
