//! Anti-Drift mechanism for task scope tracking.
//!
//! Detects when task execution deviates from expected scope:
//! - Unexpected file modifications outside planned scope
//! - Cross-module changes that weren't anticipated
//! - Excessive context switches during execution
//! - Scope creep from accumulating unplanned changes

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::config::FocusConfig;
use crate::planning::{Evidence, PlanArtifact, TasksArtifact};
use crate::verification::FileChanges;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarningSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriftWarning {
    UnexpectedFile {
        file: PathBuf,
        severity: WarningSeverity,
        reason: String,
    },
    CrossModuleChange {
        source_module: String,
        target_module: String,
        files: Vec<PathBuf>,
    },
    TooManyContextSwitches {
        count: u32,
        threshold: u32,
        modules: Vec<String>,
    },
    ScopeCreep {
        expected_files: usize,
        actual_files: usize,
        unexpected: Vec<PathBuf>,
    },
}

impl DriftWarning {
    pub fn severity(&self) -> WarningSeverity {
        match self {
            Self::UnexpectedFile { severity, .. } => *severity,
            Self::CrossModuleChange { .. } => WarningSeverity::Medium,
            Self::TooManyContextSwitches {
                count, threshold, ..
            } => {
                if *count > threshold * 2 {
                    WarningSeverity::High
                } else {
                    WarningSeverity::Medium
                }
            }
            Self::ScopeCreep {
                expected_files,
                actual_files,
                ..
            } => {
                let ratio = *actual_files as f32 / (*expected_files).max(1) as f32;
                if ratio > 3.0 {
                    WarningSeverity::High
                } else if ratio > 2.0 {
                    WarningSeverity::Medium
                } else {
                    WarningSeverity::Low
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedScope {
    pub primary_files: HashSet<PathBuf>,
    pub secondary_files: HashSet<PathBuf>,
    pub expected_modules: HashSet<String>,
    pub task_id: String,
}

impl ExpectedScope {
    pub fn new(task_id: &str) -> Self {
        Self {
            primary_files: HashSet::new(),
            secondary_files: HashSet::new(),
            expected_modules: HashSet::new(),
            task_id: task_id.to_string(),
        }
    }

    pub fn is_expected(&self, path: &Path) -> bool {
        let path_buf = path.to_path_buf();
        self.primary_files.contains(&path_buf) || self.secondary_files.contains(&path_buf)
    }

    pub fn is_in_expected_module(&self, path: &Path) -> bool {
        let module = extract_module(path);
        self.expected_modules.contains(&module)
    }

    pub fn classify_file(&self, path: &Path) -> FileClassification {
        let path_buf = path.to_path_buf();
        if self.primary_files.contains(&path_buf) {
            FileClassification::Primary
        } else if self.secondary_files.contains(&path_buf) {
            FileClassification::Secondary
        } else if self.is_in_expected_module(path) {
            FileClassification::SameModule
        } else {
            FileClassification::Unexpected
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClassification {
    Primary,
    Secondary,
    SameModule,
    Unexpected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionValidation {
    pub task_id: String,
    pub expected_scope: ExpectedScope,
    pub actual_changes: FileChanges,
    pub warnings: Vec<DriftWarning>,
    pub context_switches: u32,
    pub modules_touched: Vec<String>,
    pub passed: bool,
}

pub struct FocusTracker {
    config: FocusConfig,
    active_scopes: HashMap<String, TaskScope>,
}

struct TaskScope {
    expected: ExpectedScope,
    context_switches: u32,
    last_module: Option<String>,
    modules_visited: Vec<String>,
}

impl FocusTracker {
    pub fn new(config: FocusConfig) -> Self {
        Self {
            config,
            active_scopes: HashMap::new(),
        }
    }

    pub fn derive_scope(
        &self,
        task_id: &str,
        plan: Option<&PlanArtifact>,
        tasks: Option<&TasksArtifact>,
        evidence: Option<&Evidence>,
    ) -> ExpectedScope {
        let mut scope = ExpectedScope::new(task_id);

        if self.config.derive_from_plan {
            if let Some(plan) = plan {
                for path in &plan.project_structure.affected_paths {
                    scope.primary_files.insert(PathBuf::from(path));
                    scope
                        .expected_modules
                        .insert(extract_module(Path::new(path)));
                }
                for path in &plan.project_structure.new_paths {
                    scope.primary_files.insert(PathBuf::from(path));
                    scope
                        .expected_modules
                        .insert(extract_module(Path::new(path)));
                }
            }

            if let Some(tasks_artifact) = tasks {
                for phase in &tasks_artifact.phases {
                    for task in &phase.tasks {
                        if task.id == task_id {
                            for path in &task.affected_files {
                                scope.primary_files.insert(PathBuf::from(path));
                                scope
                                    .expected_modules
                                    .insert(extract_module(Path::new(path)));
                            }
                        }
                    }
                }
            }
        }

        if self.config.derive_from_evidence
            && let Some(ev) = evidence
        {
            self.derive_from_evidence(&mut scope, ev);
        }

        scope
    }

    fn derive_from_evidence(&self, scope: &mut ExpectedScope, evidence: &Evidence) {
        for file in &evidence.codebase_analysis.relevant_files {
            if file.confidence >= self.config.evidence_confidence_threshold {
                let path = PathBuf::from(&file.path);
                scope.secondary_files.insert(path.clone());
                scope.expected_modules.insert(extract_module(&path));
            }
        }
    }

    pub fn start_task(&mut self, task_id: &str, scope: ExpectedScope) {
        if !self.config.enabled {
            return;
        }

        self.active_scopes.insert(
            task_id.to_string(),
            TaskScope {
                expected: scope,
                context_switches: 0,
                last_module: None,
                modules_visited: Vec::new(),
            },
        );
    }

    pub fn record_file_access(&mut self, task_id: &str, path: &Path) {
        if !self.config.enabled {
            return;
        }

        if let Some(scope) = self.active_scopes.get_mut(task_id) {
            let module = extract_module(path);

            if let Some(last) = &scope.last_module {
                if last != &module {
                    scope.context_switches += 1;
                    if !scope.modules_visited.contains(&module) {
                        scope.modules_visited.push(module.clone());
                    }
                }
            } else if !scope.modules_visited.contains(&module) {
                scope.modules_visited.push(module.clone());
            }

            scope.last_module = Some(module);
        }
    }

    pub fn validate_completion(
        &mut self,
        task_id: &str,
        changes: &FileChanges,
    ) -> CompletionValidation {
        let Some(scope) = self.active_scopes.remove(task_id) else {
            return CompletionValidation {
                task_id: task_id.to_string(),
                expected_scope: ExpectedScope::new(task_id),
                actual_changes: changes.clone(),
                warnings: vec![],
                context_switches: 0,
                modules_touched: vec![],
                passed: true,
            };
        };

        let mut warnings = Vec::new();
        let mut unexpected_files = Vec::new();

        let affected = changes.affected_files();
        for path in &affected {
            let classification = scope.expected.classify_file(path);
            match classification {
                FileClassification::Primary | FileClassification::Secondary => {}
                FileClassification::SameModule => {
                    if self.config.warn_on_unexpected_files {
                        warnings.push(DriftWarning::UnexpectedFile {
                            file: (*path).clone(),
                            severity: WarningSeverity::Low,
                            reason: "File in expected module but not explicitly listed".into(),
                        });
                    }
                }
                FileClassification::Unexpected => {
                    unexpected_files.push((*path).clone());
                    let severity = if self.config.strict_module_boundaries {
                        WarningSeverity::High
                    } else {
                        WarningSeverity::Medium
                    };
                    warnings.push(DriftWarning::UnexpectedFile {
                        file: (*path).clone(),
                        severity,
                        reason: "File outside expected scope".into(),
                    });
                }
            }
        }

        if scope.context_switches > self.config.max_context_switches {
            warnings.push(DriftWarning::TooManyContextSwitches {
                count: scope.context_switches,
                threshold: self.config.max_context_switches,
                modules: scope.modules_visited.clone(),
            });
        }

        let cross_module = self.detect_cross_module_changes(&scope, &affected);
        warnings.extend(cross_module);

        let expected_count =
            scope.expected.primary_files.len() + scope.expected.secondary_files.len();
        let actual_count = affected.len();
        if !unexpected_files.is_empty() && actual_count > expected_count.max(1) * 2 {
            warnings.push(DriftWarning::ScopeCreep {
                expected_files: expected_count,
                actual_files: actual_count,
                unexpected: unexpected_files,
            });
        }

        let has_high_severity = warnings
            .iter()
            .any(|w| w.severity() == WarningSeverity::High);

        if !warnings.is_empty() {
            for warning in &warnings {
                warn!(task_id = %task_id, ?warning, "Drift warning detected");
            }
        }

        CompletionValidation {
            task_id: task_id.to_string(),
            expected_scope: scope.expected,
            actual_changes: changes.clone(),
            warnings,
            context_switches: scope.context_switches,
            modules_touched: scope.modules_visited,
            passed: !has_high_severity,
        }
    }

    fn detect_cross_module_changes(
        &self,
        scope: &TaskScope,
        affected: &[&PathBuf],
    ) -> Vec<DriftWarning> {
        if !self.config.strict_module_boundaries {
            return vec![];
        }

        let mut module_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in affected {
            let module = extract_module(path);
            module_files
                .entry(module)
                .or_default()
                .push((*path).clone());
        }

        let mut warnings = Vec::new();
        let expected_modules: Vec<_> = scope.expected.expected_modules.iter().collect();

        for (module, files) in &module_files {
            if !scope.expected.expected_modules.contains(module) && !expected_modules.is_empty() {
                warnings.push(DriftWarning::CrossModuleChange {
                    source_module: expected_modules
                        .first()
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    target_module: module.clone(),
                    files: files.clone(),
                });
            }
        }

        warnings
    }
}

/// Extract module identifier from path for drift tracking.
///
/// Uses immediate parent directory as module. No heuristics, no guessing.
/// The LLM interprets semantic relationships; we just provide consistent grouping.
///
/// Examples:
/// - `src/verification/mod.rs` → "verification"
/// - `src/main.rs` → "src"
/// - `Cargo.toml` → "root"
fn extract_module(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(String::from)
        .unwrap_or_else(|| "root".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_module_uses_parent_directory() {
        // Uses immediate parent directory - no heuristics
        assert_eq!(
            extract_module(Path::new("src/verification/mod.rs")),
            "verification"
        );
        assert_eq!(
            extract_module(Path::new("src/agent/task_agent.rs")),
            "agent"
        );
        assert_eq!(extract_module(Path::new("lib/core/engine.rs")), "core");
        assert_eq!(extract_module(Path::new("pkg/server/main.go")), "server");
        assert_eq!(
            extract_module(Path::new("internal/auth/handler.go")),
            "auth"
        );
        // Files directly under a directory use that directory
        assert_eq!(extract_module(Path::new("src/main.rs")), "src");
        assert_eq!(extract_module(Path::new("lib/index.ts")), "lib");
        // Root files
        assert_eq!(extract_module(Path::new("Cargo.toml")), "root");
        assert_eq!(extract_module(Path::new("README.md")), "root");
    }

    #[test]
    fn test_expected_scope_classification() {
        let mut scope = ExpectedScope::new("test");
        scope.primary_files.insert(PathBuf::from("src/main.rs"));
        scope.secondary_files.insert(PathBuf::from("src/lib.rs"));
        scope.expected_modules.insert("verification".to_string());

        assert_eq!(
            scope.classify_file(Path::new("src/main.rs")),
            FileClassification::Primary
        );
        assert_eq!(
            scope.classify_file(Path::new("src/lib.rs")),
            FileClassification::Secondary
        );
        assert_eq!(
            scope.classify_file(Path::new("src/verification/mod.rs")),
            FileClassification::SameModule
        );
        assert_eq!(
            scope.classify_file(Path::new("src/other/mod.rs")),
            FileClassification::Unexpected
        );
    }

    #[test]
    fn test_focus_tracker_context_switches() {
        let config = FocusConfig {
            max_context_switches: 2,
            ..Default::default()
        };
        let mut tracker = FocusTracker::new(config);

        let scope = ExpectedScope::new("T1");
        tracker.start_task("T1", scope);

        tracker.record_file_access("T1", Path::new("src/agent/mod.rs"));
        tracker.record_file_access("T1", Path::new("src/verification/mod.rs"));
        tracker.record_file_access("T1", Path::new("src/planning/mod.rs"));
        tracker.record_file_access("T1", Path::new("src/recovery/mod.rs"));

        let changes = FileChanges {
            created: vec![],
            modified: vec![
                PathBuf::from("src/agent/mod.rs"),
                PathBuf::from("src/verification/mod.rs"),
                PathBuf::from("src/planning/mod.rs"),
                PathBuf::from("src/recovery/mod.rs"),
            ],
            deleted: vec![],
        };

        let validation = tracker.validate_completion("T1", &changes);
        assert_eq!(validation.context_switches, 3);
        assert!(
            validation
                .warnings
                .iter()
                .any(|w| matches!(w, DriftWarning::TooManyContextSwitches { .. }))
        );
    }

    #[test]
    fn test_drift_warning_severity() {
        let warning = DriftWarning::UnexpectedFile {
            file: PathBuf::from("test.rs"),
            severity: WarningSeverity::High,
            reason: "test".into(),
        };
        assert_eq!(warning.severity(), WarningSeverity::High);

        let warning = DriftWarning::ScopeCreep {
            expected_files: 2,
            actual_files: 10,
            unexpected: vec![],
        };
        assert_eq!(warning.severity(), WarningSeverity::High);
    }

    #[test]
    fn test_focus_tracker_disabled() {
        let config = FocusConfig {
            enabled: false,
            ..Default::default()
        };
        let mut tracker = FocusTracker::new(config);

        let scope = ExpectedScope::new("T1");
        tracker.start_task("T1", scope);

        let changes = FileChanges::default();
        let validation = tracker.validate_completion("T1", &changes);

        assert!(validation.passed);
        assert!(validation.warnings.is_empty());
    }
}
