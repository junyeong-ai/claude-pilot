use std::path::Path;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::Check;
use crate::agent::TaskAgent;
use crate::error::Result;
use crate::utils::truncate_with_marker;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub category: IssueCategory,
    pub severity: IssueSeverity,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub fix_attempts: u32,
    pub resolved: bool,
    pub confidence: f32,
}

impl Issue {
    const DEFAULT_MAX_MESSAGE_LENGTH: usize = 1000;

    pub fn new(
        category: IssueCategory,
        severity: IssueSeverity,
        message: impl Into<String>,
        confidence: f32,
    ) -> Self {
        Self::with_message_limit(
            category,
            severity,
            message,
            confidence,
            Self::DEFAULT_MAX_MESSAGE_LENGTH,
        )
    }

    pub fn with_message_limit(
        category: IssueCategory,
        severity: IssueSeverity,
        message: impl Into<String>,
        confidence: f32,
        max_length: usize,
    ) -> Self {
        let truncated_msg = truncate_with_marker(&message.into(), max_length);

        Self {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            category,
            severity,
            message: truncated_msg,
            file: None,
            line: None,
            fix_attempts: 0,
            resolved: false,
            confidence: confidence.clamp(0.0, 1.0),
        }
    }

    pub fn with_location(mut self, file: impl Into<String>, line: Option<usize>) -> Self {
        self.file = Some(file.into());
        self.line = line;
        self
    }

    pub fn increment_fix_attempts(&mut self) {
        self.fix_attempts += 1;
    }

    pub fn mark_resolved(&mut self) {
        self.resolved = true;
    }

    pub fn can_retry(&self, max_attempts: u32) -> bool {
        self.fix_attempts < max_attempts && !self.resolved
    }

    const SIGNATURE_MSG_LEN: usize = 200;

    pub fn signature(&self) -> String {
        if let (Some(file), Some(line)) = (&self.file, self.line) {
            let normalized_path = Self::normalize_path_for_signature(file);
            return format!("{}:{}:{}", self.category, normalized_path, line);
        }

        let normalized = Self::normalize_for_signature(&self.message);
        let file_part = self
            .file
            .as_deref()
            .map(Self::normalize_path_for_signature)
            .unwrap_or_else(|| "-".to_string());
        format!("{}:{}:{}", self.category, file_part, normalized)
    }

    /// Normalize file path for consistent signatures across different path formats.
    /// Handles: `./src/file.rs`, `src/file.rs`, `src\file.rs` (Windows) → `src/file.rs`
    fn normalize_path_for_signature(path: &str) -> String {
        let mut normalized = path
            // Convert Windows backslashes to forward slashes
            .replace('\\', "/")
            // Remove redundant ./ prefixes
            .trim_start_matches("./")
            .to_string();

        // Remove any double slashes
        while normalized.contains("//") {
            normalized = normalized.replace("//", "/");
        }

        normalized
    }

    fn normalize_for_signature(message: &str) -> String {
        let mut result = String::with_capacity(Self::SIGNATURE_MSG_LEN);
        let mut prev_was_space = false;

        for c in message.chars() {
            if result.len() >= Self::SIGNATURE_MSG_LEN {
                break;
            }

            if c.is_control() {
                continue;
            }

            if c.is_whitespace() {
                if !prev_was_space && !result.is_empty() {
                    result.push(' ');
                    prev_was_space = true;
                }
                continue;
            }

            result.push(c);
            prev_was_space = false;
        }

        result.trim().to_string()
    }

