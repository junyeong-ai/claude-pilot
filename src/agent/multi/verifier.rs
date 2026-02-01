//! Verifier agent specialized for quality verification and testing.
//!
//! # Purpose
//!
//! The `VerifierAgent` ensures implementation quality by running build systems, test suites,
//! and static analysis tools. It provides structured pass/fail results that drive the
//! convergent verification loop in the coordinator.
//!
//! # Responsibilities
//!
//! - **Verify implementations**: Ensure code meets requirements and acceptance criteria
//! - **Run test suites**: Execute and analyze test results across frameworks
//! - **Check build status**: Verify compilation/build success
//! - **Static analysis**: Run linters and code quality tools when available
//! - **Detect regressions**: Ensure existing functionality remains intact
//!
//! # Verification Process
//!
//! The Verifier follows a structured, multi-step process:
//!
//! 1. **Auto-detect build system**: Identify project type (Cargo, npm, Maven, Go, Poetry, etc.)
//! 2. **Run build**: Execute appropriate build command for the detected system
//! 3. **Run tests**: Execute test suite using project's test framework
//! 4. **Run linter**: Execute static analysis if available (SKIP if not present)
//! 5. **Review code**: Check for quality issues and pattern violations
//! 6. **Provide verdict**: Overall PASS/FAIL based on all checks
//!
//! **Permission Requirements**: The Verifier uses `PermissionProfile::VerifyExecute` to run
//! build/test/lint commands. If command execution is not available (e.g., in restricted
//! environments), the Verifier should output `SKIP` for those checks, which is acceptable.
//!
//! # Quality Criteria
//!
//! Code must meet all of the following to pass verification:
//!
//! - ✅ **Build**: Code compiles/builds without errors
//! - ✅ **Tests**: All tests pass (unit, integration, etc.)
//! - ✅ **Lint**: No linter warnings (SKIP if no linter available is acceptable)
//! - ✅ **Patterns**: Code follows existing conventions
//! - ✅ **Security**: No obvious security vulnerabilities
//! - ✅ **Focus**: Changes are minimal and targeted
//!
//! # Structured Output Format
//!
//! The Verifier produces results in a structured format for parsing:
//!
//! ```text
//! BUILD: PASS|FAIL|SKIP
//! TESTS: PASS|FAIL|SKIP
//! LINT: PASS|FAIL|SKIP
//! VERDICT: PASS|FAIL
//!
//! [Detailed findings and issue descriptions...]
//! ```
//!
//! - **PASS**: Check succeeded without errors
//! - **FAIL**: Check failed with errors/failures
//! - **SKIP**: Check not applicable (e.g., no build step, no tests, no linter)
//! - **VERDICT**: Overall result (PASS if all applicable checks passed; SKIP for BUILD/TESTS/LINT counts as passing, but VERDICT: SKIP counts as failure)
//!
//! # Convergent Verification Requirement
//!
//! ## ⚠️ NON-NEGOTIABLE: 2 Consecutive Clean Rounds
//!
//! The coordinator requires **2 consecutive successful verification rounds** before
//! considering a task complete. This ensures:
//!
//! - Fixes don't introduce new issues
//! - Tests are stable and reproducible
//! - Quality bar is consistently met
//!
//! ```text
//! Attempt 1: FAIL → Coder fixes → Attempt 2: PASS → Attempt 3: PASS ✓ SUCCESS
//! Attempt 1: PASS → Attempt 2: PASS ✓ SUCCESS (2 rounds)
//! Attempt 1: PASS → Attempt 2: FAIL → Attempt 3: PASS → Attempt 4: PASS ✓ SUCCESS
//! ```
//!
//! # Integration
//!
//! The Verifier integrates into the coordinator's convergent loop:
//!
//! ```text
//! Implementation → Verification → (if FAIL) → Coder fixes → Verification
//!                      ↓
//!                  (if PASS)
//!                      ↓
//!              2nd Verification → (if PASS) → Task Complete
//! ```
//!
//! # Supported Build Systems
//!
//! The Verifier auto-detects and supports:
//!
//! - **Rust**: Cargo (`cargo build`, `cargo test`, `cargo clippy`)
//! - **Node.js**: npm/yarn/pnpm (`npm run build`, `npm test`, `npm run lint`)
//! - **Python**: Poetry/pip (`pytest`, `pylint`, `mypy`)
//! - **Java**: Maven/Gradle (`mvn test`, `gradle test`)
//! - **Go**: Go toolchain (`go build`, `go test`, `golint`)
//! - **Others**: Generic detection based on project files
//!
//! # Artifact Output
//!
//! Produces `Report` type artifacts containing:
//!
//! - Structured verification results (BUILD/TESTS/LINT/VERDICT)
//! - Detailed error messages and stack traces
//! - Test failure summaries
//! - Linter warnings and suggestions
//! - Overall quality assessment

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::error::Result;

