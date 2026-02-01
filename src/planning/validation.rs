use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::Evidence;
use super::EvidenceSufficiency;
use super::artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
use super::graph::detect_cycle;
use super::plan_agent::PlanAgent;
use crate::config::{AgentConfig, QualityConfig};
use crate::error::Result;
use crate::verification::IssueSeverity;

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub passed: bool,
    /// True when validation passes but with Yellow-tier quality.
    /// Orchestrator should consider task complexity before proceeding.
    pub requires_complexity_assessment: bool,
    /// Evidence sufficiency classification for clear completion criteria.
    pub evidence_sufficiency: EvidenceSufficiency,
    pub spec_plan_consistency: ValidationCheckResult,
    pub evidence_quality: ValidationCheckResult,
    pub evidence_alignment: ValidationCheckResult,
    pub constitution_compliance: ValidationCheckResult,
    pub execution_feasibility: ValidationCheckResult,
    pub issues: Vec<ValidationIssue>,
    pub warnings: Vec<String>,
    /// Actionable feedback for plan revision if validation failed.
    pub revision_feedback: Option<RevisionFeedback>,
}

/// Actionable feedback for planner to improve the plan.
#[derive(Debug, Clone)]
pub struct RevisionFeedback {
    pub suggestions: Vec<RevisionSuggestion>,
}

#[derive(Debug, Clone)]
pub struct RevisionSuggestion {
    pub category: RevisionCategory,
    pub problem: String,
    pub suggested_fix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionCategory {
    DependencyError,
    MissingEvidence,
    SpecMismatch,
    FeasibilityIssue,
    ScopeCreep,
}

impl RevisionFeedback {
    pub fn from_issues(issues: &[ValidationIssue], warnings: &[String]) -> Option<Self> {
        if issues.is_empty() && warnings.is_empty() {
            return None;
        }

        let mut suggestions = Vec::new();

        for issue in issues {
            let (category, suggested_fix) = Self::categorize_issue(issue);
            suggestions.push(RevisionSuggestion {
                category,
                problem: issue.description.clone(),
                suggested_fix,
            });
        }

        for warning in warnings {
            if warning.contains("Evidence gaps") {
                suggestions.push(RevisionSuggestion {
                    category: RevisionCategory::MissingEvidence,
                    problem: warning.clone(),
                    suggested_fix: "Consider if missing evidence is critical for task success"
                        .into(),
                });
            }
        }

        if suggestions.is_empty() {
            None
        } else {
            Some(Self { suggestions })
        }
    }

    /// Categorize issue using the explicit category field, not description parsing.
    /// The category field is set by our validation code, making this reliable.
    fn categorize_issue(issue: &ValidationIssue) -> (RevisionCategory, String) {
        // Use the explicit category field set by validation code
        let cat_lower = issue.category.to_lowercase();

        let (revision_cat, fix) =
            if cat_lower.contains("evidence") || cat_lower.contains("coverage") {
                (
                    RevisionCategory::MissingEvidence,
                    "Gather more evidence or adjust scope to available evidence",
                )
            } else if cat_lower.contains("depend") {
                (
                    RevisionCategory::DependencyError,
                    "Fix task dependencies or remove invalid references",
                )
            } else if cat_lower.contains("spec") || cat_lower.contains("alignment") {
                (
                    RevisionCategory::SpecMismatch,
                    "Align plan with specification requirements",
                )
            } else if cat_lower.contains("feasib") || cat_lower.contains("execution") {
                (
                    RevisionCategory::FeasibilityIssue,
                    "Revise approach or break into smaller tasks",
                )
            } else if cat_lower.contains("constitution") || cat_lower.contains("scope") {
                (
                    RevisionCategory::ScopeCreep,
                    "Review and constrain task scope",
                )
            } else {
                // Fallback: categorize based on severity for unknown categories
                match issue.severity {
                    IssueSeverity::Critical | IssueSeverity::Error => (
                        RevisionCategory::FeasibilityIssue,
                        "Address critical issue before proceeding",
                    ),
                    IssueSeverity::Warning | IssueSeverity::Info => (
                        RevisionCategory::MissingEvidence,
                        "Consider addressing this concern",
                    ),
                }
            };

        (revision_cat, fix.into())
    }