    /// Check if this issue matches another issue (same underlying problem)
    pub fn matches(&self, other: &Issue) -> bool {
        // Fast path: if both have file+line, compare with normalized paths
        if let (Some(f1), Some(l1), Some(f2), Some(l2)) =
            (&self.file, self.line, &other.file, other.line)
        {
            let norm1 = Self::normalize_path_for_signature(f1);
            let norm2 = Self::normalize_path_for_signature(f2);
            return self.category == other.category && norm1 == norm2 && l1 == l2;
        }
        self.signature() == other.signature()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    BuildError,
    TestFailure,
    LintError,
    TypeCheckError,
    RuntimeError,
    LogicError,
    SecurityIssue,
    PerformanceIssue,
    StyleViolation,
    MissingFile,
    /// Extensible category for emerging check types (Bazel, custom LSP, CI/CD, etc.)
    /// Use descriptive names like "bazel_build", "eslint_a11y", "ci_deploy".
    #[serde(rename = "custom")]
    Custom(String),
    Other,
}

impl From<Check> for IssueCategory {
    fn from(check: Check) -> Self {
        match check {
            Check::Build => IssueCategory::BuildError,
            Check::Test => IssueCategory::TestFailure,
            Check::Lint => IssueCategory::LintError,
            Check::TypeCheck => IssueCategory::TypeCheckError,
            Check::FileCreated | Check::FileModified | Check::FileDeleted => {
                IssueCategory::MissingFile
            }
            // Check::Custom maps to Other since it doesn't carry a name.
            // Use IssueCategory::Custom(name) directly for named categories.
            Check::Custom => IssueCategory::Other,
        }
    }
}

impl std::fmt::Display for IssueCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BuildError => write!(f, "Build"),
            Self::TestFailure => write!(f, "Test"),
            Self::LintError => write!(f, "Lint"),
            Self::TypeCheckError => write!(f, "Type"),
            Self::RuntimeError => write!(f, "Runtime"),
            Self::LogicError => write!(f, "Logic"),
            Self::SecurityIssue => write!(f, "Security"),
            Self::PerformanceIssue => write!(f, "Performance"),
            Self::StyleViolation => write!(f, "Style"),
            Self::MissingFile => write!(f, "File"),
            Self::Custom(name) => write!(f, "{}", name),
            Self::Other => write!(f, "Other"),
        }
    }
}

