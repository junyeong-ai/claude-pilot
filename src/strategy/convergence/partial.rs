//! Partial convergence acceptance policy.

use crate::verification::{Issue, IssueCategory, IssueSeverity};

/// Policy for accepting partial convergence.
#[derive(Debug, Clone)]
pub struct PartialConvergencePolicy {
    /// Categories that must be resolved.
    pub critical_categories: Vec<IssueCategory>,
    /// Severities that must be resolved.
    pub critical_severities: Vec<IssueSeverity>,
    /// Maximum acceptable non-critical issues.
    pub max_acceptable: usize,
}

impl Default for PartialConvergencePolicy {
    fn default() -> Self {
        Self {
            critical_categories: vec![
                IssueCategory::BuildError,
                IssueCategory::TypeCheckError,
                IssueCategory::SecurityIssue,
            ],
            critical_severities: vec![IssueSeverity::Critical, IssueSeverity::Error],
            max_acceptable: 5,
        }
    }
}

impl PartialConvergencePolicy {
    pub fn strict() -> Self {
        Self {
            critical_categories: vec![
                IssueCategory::BuildError,
                IssueCategory::TypeCheckError,
                IssueCategory::TestFailure,
                IssueCategory::SecurityIssue,
                IssueCategory::LintError,
            ],
            critical_severities: vec![
                IssueSeverity::Critical,
                IssueSeverity::Error,
                IssueSeverity::Warning,
            ],
            max_acceptable: 0,
        }
    }

    pub fn lenient() -> Self {
        Self {
            critical_categories: vec![IssueCategory::BuildError, IssueCategory::SecurityIssue],
            critical_severities: vec![IssueSeverity::Critical],
            max_acceptable: 10,
        }
    }

    /// Check if an issue is critical and must be resolved.
    pub fn is_critical(&self, issue: &Issue) -> bool {
        self.critical_categories.contains(&issue.category)
            || self.critical_severities.contains(&issue.severity)
    }

    /// Evaluate issues against policy.
    pub fn evaluate(&self, issues: &[Issue]) -> PartialEvaluation {
        let (critical, acceptable): (Vec<_>, Vec<_>) =
            issues.iter().partition(|i| self.is_critical(i));

        if !critical.is_empty() {
            return PartialEvaluation {
                is_acceptable: false,
                critical_remaining: critical.len(),
                accepted_issues: Vec::new(),
                rejected_reason: Some(format!(
                    "{} critical issues must be resolved",
                    critical.len()
                )),
            };
        }

        if acceptable.len() > self.max_acceptable {
            return PartialEvaluation {
                is_acceptable: false,
                critical_remaining: 0,
                accepted_issues: Vec::new(),
                rejected_reason: Some(format!(
                    "{} issues exceed maximum acceptable ({})",
                    acceptable.len(),
                    self.max_acceptable
                )),
            };
        }

        PartialEvaluation {
            is_acceptable: true,
            critical_remaining: 0,
            accepted_issues: acceptable.into_iter().cloned().collect(),
            rejected_reason: None,
        }
    }
}

/// Result of partial convergence evaluation.
#[derive(Debug)]
pub struct PartialEvaluation {
    pub is_acceptable: bool,
    pub critical_remaining: usize,
    pub accepted_issues: Vec<Issue>,
    pub rejected_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONFIDENCE: f32 = 0.8;

    fn make_issue(category: IssueCategory, severity: IssueSeverity) -> Issue {
        Issue::new(category, severity, "test message", TEST_CONFIDENCE)
    }

    #[test]
    fn test_default_policy_accepts_warnings() {
        let policy = PartialConvergencePolicy::default();
        let issues = vec![
            make_issue(IssueCategory::LintError, IssueSeverity::Warning),
            make_issue(IssueCategory::StyleViolation, IssueSeverity::Info),
        ];

        let eval = policy.evaluate(&issues);
        assert!(eval.is_acceptable);
        assert_eq!(eval.accepted_issues.len(), 2);
    }

    #[test]
    fn test_default_policy_rejects_build_errors() {
        let policy = PartialConvergencePolicy::default();
        let issues = vec![
            make_issue(IssueCategory::BuildError, IssueSeverity::Error),
            make_issue(IssueCategory::LintError, IssueSeverity::Warning),
        ];

        let eval = policy.evaluate(&issues);
        assert!(!eval.is_acceptable);
        assert_eq!(eval.critical_remaining, 1);
    }

    #[test]
    fn test_max_acceptable_limit() {
        let policy = PartialConvergencePolicy {
            max_acceptable: 2,
            ..Default::default()
        };
        let issues = vec![
            make_issue(IssueCategory::StyleViolation, IssueSeverity::Info),
            make_issue(IssueCategory::StyleViolation, IssueSeverity::Info),
            make_issue(IssueCategory::StyleViolation, IssueSeverity::Info),
        ];

        let eval = policy.evaluate(&issues);
        assert!(!eval.is_acceptable);
        assert!(eval.rejected_reason.unwrap().contains("exceed maximum"));
    }

    #[test]
    fn test_strict_policy() {
        let policy = PartialConvergencePolicy::strict();
        let issues = vec![make_issue(IssueCategory::LintError, IssueSeverity::Warning)];

        let eval = policy.evaluate(&issues);
        assert!(!eval.is_acceptable);
    }
}