    /// Format feedback for LLM prompt context.
    pub fn format_for_llm(&self) -> String {
        if self.suggestions.is_empty() {
            return String::new();
        }

        let items: Vec<String> = self
            .suggestions
            .iter()
            .map(|s| format!("- {:?}: {} → {}", s.category, s.problem, s.suggested_fix))
            .collect();

        format!(
            "## Previous Validation Feedback\nAddress these issues in the revised plan:\n{}",
            items.join("\n")
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    /// High quality - proceed without concerns
    Green,
    /// Acceptable quality - proceed but LLM should be aware of gaps
    Yellow,
    /// Insufficient quality - should not proceed without intervention
    Red,
}

impl QualityTier {
    pub fn allows_proceed(&self) -> bool {
        matches!(self, Self::Green | Self::Yellow)
    }
}

#[derive(Debug, Clone)]
pub struct ValidationCheckResult {
    pub passed: bool,
    pub tier: QualityTier,
    pub details: Vec<CheckDetail>,
}

#[derive(Debug, Clone)]
pub enum CheckDetail {
    Info(String),
    Warning(String),
    Error(String),
}

impl CheckDetail {
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Info(m) | Self::Warning(m) | Self::Error(m) => m,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ValidationIssue {
    pub severity: IssueSeverity,
    pub category: String,
    pub description: String,
}

pub struct PlanValidator {
    executor: PlanAgent,
    quality_config: QualityConfig,
    working_dir: PathBuf,
}

impl PlanValidator {
    pub fn with_config(
        working_dir: &Path,
        agent_config: &AgentConfig,
        quality_config: QualityConfig,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            executor: PlanAgent::new(working_dir, agent_config)?,
            quality_config,
            working_dir: working_dir.to_path_buf(),
        })
    }

    pub async fn validate(
        &self,
        spec: &SpecArtifact,
        plan: &PlanArtifact,
        tasks: &TasksArtifact,
        evidence: &Evidence,
    ) -> Result<ValidationResult> {
        info!("Validating plan against spec and evidence");

        // Validate evidence sufficiency first (NON-NEGOTIABLE fact-based development)
        let evidence_sufficiency = evidence.validate_sufficiency(&self.quality_config);
        debug!(
            sufficiency = %evidence_sufficiency.description(),
            coverage = evidence_sufficiency.coverage(),
            "Evidence sufficiency"
        );

        let spec_plan = self.check_spec_plan_consistency(spec, tasks);
        let evidence_quality = self.check_evidence_quality(evidence);
        let evidence_align = self.check_evidence_alignment(evidence, plan, tasks);
        let constitution = self.check_constitution_compliance(plan, evidence);
        let feasibility = self.check_execution_feasibility(tasks);

        let mut issues = Vec::new();
        let mut warnings = Vec::new();

        if !evidence_sufficiency.allows_planning() {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Error,
                category: "Evidence Sufficiency".to_string(),
                description: evidence_sufficiency.description(),
            });
        } else {
            match &evidence_sufficiency {
                EvidenceSufficiency::Sufficient { missing, .. } if !missing.is_empty() => {
                    warnings.push(format!("Evidence gaps: {}", missing.join(", ")));
                }
                EvidenceSufficiency::SufficientButRisky {
                    missing,
                    risk_context,
                    coverage,
                } => {
                    // Risky tier - add warning but let LLM decide based on task complexity
                    warnings.push(format!(
                        "Risky evidence ({:.0}%): {} - LLM should assess if sufficient for task complexity",
                        coverage * 100.0,
                        risk_context
                    ));
                    if !missing.is_empty() {
                        warnings.push(format!("Evidence gaps: {}", missing.join(", ")));
                    }
                }
                _ => {}
            }
        }

        collect_issues_from(&spec_plan, "Spec-Plan Consistency", &mut issues);
        collect_issues_from(&evidence_quality, "Evidence Quality", &mut issues);
        collect_issues_from(&evidence_align, "Evidence Alignment", &mut issues);
        collect_issues_from(&constitution, "Constitution Compliance", &mut issues);
        collect_issues_from(&feasibility, "Execution Feasibility", &mut issues);

        if !spec.clarifications.iter().all(|c| c.resolved) {
            warnings.push("Some clarifications are still pending".to_string());
        }

        let passed = evidence_sufficiency.allows_planning()
            && spec_plan.passed
            && evidence_quality.passed
            && evidence_align.passed
            && constitution.passed
            && feasibility.passed;