const VERIFIER_SYSTEM_PROMPT: &str = r"# Verifier Agent

You are a specialized verification agent focused on quality assurance.

## Primary Responsibilities
- Verify implementations meet requirements
- Run and analyze test results
- Review code quality
- Check for regressions
- Validate against acceptance criteria

## Verification Process
1. Detect the project's build system (Cargo, npm, Gradle, Maven, Go, Poetry, etc.)
2. Run appropriate build command for the detected system (output SKIP if execution unavailable)
3. Run the test suite using the project's test framework (output SKIP if execution unavailable)
4. Run linter/static analysis if available for the project (output SKIP if not present)
5. Review code changes for quality issues
6. Verify acceptance criteria are met

**Note**: This agent uses PermissionProfile::VerifyExecute to run build/test/lint commands.
If command execution is restricted or unavailable, use SKIP status for those checks.

## Quality Criteria
- Code compiles/builds without errors (or no build step required)
- All tests pass (or no tests available)
- No linter warnings (SKIP if no linter is available is acceptable)
- Code follows existing patterns
- No obvious security issues
- Changes are minimal and focused

## Output Format
Provide verification results in this EXACT format:

```
BUILD: PASS|FAIL|SKIP
TESTS: PASS|FAIL|SKIP
LINT: PASS|FAIL|SKIP
VERDICT: PASS|FAIL

[Details and findings here...]
```

Notes:
- Use PASS if the check succeeded
- Use FAIL if the check failed with errors
- Use SKIP if the check is not applicable (e.g., no build step, no tests, no linter)
- VERDICT should be PASS if all applicable checks passed (SKIP is treated as PASS for BUILD/TESTS/LINT)
- VERDICT: SKIP is treated as FAIL (incomplete verification)

## NON-NEGOTIABLE
2 consecutive clean rounds required for success.
";

pub struct VerifierAgent {
    core: AgentCore,
}

