use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::super::artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
use super::super::graph::detect_cycle;
use super::super::Evidence;
use super::types::{CheckDetail, QualityTier, ValidationCheckResult};
use super::PlanValidator;

impl PlanValidator {
    pub(super) fn check_evidence_quality(
        &self,
        evidence: &Evidence,
    ) -> ValidationCheckResult {
        let quality = evidence.validate_quality(&self.quality_config);
        let min_quality = self.quality_config.min_evidence_quality;
        let min_confidence = self.quality_config.min_evidence_confidence;
        let mut details = Vec::new();

        let yellow_quality_threshold = min_quality * 0.7;
        let yellow_confidence_threshold = min_confidence * 0.7;

        let quality_tier = if quality.quality_score >= min_quality {
            QualityTier::Green
        } else if quality.quality_score >= yellow_quality_threshold {
            QualityTier::Yellow
        } else {
            QualityTier::Red
        };

        let confidence_tier = if quality.average_confidence >= min_confidence {
            QualityTier::Green
        } else if quality.average_confidence >= yellow_confidence_threshold {
            QualityTier::Yellow
        } else {
            QualityTier::Red
        };

        let tier = match (quality_tier, confidence_tier) {
            (QualityTier::Red, _) | (_, QualityTier::Red) => QualityTier::Red,
            (QualityTier::Yellow, _) | (_, QualityTier::Yellow) => QualityTier::Yellow,
            _ => QualityTier::Green,
        };

        details.push(CheckDetail::Info(format!(
            "Evidence quality: {:.1}% (tier: {:?})",
            quality.quality_score * 100.0,
            tier
        )));

        match tier {
            QualityTier::Green => {
                details.push(CheckDetail::Info(format!(
                    "Quality {:.1}% meets threshold {:.0}%",
                    quality.quality_score * 100.0,
                    min_quality * 100.0
                )));
            }
            QualityTier::Yellow => {
                details.push(CheckDetail::Warning(format!(
                    "Quality {:.1}% below preferred {:.0}% - LLM should assess if sufficient for this task",
                    quality.quality_score * 100.0,
                    min_quality * 100.0
                )));
            }
            QualityTier::Red => {
                details.push(CheckDetail::Error(format!(
                    "Quality {:.1}% significantly below threshold {:.0}% - evidence likely insufficient",
                    quality.quality_score * 100.0,
                    min_quality * 100.0
                )));
            }
        }

        details.push(CheckDetail::Info(format!(
            "{}/{} evidence items verifiable ({:.0}%)",
            quality.verifiable_items,
            quality.total_evidence_items,
            if quality.total_evidence_items > 0 {
                (quality.verifiable_items as f32 / quality.total_evidence_items as f32) * 100.0
            } else {
                0.0
            }
        )));

        if confidence_tier == QualityTier::Yellow {
            details.push(CheckDetail::Warning(format!(
                "Average confidence {:.0}% below preferred {:.0}% - proceed with caution",
                quality.average_confidence * 100.0,
                min_confidence * 100.0
            )));
        } else if confidence_tier == QualityTier::Red {
            details.push(CheckDetail::Error(format!(
                "Average confidence {:.0}% significantly below threshold {:.0}%",
                quality.average_confidence * 100.0,
                min_confidence * 100.0
            )));
        }

        if self.quality_config.require_verifiable_evidence && quality.verifiable_items == 0 {
            details.push(CheckDetail::Error(
                "No verifiable evidence items found".to_string(),
            ));
            return ValidationCheckResult {
                passed: false,
                tier: QualityTier::Red,
                details,
            };
        }

        for item in &quality.low_confidence_items {
            details.push(CheckDetail::Warning(format!("Low confidence: {}", item)));
        }

        ValidationCheckResult {
            passed: tier.allows_proceed(),
            tier,
            details,
        }
    }

