use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default)]
pub enum VerificationScope {
    #[default]
    Full,
    TaskScoped {
        test_patterns: Vec<String>,
        module: Option<String>,
    },
}

impl VerificationScope {
    pub fn for_task(test_patterns: Vec<String>, module: Option<String>) -> Self {
        Self::TaskScoped {
            test_patterns,
            module,
        }
    }

    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    pub summary: String,
}

impl Verification {
    pub fn new(checks: Vec<CheckResult>) -> Self {
        // Empty checks = verification failed (no work was verified)
        // This prevents false "pass" when no actual verification occurred
        let passed = !checks.is_empty() && checks.iter().all(|c| c.passed);
        let failed_count = checks.iter().filter(|c| !c.passed).count();

        let summary = if checks.is_empty() {
            String::from("No checks performed - verification incomplete")
        } else if passed {
            format!("All {} checks passed", checks.len())
        } else {
            format!("{}/{} checks failed", failed_count, checks.len())
        };

        Self {
            passed,
            checks,
            summary,
        }
    }

    /// Creates a verification that explicitly passes without checks.
    /// Use only when verification is intentionally skipped (e.g., planning-only tasks).
    pub fn passed() -> Self {
        Self {
            passed: true,
            checks: Vec::new(),
            summary: String::from("No checks required"),
        }
    }

    /// Creates a verification that explicitly fails due to missing implementation.
    pub fn no_implementation() -> Self {
        Self {
            passed: false,
            checks: Vec::new(),
            summary: String::from("No implementation detected"),
        }
    }

    pub fn failed_checks(&self) -> Vec<&CheckResult> {
        self.checks.iter().filter(|c| !c.passed).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// File was created as expected
    FileCreated,
    /// File was modified as expected
    FileModified,
    /// File was deleted as expected
    FileDeleted,
    /// Build/compilation check
    Build,
    /// Test execution check
    Test,
    /// Linting check
    Lint,
    /// Type checking
    TypeCheck,
    /// Custom check
    Custom,
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileCreated => write!(f, "File Created"),
            Self::FileModified => write!(f, "File Modified"),
            Self::FileDeleted => write!(f, "File Deleted"),
            Self::Build => write!(f, "Build"),
            Self::Test => write!(f, "Test"),
            Self::Lint => write!(f, "Lint"),
            Self::TypeCheck => write!(f, "Type Check"),
            Self::Custom => write!(f, "Custom"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub check: Check,
    pub name: String,
    pub passed: bool,
    pub output: Option<String>,
    pub duration_ms: u64,
}

impl CheckResult {
    pub fn success(check: Check, name: impl Into<String>) -> Self {
        Self {
            check,
            name: name.into(),
            passed: true,
            output: None,
            duration_ms: 0,
        }
    }

    pub fn failure(check: Check, name: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            check,
            name: name.into(),
            passed: false,
            output: Some(output.into()),
            duration_ms: 0,
        }
    }

    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_checks_fails_verification() {
        // Empty checks should NOT pass - this prevents false "pass" with no work
        let verification = Verification::new(vec![]);
        assert!(!verification.passed);
        assert!(verification.summary.contains("No checks performed"));
    }

    #[test]
    fn test_all_checks_pass() {
        let checks = vec![
            CheckResult::success(Check::Build, "Build"),
            CheckResult::success(Check::Test, "Tests"),
        ];
        let verification = Verification::new(checks);
        assert!(verification.passed);
        assert_eq!(verification.summary, "All 2 checks passed");
    }

    #[test]
    fn test_some_checks_fail() {
        let checks = vec![
            CheckResult::success(Check::Build, "Build"),
            CheckResult::failure(Check::Test, "Tests", "Test failed"),
        ];
        let verification = Verification::new(checks);
        assert!(!verification.passed);
        assert_eq!(verification.summary, "1/2 checks failed");
    }

    #[test]
    fn test_explicit_pass() {
        let verification = Verification::passed();
        assert!(verification.passed);
        assert_eq!(verification.summary, "No checks required");
    }

    #[test]
    fn test_no_implementation() {
        let verification = Verification::no_implementation();
        assert!(!verification.passed);
        assert_eq!(verification.summary, "No implementation detected");
    }

    #[test]
    fn test_failed_checks_returns_only_failures() {
        let checks = vec![
            CheckResult::success(Check::Build, "Build"),
            CheckResult::failure(Check::Test, "Tests", "Failed"),
            CheckResult::success(Check::Lint, "Lint"),
        ];
        let verification = Verification::new(checks);
        let failed = verification.failed_checks();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].name, "Tests");
    }
}