impl VerifierAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::core_verifier(), task_agent),
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id.to_string(), task_agent))
    }

    fn build_verification_prompt(&self, task: &AgentTask) -> String {
        AgentPromptBuilder::new(VERIFIER_SYSTEM_PROMPT, "Verification", task)
            .with_context(task)
            .with_related_files(task)
            .with_section(
                "Verification Steps",
                &[
                    "1. Identify the project's build system from project files",
                    "2. Run the appropriate build command to verify compilation",
                    "3. Run the test suite to verify tests pass",
                    "4. Run linter/static analysis if available",
                    "5. Review the code changes for quality",
                    "6. Provide overall verdict",
                ],
            )
            .build()
    }

    /// Parses the raw verification output into structured components.
    ///
    /// This method extracts BUILD/TESTS/LINT/VERDICT status from LLM output using
    /// robust pattern matching that tolerates formatting variations.
    ///
    /// Design Decision (fix for issue fix-r2-1):
    /// The original implementation used simple `starts_with()` checks which were fragile.
    /// This updated version uses multi-level matching:
    /// 1. Exact word matching (most reliable)
    /// 2. Keyword presence detection (fallback for malformed output)
    /// 3. Safe defaults (fail on unrecognized output to avoid false positives)
    ///
    /// This ensures parsing remains robust even if LLM output format changes slightly.
    fn parse_verification_result(&self, output: &str) -> VerificationSummary {
        let upper = output.to_uppercase();

        let build_passed = Self::parse_check_result(&upper, "BUILD:", true);
        let tests_passed = Self::parse_check_result(&upper, "TESTS:", true);
        let lint_passed = Self::parse_check_result(&upper, "LINT:", true);
        let verdict = Self::parse_check_result(&upper, "VERDICT:", false);

        let has_structured_output =
            build_passed.is_some() || tests_passed.is_some() || verdict.is_some();

        let passed = Self::determine_overall_result(
            verdict,
            build_passed,
            tests_passed,
            has_structured_output,
            &upper,
        );

        // Fill in missing values with appropriate defaults.
        // If a check wasn't reported (None), treat it as passing (true).
        // This handles cases where:
        // - No build system exists (BUILD not reported → defaults to PASS)
        // - No tests exist (TESTS not reported → defaults to PASS)
        // - No linter configured (LINT not reported → defaults to PASS)
        //
        // Note: SKIP status is explicitly handled in parse_check_result():
        // - BUILD: SKIP → treated as PASS (skip_is_pass=true)
        // - TESTS: SKIP → treated as PASS (skip_is_pass=true)
        // - LINT: SKIP → treated as PASS (skip_is_pass=true)
        // - VERDICT: SKIP → treated as FAIL (skip_is_pass=false)
        let final_build_passed = build_passed.unwrap_or(true);
        let final_tests_passed = tests_passed.unwrap_or(true);
        let final_lint_passed = lint_passed.unwrap_or(true);

        let issues = Self::extract_issues(output, passed);

        VerificationSummary {
            build_passed: final_build_passed,
            tests_passed: final_tests_passed,
            lint_passed: final_lint_passed,
            issues,
            passed,
        }
    }

    /// Determines the overall pass/fail result from verification output.
    ///
    /// Priority:
    /// 1. Explicit VERDICT (if present)
    /// 2. Structured check results (BUILD/TESTS)
    /// 3. Heuristic analysis of unstructured output
    fn determine_overall_result(
        verdict: Option<bool>,
        build_passed: Option<bool>,
        tests_passed: Option<bool>,
        has_structured_output: bool,
        upper_text: &str,
    ) -> bool {
        if let Some(v) = verdict {
            // Priority 1: If explicit verdict exists, use it
            return v;
        }

        if has_structured_output {
            // Priority 2: Structured output without explicit verdict
            return Self::evaluate_structured_checks(build_passed, tests_passed);
        }

        // Priority 3: No structured output - use heuristics
        Self::evaluate_unstructured_output(upper_text)
    }

    /// Evaluates structured BUILD/TESTS check results to determine overall pass/fail.
    ///
    /// Logic:
    /// - If either BUILD or TESTS explicitly failed (Some(false)), overall fails
    /// - If both are Some(true), overall passes (includes SKIP treated as PASS)
    /// - If one is Some(true) and the other is None, overall passes
    /// - If both are None (unusual case with structured markers), fail safely
    ///
    /// Note: SKIP is converted to true (pass) by parse_check_result() before reaching this function.
    fn evaluate_structured_checks(build_passed: Option<bool>, tests_passed: Option<bool>) -> bool {
        match (build_passed, tests_passed) {
            (Some(false), _) | (_, Some(false)) => false,
            (Some(true), Some(true)) => true,
            (Some(true), None) | (None, Some(true)) => true,
            (None, None) => {
                // Structured markers found but no BUILD/TESTS results
                // This is unusual - fail safely to avoid false positives
                false
            }
        }
    }

    /// Evaluates unstructured output using heuristics to determine pass/fail.
    ///
    /// Success requires: explicit success indicator AND no failure indicators
    fn evaluate_unstructured_output(upper_text: &str) -> bool {
        let has_explicit_failures = upper_text.contains("FAIL")
            || upper_text.contains("ERROR:")
            || upper_text.contains("FAILED")
            || upper_text.contains("NOT PASS");

        let has_explicit_success = upper_text.contains("PASS")
            || upper_text.contains("SUCCESS")
            || upper_text.contains("NO ISSUES")
            || upper_text.contains("ALL CHECKS PASSED")
            || upper_text.contains("VERIFICATION COMPLETE");

        !has_explicit_failures && has_explicit_success
    }

    /// Extracts issue details from verification output.
    ///
    /// Returns an empty vector if verification passed, otherwise returns
    /// all lines after structured check markers.
    fn extract_issues(output: &str, passed: bool) -> Vec<String> {
        if passed {
            return vec![];
        }

        output
            .lines()
            .skip_while(|l| {
                let u = l.to_uppercase();
                u.starts_with("BUILD:")
                    || u.starts_with("TESTS:")
                    || u.starts_with("LINT:")
                    || u.starts_with("VERDICT:")
                    || l.trim().is_empty()
            })
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect()
    }

    /// Parses a structured check result (BUILD:/TESTS:/LINT:/VERDICT:) from verification output.
    ///
    /// This method uses robust pattern matching to handle variations in LLM output:
    /// - Exact matches: "PASS", "FAIL", "SKIP"
    /// - Case-insensitive matching (already handled by caller converting to uppercase)
    /// - Whitespace tolerance
    /// - Fallback to keyword detection for malformed output
    ///
    /// Returns:
    /// - Some(true) if check passed or skipped (when skip_is_pass=true)
    /// - Some(false) if check failed
    /// - None if check result line not found
    fn parse_check_result(text: &str, prefix: &str, skip_is_pass: bool) -> Option<bool> {
        text.lines()
            .find(|line| line.trim().starts_with(prefix))
            .map(|line| {
                let value = line.trim().strip_prefix(prefix).unwrap_or("").trim();

                // Normalize the value for robust matching
                let normalized = value.to_uppercase();

                // Try exact word matching first (most reliable)
                if normalized == "PASS"
                    || normalized.starts_with("PASS ")
                    || normalized.starts_with("PASS\t")
                {
                    return true;
                }

                if normalized == "SKIP"
                    || normalized.starts_with("SKIP ")
                    || normalized.starts_with("SKIP\t")
                {
                    return skip_is_pass;
                }

                if normalized == "FAIL"
                    || normalized.starts_with("FAIL ")
                    || normalized.starts_with("FAIL\t")
                {
                    return false;
                }

                // Fallback: Check for whole word keywords (not substrings like "SKIPPING")
                // This handles cases like "PASS - all tests succeeded" or "FAIL: compilation error"
                let words: Vec<&str> = normalized.split(|c: char| !c.is_alphanumeric()).collect();

                if words.contains(&"PASS") && !words.contains(&"FAIL") {
                    return true;
                }

                if words.contains(&"SKIP") && !words.contains(&"FAIL") && !words.contains(&"ERROR")
                {
                    return skip_is_pass;
                }

                // Default to failure for any unrecognized or ambiguous output
                // This is a safe default to avoid false positives
                false
            })
    }
}

