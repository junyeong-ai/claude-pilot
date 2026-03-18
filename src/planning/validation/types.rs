use crate::domain::Severity;


#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub passed: bool,
    pub requires_complexity_assessment: bool,
    pub evidence_sufficiency: super::super::EvidenceSufficiency,
    pub spec_plan_consistency: ValidationCheckResult,
    pub evidence_quality: ValidationCheckResult,
    pub evidence_alignment: ValidationCheckResult,
    pub constitution_compliance: ValidationCheckResult,
    pub execution_feasibility: ValidationCheckResult,
    pub issues: Vec<ValidationIssue>,
    pub warnings: Vec<String>,
    pub revision_feedback: Option<RevisionFeedback>,
}

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

    fn categorize_issue(issue: &ValidationIssue) -> (RevisionCategory, String) {
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
                match issue.severity {
                    Severity::Critical | Severity::Error => (
                        RevisionCategory::FeasibilityIssue,
                        "Address critical issue before proceeding",
                    ),
                    Severity::Warning | Severity::Info => (
                        RevisionCategory::MissingEvidence,
                        "Consider addressing this concern",
                    ),
                }
            };

        (revision_cat, fix.into())
    }

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
    Green,
    Yellow,
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
    pub severity: Severity,
    pub category: String,
    pub description: String,
}

impl ValidationResult {
    pub fn to_summary(&self) -> String {
        let mut output = String::new();

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
                    Severity::Critical => "CRITICAL",
                    Severity::Error => "ERROR",
                    Severity::Warning => "WARN",
                    Severity::Info => "INFO",
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