        // Complexity assessment required when:
        // - Evidence quality is Yellow (acceptable but has gaps)
        // - Evidence sufficiency requires LLM assessment (SufficientButRisky)
        let requires_complexity_assessment = evidence_quality.tier == QualityTier::Yellow
            || evidence_sufficiency.requires_llm_assessment();

        if passed {
            if requires_complexity_assessment {
                info!("Plan validation PASSED (requires complexity assessment)");
            } else {
                info!("Plan validation PASSED");
            }
        } else {
            warn!(
                error_count = issues
                    .iter()
                    .filter(|i| i.severity == IssueSeverity::Error)
                    .count(),
                "Plan validation FAILED"
            );
        }

        // Generate actionable feedback for revision if validation failed
        let revision_feedback = if !passed {
            RevisionFeedback::from_issues(&issues, &warnings)
        } else {
            None
        };

        Ok(ValidationResult {
            passed,
            requires_complexity_assessment,
            evidence_sufficiency,
            spec_plan_consistency: spec_plan,
            evidence_quality,
            evidence_alignment: evidence_align,
            constitution_compliance: constitution,
            execution_feasibility: feasibility,
            issues,
            warnings,
            revision_feedback,
        })
    }

    fn check_evidence_quality(&self, evidence: &Evidence) -> ValidationCheckResult {
        let quality = evidence.validate_quality(&self.quality_config);
        let min_quality = self.quality_config.min_evidence_quality;
        let min_confidence = self.quality_config.min_evidence_confidence;
        let mut details = Vec::new();

        // Tiered thresholds based on configured values (not hardcoded)
        // Yellow threshold = 70% of configured minimum (allows flexibility for simpler tasks)
        let yellow_quality_threshold = min_quality * 0.7;
        let yellow_confidence_threshold = min_confidence * 0.7;

        // Determine quality tier based on configured thresholds
        // Green: >= configured threshold
        // Yellow: >= 70% of configured threshold (LLM should assess)
        // Red: < 70% of configured threshold
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

        // Overall tier is the worse of quality and confidence
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

        // Verifiable evidence check - only fail (Red) if truly zero
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

    fn check_spec_plan_consistency(
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

    fn check_evidence_alignment(
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

        // Validate new_paths: must be safe relative paths, non-existent file paths
        for new_path in &plan_new_paths {
            let path = Path::new(new_path);

            // Security: reject unsafe paths (absolute, .., empty)
            if !is_safe_relative_path(path) {
                details.push(CheckDetail::Error(format!(
                    "new_paths contains unsafe path '{}' (must be relative, no .. traversal)",
                    new_path
                )));
                passed = false;
                continue;
            }

            // Reject explicit directory paths (ending with /)
            if new_path.ends_with('/') {
                details.push(CheckDetail::Error(format!(
                    "new_paths contains directory '{}' (must be file paths only)",
                    new_path
                )));
                passed = false;
                continue;
            }

            // Security: validate path stays within working directory
            if !is_new_path_safe(path, &self.working_dir) {
                details.push(CheckDetail::Error(format!(
                    "new_paths '{}' escapes working directory",
                    new_path
                )));
                passed = false;
                continue;
            }

            let full_path = self.working_dir.join(path);

            // Reject if path exists (new_paths is for files to be CREATED)
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

    fn check_constitution_compliance(
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

    fn check_execution_feasibility(&self, tasks: &TasksArtifact) -> ValidationCheckResult {
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

    pub async fn validate_with_ai(
        &self,
        spec: &SpecArtifact,
        plan: &PlanArtifact,
        tasks: &TasksArtifact,
    ) -> Result<Vec<String>> {
        let prompt = format!(
            r#"## Plan Validation

Validate this implementation plan as an independent reviewer.

**Specification**:
{}

**Plan**:
{}

**Tasks**:
{}

---

Check for:
1. Missing requirements from spec
2. Scope creep (features not in spec)
3. Dependency issues
4. Risk areas not addressed
5. Unclear or ambiguous tasks

Return a list of issues found, or "VALIDATED" if plan is sound."#,
            spec.to_markdown(),
            plan.to_markdown(),
            tasks.to_markdown()
        );

        let output = self.executor.run(&prompt).await?;

        // Trust LLM's judgment on validation.
        // "VALIDATED" indicates no issues found.
        // Any other response is treated as the full issue description.
        // We avoid fragile line-by-line parsing which assumes specific output format
        // (e.g., bullet points, numbered lists) that may vary across LLM responses.
        let trimmed = output.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("validated") {
            return Ok(Vec::new());
        }

        // Return the entire response as a single issue rather than
        // attempting to parse it into structured items. The caller
        // can log/display it as-is for human review.
        Ok(vec![trimmed.to_string()])
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

/// Normalizes a path by removing `.` components and resolving `..` where possible.
/// Returns None if the path escapes upward (more `..` than depth allows).
///
/// ## Design Philosophy
/// This function balances security (preventing directory escape) with usability
/// (allowing legitimate internal paths like `src/foo/../bar.rs`).
///
/// - `src/foo/../bar.rs` → `src/bar.rs` (valid internal navigation)
/// - `../outside.rs` → None (escapes working directory)
/// - `src/../../outside.rs` → None (escapes after going into src)
fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut components = Vec::new();
    // Track how many parent-dir references we've accumulated at the start
    // These represent "we want to go up from the current directory"
    let mut leading_parents = 0usize;
    let mut saw_normal = false;

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if saw_normal {
                    // We've seen normal components, so we can pop
                    if components.is_empty() {
                        // Would escape upward after entering
                        return None;
                    }
                    components.pop();
                } else {
                    // Leading parent dirs - track them
                    leading_parents += 1;
                }
            }
            std::path::Component::CurDir => {
                // `.` is always skipped
            }
            std::path::Component::Normal(c) => {
                // If we had leading parents, we're escaping
                if leading_parents > 0 {
                    return None;
                }
                saw_normal = true;
                components.push(c);
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                // Absolute paths are not allowed
                return None;
            }
        }
    }

    if components.is_empty() {
        return None;
    }

    Some(components.iter().collect())
}

/// Checks if a path is a safe relative path (no traversal or absolute paths).
fn is_safe_relative_path(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    normalize_path(path).is_some()
}

/// Verifies that a new file path's parent directories don't escape via symlinks.
/// Uses atomic metadata check to avoid TOCTTOU race conditions.
fn is_new_path_safe(path: &Path, working_dir: &Path) -> bool {
    let Some(normalized) = normalize_path(path) else {
        return false;
    };

    let full_path = working_dir.join(&normalized);
    let Ok(canonical_working) = working_dir.canonicalize() else {
        return false;
    };

    // Walk up to find existing ancestor, using symlink_metadata to avoid following links
    let mut current = full_path.as_path();
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(meta) => {
                // Found existing path - check if it's a symlink
                if meta.is_symlink() {
                    // Resolve symlink and verify destination is within working_dir
                    match current.canonicalize() {
                        Ok(resolved) if resolved.starts_with(&canonical_working) => {
                            // If this symlink is an ancestor (not the target itself),
                            // it must resolve to a directory (can't create path under a file)
                            if current != full_path && !resolved.is_dir() {
                                return false;
                            }
                            return true;
                        }
                        _ => return false,
                    }
                }
                // Reject if ancestor is a file (can't create path under a file)
                if meta.is_file() && current != full_path {
                    return false;
                }
                // Regular file/dir exists - verify it's within working_dir
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

/// Checks if a file path has evidence backing for planning.
///
/// Returns `(has_backing, is_new_file)`:
/// - `has_backing`: true if the path is justified by evidence, plan, or is a planned new file
/// - `is_new_file`: true if this is a planned new file (doesn't need to exist)
///
/// For existing files, also verifies the file actually exists on the filesystem
/// to prevent hallucinated file references from passing validation.
fn path_has_evidence_backing(
    file: &str,
    evidence_files: &HashSet<&str>,
    plan_paths: &HashSet<&str>,
    new_paths: &HashSet<&str>,
    evidence_areas: &HashSet<&str>,
    working_dir: &Path,
) -> (bool, bool) {
    let file_path = Path::new(file);

    // Security: Reject unsafe paths (absolute or with traversal components)
    if !is_safe_relative_path(file_path) {
        return (false, false);
    }

    // Check if this is a planned new file (exempt from existence verification)
    // Use normalized path comparison to handle ./src/foo.rs vs src/foo.rs
    // new_paths must be specific file paths (already validated to not be directories)
    let normalized_file = normalize_path(file_path);
    let is_new = normalized_file.as_ref().is_some_and(|nf| {
        new_paths
            .iter()
            .filter_map(|np| normalize_path(Path::new(np)))
            .any(|nnp| nf == &nnp)
    });
    if is_new {
        // For new paths, verify parent directories don't escape via symlinks
        if !is_new_path_safe(file_path, working_dir) {
            return (false, false);
        }
        return (true, true);
    }

    // Check evidence backing
    let in_evidence = evidence_files
        .iter()
        .any(|ef| paths_overlap(file_path, Path::new(ef)));

    // Evidence files are already verified to exist during gathering
    if in_evidence {
        return (true, false);
    }

    let in_plan = plan_paths
        .iter()
        .any(|pp| paths_overlap(file_path, Path::new(pp)));

    let in_area = evidence_areas
        .iter()
        .any(|area| paths_overlap(file_path, Path::new(area)));

    // For plan_paths and evidence_areas matches, verify file actually exists
    // This prevents hallucinated file references when areas are broad (e.g., "src/")
    if in_plan || in_area {
        let full_path = working_dir.join(file);
        // Additional safety: verify resolved path is still within working_dir
        if let (Ok(canonical), Ok(canonical_working)) =
            (full_path.canonicalize(), working_dir.canonicalize())
            && canonical.starts_with(&canonical_working)
        {
            return (true, false);
        }
        // File doesn't exist or is outside working_dir
        return (false, false);
    }

    (false, false)
}

fn collect_issues_from(
    check: &ValidationCheckResult,
    category: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    if check.passed {
        return;
    }

    for detail in &check.details {
        if let CheckDetail::Error(msg) = detail {
            issues.push(ValidationIssue {
                severity: IssueSeverity::Error,
                category: category.to_string(),
                description: msg.clone(),
            });
        }
    }
}

impl ValidationResult {
    pub fn to_summary(&self) -> String {
        let mut output = String::new();

        // Evidence sufficiency at the top (NON-NEGOTIABLE fact-based development)
        output.push_str(&format!(
            "Evidence Sufficiency: {} (coverage: {:.0}%)\n",
            if self.evidence_sufficiency.allows_planning() {
                "OK"
            } else {
                "INSUFFICIENT"
            },
            self.evidence_sufficiency.coverage() * 100.0
        ));

        let checks = [
            ("Spec-Plan Consistency", &self.spec_plan_consistency),
            ("Evidence Quality", &self.evidence_quality),
            ("Evidence Alignment", &self.evidence_alignment),
            ("Constitution Compliance", &self.constitution_compliance),
            ("Execution Feasibility", &self.execution_feasibility),
        ];

        for (name, check) in checks {
            output.push_str(&format!(
                "{}: {}\n",
                name,
                if check.passed { "PASS" } else { "FAIL" }
            ));
        }

        if !self.issues.is_empty() {
            output.push_str(&format!("\nIssues ({}):\n", self.issues.len()));
            for issue in &self.issues {
                let severity = match issue.severity {
                    IssueSeverity::Critical => "CRITICAL",
                    IssueSeverity::Error => "ERROR",
                    IssueSeverity::Warning => "WARN",
                    IssueSeverity::Info => "INFO",
                };
                output.push_str(&format!(
                    "  [{}] {}: {}\n",
                    severity, issue.category, issue.description
                ));
            }
        }

        if !self.warnings.is_empty() {
            output.push_str(&format!("\nWarnings ({}):\n", self.warnings.len()));
            for warning in &self.warnings {
                output.push_str(&format!("  - {}\n", warning));
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_path() {
        assert_eq!(
            normalize_path(Path::new("src/main.rs")),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(
            normalize_path(Path::new("./src/lib.rs")),
            Some(PathBuf::from("src/lib.rs"))
        );
        assert_eq!(
            normalize_path(Path::new("src/./foo/./bar.rs")),
            Some(PathBuf::from("src/foo/bar.rs"))
        );
        // Safe traversal within path
        assert_eq!(
            normalize_path(Path::new("src/foo/../bar.rs")),
            Some(PathBuf::from("src/bar.rs"))
        );
        // Escape attempt
        assert_eq!(normalize_path(Path::new("../outside.rs")), None);
        assert_eq!(normalize_path(Path::new("src/../../outside.rs")), None);
        // Absolute paths
        assert_eq!(normalize_path(Path::new("/etc/passwd")), None);
        // Empty after normalization
        assert_eq!(normalize_path(Path::new(".")), None);
        assert_eq!(normalize_path(Path::new("")), None);
    }

    #[test]
    fn test_is_safe_relative_path_accepts_valid() {
        assert!(is_safe_relative_path(Path::new("src/main.rs")));
        assert!(is_safe_relative_path(Path::new("foo/bar/baz.txt")));
        assert!(is_safe_relative_path(Path::new("file.txt")));
        assert!(is_safe_relative_path(Path::new("./src/lib.rs")));
        // Internal traversal that stays within bounds is safe
        assert!(is_safe_relative_path(Path::new("src/foo/../bar.rs")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_traversal() {
        assert!(!is_safe_relative_path(Path::new("../outside.rs")));
        assert!(!is_safe_relative_path(Path::new("src/../../outside.rs")));
        assert!(!is_safe_relative_path(Path::new("..")));
        // Tricky patterns
        assert!(!is_safe_relative_path(Path::new("./../../etc/passwd")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_absolute() {
        assert!(!is_safe_relative_path(Path::new("/etc/passwd")));
        assert!(!is_safe_relative_path(Path::new("/usr/bin/sh")));
    }

    #[test]
    fn test_is_safe_relative_path_rejects_empty() {
        assert!(!is_safe_relative_path(Path::new("")));
        assert!(!is_safe_relative_path(Path::new(".")));
    }

    #[test]
    fn test_is_new_path_safe_within_working_dir() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        // Create a subdirectory
        std::fs::create_dir(working_dir.join("src")).unwrap();

        // New file in existing subdirectory should be safe
        assert!(is_new_path_safe(Path::new("src/new_file.rs"), working_dir));

        // New file in new subdirectory (parent exists) should be safe
        assert!(is_new_path_safe(
            Path::new("src/subdir/new_file.rs"),
            working_dir
        ));

        // New file at root should be safe
        assert!(is_new_path_safe(Path::new("new_file.rs"), working_dir));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_new_path_safe_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let working_dir = tempdir().unwrap();
        let outside_dir = tempdir().unwrap();

        // Create a symlink inside working_dir that points outside
        let symlink_path = working_dir.path().join("escape_link");
        symlink(outside_dir.path(), &symlink_path).unwrap();

        // New file via symlink should be rejected
        assert!(!is_new_path_safe(
            Path::new("escape_link/malicious.rs"),
            working_dir.path()
        ));
    }

    #[test]
    fn test_is_new_path_safe_rejects_path_under_file() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        // Create a file (not a directory)
        std::fs::write(working_dir.join("main.rs"), "fn main() {}").unwrap();

        // Attempting to create a path "under" a file should be rejected
        // e.g., main.rs/subfile.rs is invalid because main.rs is a file
        assert!(!is_new_path_safe(
            Path::new("main.rs/subfile.rs"),
            working_dir
        ));

        // But creating a sibling file should work
        assert!(is_new_path_safe(Path::new("lib.rs"), working_dir));

        // And creating in a real directory should work
        std::fs::create_dir(working_dir.join("src")).unwrap();
        assert!(is_new_path_safe(Path::new("src/mod.rs"), working_dir));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_new_path_safe_rejects_symlink_to_file() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        // Create a file and a symlink pointing to it
        let target_file = working_dir.join("target.rs");
        std::fs::write(&target_file, "fn foo() {}").unwrap();

        let symlink_path = working_dir.join("link_to_file");
        symlink(&target_file, &symlink_path).unwrap();

        // Attempting to create a path "under" a symlink that points to a file should be rejected
        // e.g., link_to_file/new.rs is invalid because link_to_file resolves to a file
        assert!(!is_new_path_safe(
            Path::new("link_to_file/new.rs"),
            working_dir
        ));

        // But creating the symlink target itself should work
        assert!(is_new_path_safe(Path::new("link_to_file"), working_dir));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_new_path_safe_allows_symlink_to_directory() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let working_dir = dir.path();

        // Create a directory and a symlink pointing to it
        let target_dir = working_dir.join("target_dir");
        std::fs::create_dir(&target_dir).unwrap();

        let symlink_path = working_dir.join("link_to_dir");
        symlink(&target_dir, &symlink_path).unwrap();

        // Creating a path under a symlink that points to a directory should work
        assert!(is_new_path_safe(
            Path::new("link_to_dir/new.rs"),
            working_dir
        ));
    }
}