#[derive(Debug)]
struct VerificationSummary {
    build_passed: bool,
    tests_passed: bool,
    lint_passed: bool,
    issues: Vec<String>,
    passed: bool,
}

impl VerificationSummary {
    fn to_findings(&self) -> Vec<String> {
        if self.passed {
            return vec![
                "Verification passed".to_string(),
                format!("Build: {}", if self.build_passed { "PASS" } else { "FAIL" }),
                format!("Tests: {}", if self.tests_passed { "PASS" } else { "FAIL" }),
                format!("Lint: {}", if self.lint_passed { "PASS" } else { "FAIL" }),
            ];
        }

        let mut findings = Vec::new();

        if !self.build_passed {
            findings.push("Build: FAIL".to_string());
        }
        if !self.tests_passed {
            findings.push("Tests: FAIL".to_string());
        }
        if !self.lint_passed {
            findings.push("Lint: FAIL".to_string());
        }

        findings.extend(self.issues.iter().cloned());
        findings
    }
}

#[async_trait]
impl SpecializedAgent for VerifierAgent {
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
        VERIFIER_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Verifier agent executing");

        let prompt = self.build_verification_prompt(task);
        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                let summary = self.parse_verification_result(&output);
                let findings = summary.to_findings();
                let result = if summary.passed {
                    AgentTaskResult::success(&task.id, output.clone())
                } else {
                    AgentTaskResult::failure(&task.id, output.clone())
                };
                Ok(result
                    .with_findings(findings)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("verification-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Report,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())
                .with_findings(vec![format!("Verification error: {}", e)])),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_verification_result() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = VerifierAgent::new("test".to_string(), task_agent);

        let output = "BUILD: PASS\nTESTS: PASS\nLINT: PASS\nVERDICT: PASS\n\nAll checks completed.";
        let summary = agent.parse_verification_result(output);
        assert!(summary.passed);
        assert!(summary.build_passed);
        assert!(summary.tests_passed);

        let output_fail =
            "BUILD: FAIL\nTESTS: SKIP\nLINT: SKIP\nVERDICT: FAIL\n\nCompilation error in main.rs";
        let summary = agent.parse_verification_result(output_fail);
        assert!(!summary.passed);
        assert!(!summary.build_passed);
        assert!(!summary.issues.is_empty());

        let output_test_fail =
            "BUILD: PASS\nTESTS: FAIL\nLINT: PASS\nVERDICT: FAIL\n\n2 tests failed";
        let summary = agent.parse_verification_result(output_test_fail);
        assert!(!summary.passed);
        assert!(summary.build_passed);
        assert!(!summary.tests_passed);

        let output_lint_skip = "BUILD: PASS\nTESTS: PASS\nLINT: SKIP\nVERDICT: PASS";
        let summary = agent.parse_verification_result(output_lint_skip);
        assert!(summary.passed);
        assert!(summary.lint_passed);

        // Test BUILD: SKIP is treated as acceptable
        let output_build_skip = "BUILD: SKIP\nTESTS: PASS\nLINT: SKIP\nVERDICT: PASS";
        let summary = agent.parse_verification_result(output_build_skip);
        assert!(summary.passed);
        assert!(summary.build_passed);

        // Test TESTS: SKIP is treated as acceptable
        let output_tests_skip = "BUILD: PASS\nTESTS: SKIP\nLINT: SKIP\nVERDICT: PASS";
        let summary = agent.parse_verification_result(output_tests_skip);
        assert!(summary.passed);
        assert!(summary.tests_passed);

        // Test all SKIP is acceptable
        let output_all_skip = "BUILD: SKIP\nTESTS: SKIP\nLINT: SKIP\nVERDICT: PASS";
        let summary = agent.parse_verification_result(output_all_skip);
        assert!(summary.passed);
        assert!(summary.build_passed);
        assert!(summary.tests_passed);
        assert!(summary.lint_passed);

        let output_minimal = "Verification complete.\nNo issues found.";
        let summary = agent.parse_verification_result(output_minimal);
        assert!(summary.passed);

        let output_failure = "Some errors were encountered during verification.";
        let summary = agent.parse_verification_result(output_failure);
        assert!(!summary.passed);

        let output_ambiguous = "Processing done.";
        let summary = agent.parse_verification_result(output_ambiguous);
        assert!(!summary.passed);

        // Test VERDICT: INCOMPLETE is treated as failure
        let output_incomplete = "BUILD: SKIP\nTESTS: SKIP\nLINT: SKIP\nVERDICT: INCOMPLETE";
        let summary = agent.parse_verification_result(output_incomplete);
        assert!(!summary.passed);
        assert!(summary.build_passed); // SKIP should still be treated as pass
        assert!(summary.tests_passed); // SKIP should still be treated as pass
        assert!(summary.lint_passed); // SKIP should still be treated as pass
    }