    pub(super) fn check_spec_plan_consistency(
        &self,
        spec: &SpecArtifact,
        tasks: &TasksArtifact,
    ) -> ValidationCheckResult {
        let mut details = Vec::new();
        let mut passed = true;

        for story in &spec.user_stories {
            let has_tasks = tasks.phases.iter().any(|p| {
                p.tasks
                    .iter()
                    .any(|t| t.user_story.as_ref() == Some(&story.id))
            });

            if has_tasks {
                details.push(CheckDetail::Info(format!(
                    "User story {} has tasks",
                    story.id
                )));
            } else {
                details.push(CheckDetail::Error(format!(
                    "User story {} missing tasks",
                    story.id
                )));
                passed = false;
            }
        }

        let total_tasks = tasks.total_tasks();
        if total_tasks == 0 {
            details.push(CheckDetail::Error("No tasks defined".to_string()));
            passed = false;
        } else {
            details.push(CheckDetail::Info(format!(
                "{} tasks across {} phases",
                total_tasks,
                tasks.phases.len()
            )));
        }

        ValidationCheckResult {
            passed,
            tier: if passed {
                QualityTier::Green
            } else {
                QualityTier::Red
            },
            details,
        }
    }

    pub(super) fn check_evidence_alignment(
        &self,
        evidence: &Evidence,
        plan: &PlanArtifact,
        tasks: &TasksArtifact,
    ) -> ValidationCheckResult {
        let mut details = Vec::new();
        let mut passed = true;
        let coverage_threshold = self.quality_config.evidence_coverage_threshold;

        if evidence.codebase_analysis.relevant_files.is_empty() {
            details.push(CheckDetail::Error(
                "No relevant files found in codebase analysis".to_string(),
            ));
            passed = false;
        } else {
            details.push(CheckDetail::Info(format!(
                "{} relevant files identified",
                evidence.codebase_analysis.relevant_files.len()
            )));
        }

        let plan_paths: HashSet<&str> = plan
            .project_structure
            .affected_paths
            .iter()
            .map(|s| s.as_str())
            .collect();

        let evidence_areas: HashSet<&str> = evidence
            .codebase_analysis
            .affected_areas
            .iter()
            .map(|s| s.as_str())
            .collect();

        let covered_count = plan_paths
            .iter()
            .filter(|path| path_matches_any_area(path, &evidence_areas))
            .count();

        if !plan_paths.is_empty() {
            let coverage_ratio = covered_count as f32 / plan_paths.len() as f32;
            details.push(CheckDetail::Info(format!(
                "Plan path coverage: {:.0}% ({}/{} paths)",
                coverage_ratio * 100.0,
                covered_count,
                plan_paths.len()
            )));

            if coverage_ratio < coverage_threshold {
                details.push(CheckDetail::Error(format!(
                    "Plan coverage {:.0}% below threshold {:.0}%",
                    coverage_ratio * 100.0,
                    coverage_threshold * 100.0
                )));
                passed = false;
            }
        }

        let evidence_files: HashSet<&str> = evidence
            .codebase_analysis
            .relevant_files
            .iter()
            .map(|f| f.path.as_str())
            .collect();

        let plan_new_paths: HashSet<&str> = plan
            .project_structure
            .new_paths
            .iter()
            .map(|s| s.as_str())
            .collect();

        for new_path in &plan_new_paths {
            let path = Path::new(new_path);

            if !is_safe_relative_path(path) {
                details.push(CheckDetail::Error(format!(
                    "new_paths contains unsafe path '{}' (must be relative, no .. traversal)",
                    new_path
                )));
                passed = false;
                continue;
            }

            if new_path.ends_with('/') {
                details.push(CheckDetail::Error(format!(
                    "new_paths contains directory '{}' (must be file paths only)",
                    new_path
                )));
                passed = false;
                continue;
            }

            if !is_new_path_safe(path, &self.working_dir) {
                details.push(CheckDetail::Error(format!(
                    "new_paths '{}' escapes working directory",
                    new_path
                )));
                passed = false;
                continue;
            }

            let full_path = self.working_dir.join(path);

            if full_path.exists() {
                details.push(CheckDetail::Error(format!(
                    "new_paths contains existing path '{}' (must be files to create)",
                    new_path
                )));
                passed = false;
            }
        }

        let mut tasks_with_evidence = 0;
        let mut tasks_without_evidence = Vec::new();
        let mut new_file_count = 0;

        for phase in &tasks.phases {
            for task in &phase.tasks {
                if task.affected_files.is_empty() {
                    continue;
                }

                let (has_backing, is_new) = task
                    .affected_files
                    .iter()
                    .map(|f| {
                        path_has_evidence_backing(
                            f,
                            &evidence_files,
                            &plan_paths,
                            &plan_new_paths,
                            &evidence_areas,
                            &self.working_dir,
                        )
                    })
                    .fold((false, false), |(acc_b, acc_n), (b, n)| {
                        (acc_b || b, acc_n || n)
                    });

                if is_new {
                    new_file_count += 1;
                }

                if has_backing {
                    tasks_with_evidence += 1;
                } else {
                    tasks_without_evidence.push(&task.id);
                }
            }
        }

        if new_file_count > 0 {
            details.push(CheckDetail::Info(format!(
                "{} tasks involve planned new files",
                new_file_count
            )));
        }

        let tasks_with_files = tasks_with_evidence + tasks_without_evidence.len();
        if tasks_with_files > 0 {
            let task_coverage_ratio = tasks_with_evidence as f32 / tasks_with_files as f32;
            details.push(CheckDetail::Info(format!(
                "Task evidence coverage: {:.0}% ({}/{})",
                task_coverage_ratio * 100.0,
                tasks_with_evidence,
                tasks_with_files
            )));

            if task_coverage_ratio < coverage_threshold {
                details.push(CheckDetail::Error(format!(
                    "Task coverage {:.0}% below threshold {:.0}%",
                    task_coverage_ratio * 100.0,
                    coverage_threshold * 100.0
                )));
                if !tasks_without_evidence.is_empty() {
                    details.push(CheckDetail::Warning(format!(
                        "Tasks without evidence: {:?}",
                        tasks_without_evidence
                            .iter()
                            .take(self.quality_config.max_display_items)
                            .collect::<Vec<_>>()
                    )));
                }
                passed = false;
            }
        }

        if !evidence.codebase_analysis.existing_patterns.is_empty() {
            details.push(CheckDetail::Info(format!(
                "{} existing patterns found",
                evidence.codebase_analysis.existing_patterns.len()
            )));
        }

        ValidationCheckResult {
            passed,
            tier: if passed {
                QualityTier::Green
            } else {
                QualityTier::Red
            },
            details,
        }
    }

