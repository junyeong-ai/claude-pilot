use tracing::{debug, info, warn};

use super::super::artifacts::{PlanArtifact, SpecArtifact, TasksArtifact};
use super::super::Evidence;
use super::types::{QualityTier, RevisionFeedback, ValidationIssue, ValidationResult};
use super::PlanValidator;
use crate::error::Result;
use crate::domain::Severity;

impl PlanValidator {
    pub async fn validate(
        &self,
        spec: &SpecArtifact,
        plan: &PlanArtifact,
        tasks: &TasksArtifact,
        evidence: &Evidence,
    ) -> Result<ValidationResult> {
        info!("Validating plan against spec and evidence");

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
                severity: Severity::Error,
                category: "Evidence Sufficiency".to_string(),
                description: evidence_sufficiency.description(),
            });
        } else {
            match &evidence_sufficiency {
                super::super::EvidenceSufficiency::Sufficient { missing, .. }
                    if !missing.is_empty() =>
                {
                    warnings.push(format!("Evidence gaps: {}", missing.join(", ")));
                }
                super::super::EvidenceSufficiency::SufficientButRisky {
                    missing,
                    risk_context,
                    coverage,
                } => {
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
                    .filter(|i| i.severity == Severity::Error)
                    .count(),
                "Plan validation FAILED"
            );
        }

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

        let trimmed = output.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("validated") {
            return Ok(Vec::new());
        }

        Ok(vec![trimmed.to_string()])
    }
}

use super::types::{CheckDetail, ValidationCheckResult};

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
                severity: Severity::Error,
                category: category.to_string(),
                description: msg.clone(),
            });
        }
    }
}