    #[test]
    fn test_evaluate_structured_checks() {
        // Both pass
        assert!(VerifierAgent::evaluate_structured_checks(
            Some(true),
            Some(true)
        ));

        // Build pass, tests missing
        assert!(VerifierAgent::evaluate_structured_checks(Some(true), None));

        // Build missing, tests pass
        assert!(VerifierAgent::evaluate_structured_checks(None, Some(true)));

        // Build fail
        assert!(!VerifierAgent::evaluate_structured_checks(
            Some(false),
            Some(true)
        ));

        // Tests fail
        assert!(!VerifierAgent::evaluate_structured_checks(
            Some(true),
            Some(false)
        ));

        // Both fail
        assert!(!VerifierAgent::evaluate_structured_checks(
            Some(false),
            Some(false)
        ));

        // Both missing (unusual, fail safely)
        assert!(!VerifierAgent::evaluate_structured_checks(None, None));
    }

    #[test]
    fn test_evaluate_unstructured_output() {
        // Success indicators without failures
        assert!(VerifierAgent::evaluate_unstructured_output(
            "VERIFICATION COMPLETE"
        ));
        assert!(VerifierAgent::evaluate_unstructured_output(
            "ALL CHECKS PASSED"
        ));
        assert!(VerifierAgent::evaluate_unstructured_output(
            "NO ISSUES FOUND"
        ));
        assert!(VerifierAgent::evaluate_unstructured_output(
            "SUCCESS - EVERYTHING LOOKS GOOD"
        ));

        // Failure indicators
        assert!(!VerifierAgent::evaluate_unstructured_output("BUILD FAILED"));
        assert!(!VerifierAgent::evaluate_unstructured_output(
            "ERROR: COMPILATION"
        ));
        assert!(!VerifierAgent::evaluate_unstructured_output("TESTS FAIL"));
        assert!(!VerifierAgent::evaluate_unstructured_output(
            "DID NOT PASS VERIFICATION"
        ));

        // Mixed (failure takes precedence)
        assert!(!VerifierAgent::evaluate_unstructured_output(
            "SUCCESS BUT SOME TESTS FAILED"
        ));

        // Ambiguous (no clear indicators)
        assert!(!VerifierAgent::evaluate_unstructured_output(
            "VERIFICATION DONE"
        ));
        assert!(!VerifierAgent::evaluate_unstructured_output("COMPLETED"));
    }