    pub(super) fn check_constitution_compliance(
        &self,
        plan: &PlanArtifact,
        evidence: &Evidence,
    ) -> ValidationCheckResult {
        let mut details = Vec::new();
        let mut passed = true;
        let min_quality = self.quality_config.min_evidence_quality;

        let check = &plan.constitution_check;

        if check.fact_based.passed {
            let quality = evidence.validate_quality(&self.quality_config);
            if quality.quality_score < min_quality {
                details.push(CheckDetail::Error(format!(
                    "Fact-Based claim not verified: evidence quality {:.0}% insufficient",
                    quality.quality_score * 100.0
                )));
                passed = false;
            } else {
                details.push(CheckDetail::Info(format!(
                    "Fact-Based verified with {:.0}% evidence quality",
                    quality.quality_score * 100.0
                )));
            }
        } else {
            details.push(CheckDetail::Error(format!(
                "Fact-Based: {}",
                check.fact_based.note.as_deref().unwrap_or("Failed")
            )));
            passed = false;
        }

        if !check.dry_principle.passed {
            details.push(CheckDetail::Error(format!(
                "DRY: {}",
                check.dry_principle.note.as_deref().unwrap_or("Failed")
            )));
            passed = false;
        }

        if !check.minimal_scope.passed {
            details.push(CheckDetail::Error(format!(
                "Minimal Scope: {}",
                check.minimal_scope.note.as_deref().unwrap_or("Failed")
            )));
            passed = false;
        }

        if !check.intellectual_honesty.passed {
            details.push(CheckDetail::Error(format!(
                "Intellectual Honesty: {}",
                check
                    .intellectual_honesty
                    .note
                    .as_deref()
                    .unwrap_or("Failed")
            )));
            passed = false;
        }

        if passed {
            details.push(CheckDetail::Info(
                "All constitution principles satisfied".to_string(),
            ));
        }

        ValidationCheckResult {
            passed,
            tier: if passed {
                QualityTier::Green
            } else {
                QualityTier::Red
            },
            details,
        }
    }