/// Category characteristics for LLM context.
#[derive(Debug, Clone)]
pub struct CategoryCharacteristics {
    pub determinism: DeterminismLevel,
    pub auto_fixable: AutoFixability,
    pub review_recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeterminismLevel {
    /// Same input always produces same output (static compiler errors)
    High,
    /// Usually deterministic but can vary (type errors in mixed static/dynamic code)
    Medium,
    /// Inherently variable (flaky tests, timing-dependent issues)
    Low,
    /// Requires judgment (logic, security, performance)
    Subjective,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoFixability {
    /// Can typically be fixed automatically (syntax, imports, type annotations)
    Likely,
    /// May be fixable depending on context
    Possible,
    /// Usually requires human input (credentials, external resources)
    Unlikely,
}

impl IssueCategory {
    /// Get characteristics for LLM context. Returns information about this category
    /// that helps LLM make informed decisions rather than binary classifications.
    pub fn characteristics(&self) -> CategoryCharacteristics {
        match self {
            Self::BuildError => CategoryCharacteristics {
                determinism: DeterminismLevel::High,
                auto_fixable: AutoFixability::Likely,
                review_recommended: false,
            },
            Self::TypeCheckError => CategoryCharacteristics {
                // Medium: static languages = High, dynamic languages = Low
                determinism: DeterminismLevel::Medium,
                auto_fixable: AutoFixability::Likely,
                review_recommended: false,
            },
            Self::TestFailure => CategoryCharacteristics {
                // Medium: unit tests usually deterministic, integration tests may be flaky
                determinism: DeterminismLevel::Medium,
                auto_fixable: AutoFixability::Possible,
                review_recommended: false,
            },
            Self::MissingFile => CategoryCharacteristics {
                determinism: DeterminismLevel::High,
                auto_fixable: AutoFixability::Likely,
                review_recommended: false,
            },
            Self::LintError | Self::StyleViolation => CategoryCharacteristics {
                determinism: DeterminismLevel::High,
                auto_fixable: AutoFixability::Likely,
                review_recommended: false,
            },
            Self::RuntimeError => CategoryCharacteristics {
                determinism: DeterminismLevel::Low,
                auto_fixable: AutoFixability::Possible,
                review_recommended: false,
            },
            Self::LogicError => CategoryCharacteristics {
                determinism: DeterminismLevel::Subjective,
                auto_fixable: AutoFixability::Possible,
                review_recommended: true,
            },
            Self::SecurityIssue => CategoryCharacteristics {
                determinism: DeterminismLevel::Subjective,
                auto_fixable: AutoFixability::Possible,
                review_recommended: true,
            },
            Self::PerformanceIssue => CategoryCharacteristics {
                determinism: DeterminismLevel::Subjective,
                auto_fixable: AutoFixability::Unlikely,
                review_recommended: true,
            },
            Self::Custom(_) | Self::Other => CategoryCharacteristics {
                determinism: DeterminismLevel::Medium,
                auto_fixable: AutoFixability::Possible,
                review_recommended: false,
            },
        }
    }

    /// Format characteristics for LLM prompt context.
    pub fn format_for_llm(&self) -> String {
        let chars = self.characteristics();
        let det = match chars.determinism {
            DeterminismLevel::High => "deterministic",
            DeterminismLevel::Medium => "usually deterministic",
            DeterminismLevel::Low => "may vary between runs",
            DeterminismLevel::Subjective => "requires judgment",
        };
        let fix = match chars.auto_fixable {
            AutoFixability::Likely => "usually auto-fixable",
            AutoFixability::Possible => "may be auto-fixable",
            AutoFixability::Unlikely => "likely needs human input",
        };
        format!("{} ({}, {})", self, det, fix)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

impl std::fmt::Display for IssueSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warning => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// LLM-based issue extraction for universal language support.
/// Replaces programmatic parsing with LLM classification.
pub struct IssueExtractor {
    agent: Arc<TaskAgent>,
    working_dir: std::path::PathBuf,
    max_output_length: usize,
}

/// Response from LLM issue extraction.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct IssueExtractionResponse {
    issues: Vec<ExtractedIssue>,
}

/// Single issue extracted by LLM.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct ExtractedIssue {
    /// Issue category determined by LLM
    category: IssueCategory,
    /// Issue severity determined by LLM
    severity: IssueSeverity,
    /// Error message (cleaned, normalized by LLM)
    message: String,
    /// File path if visible in output
    #[serde(default)]
    file: Option<String>,
    /// Line number if visible in output
    #[serde(default)]
    line: Option<u32>,
    /// LLM's confidence in this classification (0.0-1.0).
    /// Required - LLM must provide confidence for pattern learning.
    confidence: f32,
}

impl From<ExtractedIssue> for Issue {
    fn from(extracted: ExtractedIssue) -> Self {
        let mut issue = Issue::new(
            extracted.category,
            extracted.severity,
            extracted.message,
            extracted.confidence,
        );
        if let Some(file) = extracted.file {
            issue = issue.with_location(file, extracted.line.map(|l| l as usize));
        }
        issue
    }
}

/// Result of programmatic extraction attempt.
struct ProgrammaticResult {
    issues: Vec<Issue>,
    confidence: f32,
    needs_llm: bool,
}

impl IssueExtractor {
    const DEFAULT_MAX_OUTPUT_LENGTH: usize = 8000;
    const FALLBACK_LOW_CONFIDENCE: f32 = 0.2;

    /// Get check-type specific confidence for structured output parsing.
    ///
    /// Rationale for calibration by check_type:
    /// - Build/TypeCheck: Compiler output is standardized (file:line:col format)
    /// - Test: Output format varies by framework (pytest vs jest vs cargo test)
    /// - Lint: Output format varies by tool and configuration
    /// - Custom: Unknown tool, unknown reliability
    ///
    /// Note: These are for STRUCTURED parsing confidence, not semantic accuracy.
    /// Even with low parsing confidence, the extracted issues are still valid.
    fn structured_confidence_for(check_type: Check) -> f32 {
        match check_type {
            // Compiler output is highly standardized across languages
            Check::Build | Check::TypeCheck => 0.90,
            // File operations are deterministic
            Check::FileCreated | Check::FileModified | Check::FileDeleted => 0.95,
            // Test/Lint output varies significantly by tool
            Check::Test | Check::Lint => 0.75,
            // Custom checks have unknown output format
            Check::Custom => 0.60,
        }
    }

    /// Get medium confidence threshold for triggering LLM fallback.
    fn medium_confidence_for(check_type: Check) -> f32 {
        match check_type {
            Check::Build | Check::TypeCheck => 0.70,
            Check::FileCreated | Check::FileModified | Check::FileDeleted => 0.80,
            Check::Test | Check::Lint => 0.55,
            Check::Custom => 0.40,
        }
    }

    pub fn new(agent: Arc<TaskAgent>, working_dir: &Path) -> Self {
        Self {
            agent,
            working_dir: working_dir.to_path_buf(),
            max_output_length: Self::DEFAULT_MAX_OUTPUT_LENGTH,
        }
    }

    pub fn with_max_output_length(mut self, length: usize) -> Self {
        self.max_output_length = length;
        self
    }

    /// Two-stage extraction: programmatic first, LLM only for ambiguous cases.
    pub async fn extract(&self, output: &str, check_type: Check) -> Result<Vec<Issue>> {
        if output.trim().is_empty() {
            return Ok(vec![]);
        }

        // Stage 1: Try programmatic parsing for structured compiler output
        let programmatic = self.try_structured_extraction(output, check_type);

        // Use check-type specific thresholds (not one-size-fits-all)
        let medium_threshold = Self::medium_confidence_for(check_type);
        if !programmatic.needs_llm && programmatic.confidence >= medium_threshold {
            debug!(
                count = programmatic.issues.len(),
                confidence = programmatic.confidence,
                check = ?check_type,
                "Extracted issues via programmatic parsing (skipped LLM)"
            );
            return Ok(programmatic.issues);
        }

        // Stage 2: LLM for ambiguous/complex output
        let truncated_output = self.smart_truncate(output);
        let prompt = self.build_prompt(&truncated_output, check_type);

        match self
            .agent
            .run_prompt_structured::<IssueExtractionResponse>(&prompt, &self.working_dir)
            .await
        {
            Ok(response) => {
                let issues: Vec<Issue> = response.issues.into_iter().map(Issue::from).collect();
                debug!(count = issues.len(), check = ?check_type, "Extracted issues via LLM");
                Ok(issues)
            }
            Err(e) => {
                warn!(error = %e, check = ?check_type, "LLM extraction failed");
                // If programmatic had some results, use those
                if !programmatic.issues.is_empty() {
                    debug!("Using programmatic results as fallback");
                    Ok(programmatic.issues)
                } else {
                    Ok(self.fallback_extraction(output, check_type))
                }
            }
        }
    }

    /// Try to extract issues using structured compiler output patterns.
    /// Universal format: file:line:col: level: message
    fn try_structured_extraction(&self, output: &str, check_type: Check) -> ProgrammaticResult {
        // Pattern: file:line:col: error/warning: message
        // Works for: gcc, clang, rustc, tsc, eslint, go, python, etc.
        let structured_pattern = regex::Regex::new(
            r"(?m)^([^\s:]+):(\d+):(?:\d+:)?\s*(error|warning|note|info|hint):\s*(.+)$",
        )
        .unwrap();

        let mut issues = Vec::new();

        for cap in structured_pattern.captures_iter(output) {
            let file = cap.get(1).map(|m| m.as_str().to_string());
            let line = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok());
            let level = cap.get(3).map(|m| m.as_str()).unwrap_or("error");
            let message = cap.get(4).map(|m| m.as_str()).unwrap_or("");

            // Infer severity from check_type (language-agnostic) rather than English keywords.
            // The captured "level" may be in any language (error, erreur, Fehler, エラー).
            let severity = Self::infer_severity_from_check_type(check_type, level);

            let category = self.infer_category_from_message(message, check_type);

            // Use check-type calibrated confidence
            let confidence = Self::structured_confidence_for(check_type);
            let mut issue = Issue::new(category, severity, message.to_string(), confidence);
            if let Some(f) = file {
                issue = issue.with_location(f, line);
            }
            issues.push(issue);
        }

        // Check if we found structured output
        if issues.is_empty() {
            // No structured output found - needs LLM
            ProgrammaticResult {
                issues: vec![],
                confidence: 0.0,
                needs_llm: true,
            }
        } else {
            // Found structured output - check if output has more content
            let total_lines = output.lines().count();
            let parsed_lines = issues.len();
            let coverage = parsed_lines as f32 / total_lines.max(1) as f32;

            // If we parsed < 30% of lines, there's likely more content LLM could find
            let needs_llm = coverage < 0.3 && total_lines > 10;

            // Use check-type calibrated confidence
            let high_conf = Self::structured_confidence_for(check_type);
            let medium_conf = Self::medium_confidence_for(check_type);

            ProgrammaticResult {
                issues,
                confidence: if needs_llm { medium_conf } else { high_conf },
                needs_llm,
            }
        }
    }

    /// Infer category from message using check_type as primary signal.
    ///
    /// NOTE: This intentionally does NOT use keyword matching because:
    /// 1. Error messages vary by language, locale, and compiler version
    /// 2. English keywords don't work for non-English toolchains
    /// 3. The check_type from the verification system is more reliable
    /// 4. LLM-based extraction handles semantic classification better
    ///
    /// If more granular classification is needed, it should be done by the
    /// LLM in the second stage, not by keyword matching here.
    fn infer_category_from_message(&self, _message: &str, check_type: Check) -> IssueCategory {
        // Check type is the reliable, language-agnostic signal
        IssueCategory::from(check_type)
    }

    /// Infer severity from check_type (language-agnostic), using captured level as hint.
    ///
    /// The captured "level" may be in any language (error, Fehler, erreur, エラー).
    /// Instead of matching English keywords, we use check_type as the primary signal:
    /// - Build/TypeCheck → likely Error (compilation failures block execution)
    /// - Lint → likely Warning (style issues don't block execution)
    /// - Test → Error (test failures are actionable)
    fn infer_severity_from_check_type(check_type: Check, captured_level: &str) -> IssueSeverity {
        // Try known English patterns first (many compilers use English keywords)
        let level_lower = captured_level.to_lowercase();
        match level_lower.as_str() {
            "error" => return IssueSeverity::Error,
            "warning" | "warn" => return IssueSeverity::Warning,
            "note" | "info" | "hint" => return IssueSeverity::Info,
            _ => {}
        }

        // Unknown level text (possibly non-English) → infer from check_type
        match check_type {
            Check::Build | Check::TypeCheck | Check::Test => IssueSeverity::Error,
            Check::Lint => IssueSeverity::Warning,
            // File operations and custom checks default to Error (actionable)
            Check::FileCreated | Check::FileModified | Check::FileDeleted | Check::Custom => {
                IssueSeverity::Error
            }
        }
    }

    /// Smart truncation that preserves critical error information.
    ///
    /// Strategy: **Prioritize LAST blocks** for most check types because:
    /// - Compiler output: Final errors + "aborting due to N errors" summary
    /// - Test output: Test summary at the end
    /// - Stack traces: Often have root cause context at the end
    ///
    /// Allocation: 25% first (context), 75% last (critical info)
    /// This is language-agnostic: works for all compilers/test frameworks.
    fn smart_truncate(&self, output: &str) -> String {
        if output.len() <= self.max_output_length {
            return output.to_string();
        }

        let blocks = Self::split_into_blocks(output);
        if blocks.len() <= 2 {
            return Self::simple_truncate(output, self.max_output_length);
        }

        // Reserve space for context markers (helps LLM understand truncation)
        let marker_size = 150;
        let available = self.max_output_length.saturating_sub(marker_size);

        // Asymmetric allocation: 25% first, 75% last
        // Most critical info (errors, summaries) appear at the end
        let first_budget = available / 4;
        let last_budget = available - first_budget;

        // Take first block(s) for initial context
        let mut first_part = String::new();
        let mut first_blocks_taken = 0;
        for block in &blocks {
            if first_part.len() + block.len() + 2 <= first_budget {
                if !first_part.is_empty() {
                    first_part.push_str("\n\n");
                }
                first_part.push_str(block);
                first_blocks_taken += 1;
            } else {
                break;
            }
        }

        // Take last block(s) - prioritize these (critical errors/summary)
        let mut last_part = String::new();
        let mut last_blocks_taken = 0;
        for block in blocks.iter().rev() {
            if last_part.len() + block.len() + 2 <= last_budget {
                let prepend = if last_part.is_empty() {
                    block.to_string()
                } else {
                    format!("{block}\n\n{last_part}")
                };
                last_part = prepend;
                last_blocks_taken += 1;
            } else {
                break;
            }
        }

        let total_blocks = blocks.len();
        let omitted = total_blocks.saturating_sub(first_blocks_taken + last_blocks_taken);

        // Provide clear context markers for LLM
        if omitted > 0 {
            format!(
                "{first_part}\n\n\
                 [TRUNCATED: {omitted} of {total_blocks} blocks omitted ({original_len} chars total)]\n\
                 [Note: Last {last_blocks_taken} blocks preserved - likely contains error summary]\n\n\
                 {last_part}",
                original_len = output.len()
            )
        } else {
            format!("{first_part}\n\n{last_part}")
        }
    }

    fn split_into_blocks(output: &str) -> Vec<&str> {
        output
            .split("\n\n")
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    fn simple_truncate(output: &str, max_len: usize) -> String {
        if output.len() <= max_len {
            return output.to_string();
        }
        let half = max_len / 2;
        // Find safe UTF-8 boundaries
        let start_boundary = Self::safe_byte_boundary(output, half);
        let end_start = output.len().saturating_sub(half);
        let end_boundary = Self::safe_byte_boundary_from(output, end_start);
        let start = &output[..start_boundary];
        let end = &output[end_boundary..];
        format!("{start}\n[... truncated ...]\n{end}")
    }

    fn safe_byte_boundary(s: &str, max_bytes: usize) -> usize {
        if max_bytes >= s.len() {
            return s.len();
        }
        s.char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max_bytes)
            .last()
            .unwrap_or(0)
    }

    fn safe_byte_boundary_from(s: &str, min_bytes: usize) -> usize {
        if min_bytes == 0 {
            return 0;
        }
        s.char_indices()
            .map(|(i, _)| i)
            .find(|&i| i >= min_bytes)
            .unwrap_or(s.len())
    }

    fn build_prompt(&self, output: &str, check_type: Check) -> String {
        format!(
            r"Extract issues from this verification output (check_type: {check_type:?}).

## Output
```
{output}
```

## Instructions
For each issue found, provide:
- category: Classify by issue type (build, test, lint, type, runtime, logic, security, performance, style, missing_file, or other)
- severity: info, warning, error, or critical
- message: The cleaned error message (remove noise, keep essential info)
- file: File path if visible (null if not found)
- line: Line number if visible (null if not found)
- confidence: Your confidence in this classification (0.0-1.0)

Return empty array if no issues found.
Do NOT fabricate issues - only extract what's actually in the output."
        )
    }

    /// Fallback extraction when structured and LLM extraction fail.
    ///
    /// NOTE: This intentionally does NOT use keyword matching because:
    /// - English keywords ("error", "failed") don't work for non-English toolchains
    /// - German: "Fehler", French: "Erreur", Japanese: "エラー", etc.
    /// - Better to pass raw output to LLM than filter incorrectly
    ///
    /// Instead, we take the first and last non-empty lines to capture
    /// summary information that often appears at the beginning or end.
    fn fallback_extraction(&self, output: &str, check_type: Check) -> Vec<Issue> {
        if output.trim().is_empty() {
            return vec![];
        }

        // Take first 5 and last 5 non-empty lines for context
        // Many tools show summary at beginning or end
        let non_empty: Vec<&str> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();

        let message = if non_empty.len() <= 10 {
            // Short output - use all of it
            non_empty.join("\n")
        } else {
            // Long output - take first 5 and last 5 lines
            let first = &non_empty[..5];
            let last = &non_empty[non_empty.len() - 5..];
            format!(
                "{}\n... ({} lines omitted) ...\n{}",
                first.join("\n"),
                non_empty.len() - 10,
                last.join("\n")
            )
        };

        if message.trim().is_empty() {
            return vec![];
        }

        let category = IssueCategory::from(check_type);

        vec![Issue::new(
            category,
            IssueSeverity::Error,
            message,
            Self::FALLBACK_LOW_CONFIDENCE,
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONFIDENCE: f32 = 0.8;

    #[test]
    fn test_normalize_preserves_numbers() {
        let msg1 = "expected 3 args, got 5";
        let msg2 = "expected 10 args, got 8";
        let norm1 = Issue::normalize_for_signature(msg1);
        let norm2 = Issue::normalize_for_signature(msg2);
        assert_ne!(
            norm1, norm2,
            "Different numbers should produce different signatures"
        );
        assert!(norm1.contains("3") && norm1.contains("5"));
        assert!(norm2.contains("10") && norm2.contains("8"));
    }

    #[test]
    fn test_normalize_preserves_case() {
        let msg = "Error: MyClass not found";
        let norm = Issue::normalize_for_signature(msg);
        assert!(
            norm.contains("MyClass"),
            "Case should be preserved for identifiers"
        );
    }

    #[test]
    fn test_normalize_preserves_quoted_strings() {
        let msg = r#"cannot find "MyModule" in scope"#;
        let norm = Issue::normalize_for_signature(msg);
        assert!(
            norm.contains("\"MyModule\""),
            "Quoted strings should be preserved"
        );
    }

    #[test]
    fn test_normalize_preserves_port_numbers() {
        let msg1 = "connection refused on port 8080";
        let msg2 = "connection refused on port 8081";
        let norm1 = Issue::normalize_for_signature(msg1);
        let norm2 = Issue::normalize_for_signature(msg2);
        assert_ne!(norm1, norm2, "Port numbers are semantically meaningful");
    }

    #[test]
    fn test_normalize_collapses_whitespace() {
        let msg1 = "error:   multiple   spaces";
        let msg2 = "error: multiple spaces";
        assert_eq!(
            Issue::normalize_for_signature(msg1),
            Issue::normalize_for_signature(msg2)
        );
    }

    #[test]
    fn test_signature_with_file_and_line() {
        let issue = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "some error",
            TEST_CONFIDENCE,
        )
        .with_location("src/main.rs", Some(42));
        assert_eq!(issue.signature(), "Build:src/main.rs:42");
    }

    #[test]
    fn test_signature_preserves_semantic_differences() {
        let issue1 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "expected 3 arguments but got 5",
            TEST_CONFIDENCE,
        );
        let issue2 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "expected 10 arguments but got 2",
            TEST_CONFIDENCE,
        );
        assert_ne!(issue1.signature(), issue2.signature());
    }

    #[test]
    fn test_matches_same_location() {
        let issue1 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error 1",
            TEST_CONFIDENCE,
        )
        .with_location("src/lib.rs", Some(10));
        let issue2 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error 2",
            TEST_CONFIDENCE,
        )
        .with_location("src/lib.rs", Some(10));
        assert!(issue1.matches(&issue2));
    }

    #[test]
    fn test_matches_different_location() {
        let issue1 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error",
            TEST_CONFIDENCE,
        )
        .with_location("src/lib.rs", Some(10));
        let issue2 = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error",
            TEST_CONFIDENCE,
        )
        .with_location("src/lib.rs", Some(20));
        assert!(!issue1.matches(&issue2));
    }

    #[test]
    fn test_smart_truncate_preserves_first_and_last() {
        let blocks: Vec<String> = (0..10)
            .map(|i| format!("Error block {i}: some error message here"))
            .collect();
        let output = blocks.join("\n\n");
        let result = IssueExtractor::simple_truncate(&output, 200);
        assert!(result.contains("Error block 0"));
        assert!(result.contains("Error block 9"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_split_into_blocks() {
        let output = "block1\n\nblock2\n\nblock3";
        let blocks = IssueExtractor::split_into_blocks(output);
        assert_eq!(blocks.len(), 3);
    }
}