    #[test]
    fn test_extract_issues() {
        // No issues when passed
        let issues = VerifierAgent::extract_issues("BUILD: PASS\nTESTS: PASS", true);
        assert!(issues.is_empty());

        // Extract issues after markers
        let output = "BUILD: FAIL\nTESTS: PASS\n\nError in main.rs line 42\nUndefined variable";
        let issues = VerifierAgent::extract_issues(output, false);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0], "Error in main.rs line 42");
        assert_eq!(issues[1], "Undefined variable");

        // Skip empty lines
        let output_with_empty = "VERDICT: FAIL\n\n\nActual issue here\n\nAnother issue";
        let issues = VerifierAgent::extract_issues(output_with_empty, false);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0], "Actual issue here");
        assert_eq!(issues[1], "Another issue");
    }

    #[test]
    fn test_determine_overall_result() {
        // Priority 1: Explicit verdict takes precedence
        assert!(VerifierAgent::determine_overall_result(
            Some(true),
            Some(false),
            Some(false),
            true,
            "FAIL"
        ));
        assert!(!VerifierAgent::determine_overall_result(
            Some(false),
            Some(true),
            Some(true),
            true,
            "PASS"
        ));

        // Priority 2: Structured checks when no verdict
        assert!(VerifierAgent::determine_overall_result(
            None,
            Some(true),
            Some(true),
            true,
            ""
        ));
        assert!(!VerifierAgent::determine_overall_result(
            None,
            Some(false),
            Some(true),
            true,
            ""
        ));

        // Priority 3: Unstructured heuristics
        assert!(VerifierAgent::determine_overall_result(
            None,
            None,
            None,
            false,
            "ALL CHECKS PASSED"
        ));
        assert!(!VerifierAgent::determine_overall_result(
            None,
            None,
            None,
            false,
            "BUILD FAILED"
        ));
    }

    #[test]
    fn test_parse_check_result_robust() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = VerifierAgent::new("test".to_string(), task_agent);

        // Test exact matches
        let output = "BUILD: PASS\nTESTS: FAIL\nLINT: SKIP";
        let summary = agent.parse_verification_result(output);
        assert!(summary.build_passed);
        assert!(!summary.tests_passed);
        assert!(summary.lint_passed); // SKIP treated as pass

        // Test with extra whitespace
        let output_whitespace = "BUILD:   PASS  \nTESTS:  SKIP  \nLINT: FAIL";
        let summary = agent.parse_verification_result(output_whitespace);
        assert!(summary.build_passed);
        assert!(summary.tests_passed); // SKIP treated as pass
        assert!(!summary.lint_passed);

        // Test with descriptive text after status
        let output_desc = "BUILD: PASS - compilation successful\nTESTS: FAIL - 2 tests failed\nLINT: SKIP - no linter configured";
        let summary = agent.parse_verification_result(output_desc);
        assert!(summary.build_passed);
        assert!(!summary.tests_passed);
        assert!(summary.lint_passed); // SKIP treated as pass

        // Test with tabs instead of spaces
        let output_tabs = "BUILD:\tPASS\nTESTS:\tFAIL\nLINT:\tSKIP";
        let summary = agent.parse_verification_result(output_tabs);
        assert!(summary.build_passed);
        assert!(!summary.tests_passed);
        assert!(summary.lint_passed);

        // Test malformed output with keywords
        let output_malformed =
            "BUILD: PASS (all good)\nTESTS: Something went wrong - FAIL\nLINT: Skipping linter";
        let summary = agent.parse_verification_result(output_malformed);
        assert!(summary.build_passed);
        assert!(!summary.tests_passed); // Contains "FAIL"
        assert!(!summary.lint_passed); // "Skipping" doesn't match exact "SKIP"

        // Test ambiguous output defaults to fail
        let output_ambiguous = "BUILD: UNKNOWN\nTESTS: N/A\nLINT: ???";
        let summary = agent.parse_verification_result(output_ambiguous);
        assert!(!summary.build_passed); // Unrecognized defaults to fail
        assert!(!summary.tests_passed);
        assert!(!summary.lint_passed);

        // Test mixed case (should still work due to uppercase conversion)
        let output_mixed = "build: pass\ntests: SKIP\nlint: Fail";
        let summary = agent.parse_verification_result(output_mixed);
        assert!(summary.build_passed);
        assert!(summary.tests_passed);
        assert!(!summary.lint_passed);
    }
}