    pub(super) fn check_execution_feasibility(
        &self,
        tasks: &TasksArtifact,
    ) -> ValidationCheckResult {
        let mut details = Vec::new();
        let mut passed = true;

        let deps = tasks.to_dependency_map();
        if let Some(cycle) = detect_cycle(&deps) {
            details.push(CheckDetail::Error(format!(
                "Circular dependency: {}",
                cycle.join(" -> ")
            )));
            passed = false;
        }

        for phase in &tasks.phases {
            for task in &phase.tasks {
                for dep in &task.dependencies {
                    let dep_exists = tasks
                        .phases
                        .iter()
                        .any(|p| p.tasks.iter().any(|t| &t.id == dep));
                    if !dep_exists {
                        details.push(CheckDetail::Error(format!(
                            "Task {} depends on non-existent task {}",
                            task.id, dep
                        )));
                        passed = false;
                    }
                }
            }
        }

        let has_checkpoints = tasks.phases.iter().all(|p| p.checkpoint.is_some());
        if has_checkpoints {
            details.push(CheckDetail::Info("All phases have checkpoints".to_string()));
        } else {
            details.push(CheckDetail::Warning(
                "Some phases missing checkpoints".to_string(),
            ));
        }

        let parallel_count = tasks.parallel_tasks();
        if parallel_count > 0 {
            details.push(CheckDetail::Info(format!(
                "{} tasks can run in parallel",
                parallel_count
            )));
        }

        ValidationCheckResult {
            passed,
            tier: if passed {
                QualityTier::Green
            } else {
                QualityTier::Red
            },
            details,
        }
    }
}

fn paths_overlap(p1: &Path, p2: &Path) -> bool {
    p1 == p2 || p1.starts_with(p2) || p2.starts_with(p1)
}

fn path_matches_any_area(path: &str, areas: &HashSet<&str>) -> bool {
    let path = Path::new(path);
    areas
        .iter()
        .any(|area| paths_overlap(path, Path::new(area)))
}

fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut components = Vec::new();
    let mut leading_parents = 0usize;
    let mut saw_normal = false;

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if saw_normal {
                    if components.is_empty() {
                        return None;
                    }
                    components.pop();
                } else {
                    leading_parents += 1;
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(c) => {
                if leading_parents > 0 {
                    return None;
                }
                saw_normal = true;
                components.push(c);
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return None;
            }
        }
    }

    if components.is_empty() {
        return None;
    }

    Some(components.iter().collect())
}

pub(super) fn is_safe_relative_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    normalize_path(path).is_some()
}

fn is_new_path_safe(path: &Path, working_dir: &Path) -> bool {
    let Some(normalized) = normalize_path(path) else {
        return false;
    };

    let full_path = working_dir.join(&normalized);
    let Ok(canonical_working) = working_dir.canonicalize() else {
        return false;
    };

    let mut current = full_path.as_path();
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(meta) => {
                if meta.is_symlink() {
                    match current.canonicalize() {
                        Ok(resolved) if resolved.starts_with(&canonical_working) => {
                            if current != full_path && !resolved.is_dir() {
                                return false;
                            }
                            return true;
                        }
                        _ => return false,
                    }
                }
                if meta.is_file() && current != full_path {
                    return false;
                }
                match current.canonicalize() {
                    Ok(resolved) => return resolved.starts_with(&canonical_working),
                    Err(_) => return false,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => current = parent,
                _ => return false,
            },
            Err(_) => return false,
        }
    }
}

fn path_has_evidence_backing(
    file: &str,
    evidence_files: &HashSet<&str>,
    plan_paths: &HashSet<&str>,
    new_paths: &HashSet<&str>,
    evidence_areas: &HashSet<&str>,
    working_dir: &Path,
) -> (bool, bool) {
    let file_path = Path::new(file);

    if !is_safe_relative_path(file_path) {
        return (false, false);
    }

    let normalized_file = normalize_path(file_path);
    let is_new = normalized_file.as_ref().is_some_and(|nf| {
        new_paths
            .iter()
            .filter_map(|np| normalize_path(Path::new(np)))
            .any(|nnp| nf == &nnp)
    });
    if is_new {
        if !is_new_path_safe(file_path, working_dir) {
            return (false, false);
        }
        return (true, true);
    }

    let in_evidence = evidence_files
        .iter()
        .any(|ef| paths_overlap(file_path, Path::new(ef)));

    if in_evidence {
        return (true, false);
    }

    let in_plan = plan_paths
        .iter()
        .any(|pp| paths_overlap(file_path, Path::new(pp)));

    let in_area = evidence_areas
        .iter()
        .any(|area| paths_overlap(file_path, Path::new(area)));

    if in_plan || in_area {
        let full_path = working_dir.join(file);
        if let (Ok(canonical), Ok(canonical_working)) =
            (full_path.canonicalize(), working_dir.canonicalize())
            && canonical.starts_with(&canonical_working)
        {
            return (true, false);
        }
        return (false, false);
    }

    (false, false)
}
