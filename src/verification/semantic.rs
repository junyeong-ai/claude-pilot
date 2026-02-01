//! LSP-powered semantic validation via Symora.

use std::path::Path;

use tracing::{debug, info};

use super::issue::{Issue, IssueCategory, IssueSeverity};
use crate::error::Result;
use crate::symora::{Diagnostic, DiagnosticSeverity, SymoraClient};

#[derive(Debug, Clone, Default)]
pub struct SemanticValidation {
    pub diagnostic_issues: Vec<Issue>,
    pub files_analyzed: Vec<String>,
    pub files_skipped: Vec<String>,
    pub symora_available: bool,
}

impl SemanticValidation {
    pub fn unavailable() -> Self {
        Self {
            symora_available: false,
            ..Default::default()
        }
    }

    pub fn has_issues(&self) -> bool {
        !self.diagnostic_issues.is_empty()
    }

    pub fn error_count(&self) -> usize {
        self.diagnostic_issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostic_issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Warning)
            .count()
    }

    pub fn to_review_context(&self) -> String {
        if !self.symora_available {
            return String::from("(Semantic analysis unavailable - Symora not installed)\n");
        }

        if self.diagnostic_issues.is_empty() {
            return format!(
                "## Semantic Analysis\n✓ {} files analyzed, no issues detected\n",
                self.files_analyzed.len()
            );
        }

        let mut context = format!(
            "## Semantic Analysis (Symora)\n{} errors, {} warnings in {} files:\n\n",
            self.error_count(),
            self.warning_count(),
            self.files_analyzed.len()
        );

        for issue in &self.diagnostic_issues {
            let severity = match issue.severity {
                IssueSeverity::Error | IssueSeverity::Critical => "ERROR",
                IssueSeverity::Warning => "WARN",
                IssueSeverity::Info => "INFO",
            };
            context.push_str(&format!("- [{}] {}\n", severity, issue.message));
            if let Some(ref file) = issue.file {
                if let Some(line) = issue.line {
                    context.push_str(&format!("  at {}:{}\n", file, line));
                } else {
                    context.push_str(&format!("  in {}\n", file));
                }
            }
        }

        context
    }
}

/// LSP-powered semantic validator using Symora.
pub struct SemanticValidator {
    symora: SymoraClient,
}

impl SemanticValidator {
    pub fn new(working_dir: &Path, timeout_secs: u64) -> Self {
        Self {
            symora: SymoraClient::new(working_dir, timeout_secs),
        }
    }

    pub async fn is_available(&self) -> bool {
        self.symora.is_available().await
    }

    pub async fn validate_files(&self, files: &[String]) -> SemanticValidation {
        if !self.symora.is_available().await {
            debug!("Symora not available for semantic validation");
            return SemanticValidation::unavailable();
        }

        let mut result = SemanticValidation {
            symora_available: true,
            ..Default::default()
        };

        for file in files {
            // Skip non-code files
            if !is_code_file(file) {
                result.files_skipped.push(file.clone());
                continue;
            }

            match self.validate_file(file).await {
                Ok(issues) => {
                    result.files_analyzed.push(file.clone());
                    result.diagnostic_issues.extend(issues);
                }
                Err(e) => {
                    debug!(file, error = %e, "Failed to validate file");
                    result.files_skipped.push(file.clone());
                }
            }
        }

        if !result.diagnostic_issues.is_empty() {
            info!(
                files_analyzed = result.files_analyzed.len(),
                errors = result.error_count(),
                warnings = result.warning_count(),
                "Semantic validation completed with issues"
            );
        } else {
            debug!(
                files_analyzed = result.files_analyzed.len(),
                "Semantic validation completed, no issues"
            );
        }

        result
    }

    async fn validate_file(&self, file: &str) -> Result<Vec<Issue>> {
        let diagnostics_result = self.symora.diagnostics(file, true, true).await?;

        let issues: Vec<Issue> = diagnostics_result
            .diagnostics
            .iter()
            .filter_map(|d| convert_diagnostic(d, file))
            .collect();

        Ok(issues)
    }
}

/// Convert Symora diagnostic to verification Issue.
fn convert_diagnostic(diagnostic: &Diagnostic, file: &str) -> Option<Issue> {
    // Skip hints and info-level diagnostics (too noisy for verification)
    let severity = match diagnostic.severity {
        DiagnosticSeverity::Error => IssueSeverity::Error,
        DiagnosticSeverity::Warning => IssueSeverity::Warning,
        DiagnosticSeverity::Information | DiagnosticSeverity::Hint => return None,
    };

    const LSP_DIAGNOSTIC_CONFIDENCE: f32 = 0.95;

    // Categorize using error codes and file extension (universal signals)
    let category = categorize_diagnostic(&diagnostic.message, file);

    Some(
        Issue::new(
            category,
            severity,
            &diagnostic.message,
            LSP_DIAGNOSTIC_CONFIDENCE,
        )
        .with_location(file, Some(diagnostic.line as usize)),
    )
}

/// Categorize diagnostic using universal signals (error codes, file extension).
///
/// This is a "golden heuristic" - uses deterministic patterns that work across
/// all languages without relying on English keyword matching.
///
/// ## Strategy
/// 1. Extract error code from message (E0277, TS2322, CS8602, etc.)
/// 2. Classify by error code prefix (language-specific but universal pattern)
/// 3. Fall back to language default based on file extension
fn categorize_diagnostic(message: &str, file: &str) -> IssueCategory {
    // Phase 1: Try to extract error code from message
    // Error codes follow universal patterns: letter(s) + digits
    // Examples: E0277 (Rust), TS2322 (TypeScript), CS8602 (C#), E1101 (pylint)
    if let Some(category) = extract_category_from_error_code(message) {
        return category;
    }

    // Phase 2: Use file extension to determine language-appropriate default
    // LSP diagnostics from language servers are almost always type/build errors
    let extension = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    match extension.to_lowercase().as_str() {
        // Statically typed languages: LSP diagnostics are usually type errors
        "rs" | "ts" | "tsx" | "go" | "java" | "kt" | "cs" | "fs" | "scala" => {
            IssueCategory::TypeCheckError
        }
        // Dynamically typed: LSP catches fewer type issues, more likely lint/style
        "py" | "js" | "jsx" | "rb" | "php" | "lua" => IssueCategory::LintError,
        // Default: treat as build error (conservative)
        _ => IssueCategory::BuildError,
    }
}

/// Extract category from error code patterns in message.
///
/// Error codes are universal across languages - each language has distinct prefixes:
/// - Rust: E0277, E0308 (type errors), E0425 (unresolved)
/// - TypeScript: TS2322, TS2345 (type errors), TS1005 (syntax)
/// - C#: CS8602, CS0029 (type errors), CS1002 (syntax)
/// - Go: (no standard codes, returns None)
/// - Python/pylint: E1101, W0612 (various)
/// - ESLint: no-unused-vars, @typescript-eslint/* (lint rules)
fn extract_category_from_error_code(message: &str) -> Option<IssueCategory> {
    // Look for error code pattern at start of message or after common delimiters
    // Pattern: word boundary + 1-3 uppercase letters + 4-5 digits
    let bytes = message.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Find potential start of error code (uppercase letter)
        if bytes[i].is_ascii_uppercase() {
            let start = i;
            let mut letters = 0;
            let mut digits = 0;

            // Count uppercase letters (1-3)
            while i < bytes.len() && bytes[i].is_ascii_uppercase() {
                letters += 1;
                i += 1;
            }

            // Count digits (4-5)
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                digits += 1;
                i += 1;
            }

            // Valid error code pattern
            if (1..=3).contains(&letters) && (4..=5).contains(&digits) {
                let code = &message[start..i];
                return categorize_by_error_code(code);
            }
        } else {
            i += 1;
        }
    }

    // Check for eslint-style rules: word-word or @scope/rule-name
    if message.contains("eslint") || message.contains("@typescript-eslint") {
        return Some(IssueCategory::LintError);
    }

    None
}

/// Map error code to category based on language conventions.
fn categorize_by_error_code(code: &str) -> Option<IssueCategory> {
    let prefix = code
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>();

    match prefix.as_str() {
        // Rust: E = errors (mostly type/borrow checker)
        "E" if code.starts_with("E0") => Some(IssueCategory::TypeCheckError),
        // TypeScript: TS = type errors
        "TS" => Some(IssueCategory::TypeCheckError),
        // C#: CS = compiler errors
        "CS" => Some(IssueCategory::TypeCheckError),
        // Python linters: E/W/C/R = pylint categories
        "E" | "W" | "C" | "R" if code.len() == 5 => Some(IssueCategory::LintError),
        // Java: (usually no prefix, handled by default)
        _ => None,
    }
}

fn is_code_file(path: &str) -> bool {
    let file_path = Path::new(path);

    // Check by extension first (covers most cases)
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        let ext_lower = ext.to_lowercase();
        // Comprehensive list of code extensions
        let code_extensions = [
            // Systems: Rust, C/C++, Go
            "rs", "c", "cpp", "cc", "cxx", "h", "hpp", "hxx", "go",
            // JVM: Java, Kotlin, Scala, Groovy, Clojure
            "java", "kt", "kts", "scala", "groovy", "gradle", "clj", "cljs", "cljc",
            // .NET: C#, F#
            "cs", "fs", "fsx", // Web: JavaScript, TypeScript, HTML, CSS
            "js", "jsx", "ts", "tsx", "mjs", "cjs", "vue", "svelte", "html", "css", "scss", "sass",
            "less", // Python
            "py", "pyx", "pyi", "pyw", // Ruby
            "rb", "erb", "rake", // PHP
            "php", "phtml", // Swift/Objective-C
            "swift", "m", "mm", // Shell/Script
            "sh", "bash", "zsh", "fish", "ps1", "psm1",
            // Functional: Haskell, Elixir, Erlang, OCaml, F#
            "hs", "lhs", "ex", "exs", "erl", "hrl", "ml", "mli", // Other languages
            "lua", "pl", "pm", "r", "jl", "dart", "nim", "zig", "v",
            // Config/Data (often contain code-like content)
            "toml", "yaml", "yml", "json", "jsonc", "json5", "xml", "plist",
            // Build files
            "cmake", "make", "mk", "bazel", "bzl", // SQL
            "sql",
        ];
        return code_extensions.contains(&ext_lower.as_str());
    }

    // Handle extensionless files by filename pattern
    if let Some(filename) = file_path.file_name().and_then(|n| n.to_str()) {
        // Common extensionless code/build files
        let extensionless_files = [
            "Makefile",
            "makefile",
            "GNUmakefile",
            "Dockerfile",
            "Containerfile",
            "Rakefile",
            "Gemfile",
            "Guardfile",
            "BUILD",
            "WORKSPACE",
            "BUILD.bazel",
            "WORKSPACE.bazel",
            "Procfile",
            "Brewfile",
            "Vagrantfile",
            "Berksfile",
            "Justfile",
            "justfile",
            "CMakeLists.txt", // Has extension but commonly overlooked
        ];
        if extensionless_files.contains(&filename) {
            return true;
        }

        // Pattern-based detection for other extensionless files:
        // - All caps (likely build files): BUILD, OWNERS, DEPS
        // - Ends with "file" (Makefile-like): Jakefile, Cakefile
        let is_all_caps = !filename.is_empty()
            && filename
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c == '.');
        let ends_with_file = filename.to_lowercase().ends_with("file");

        if is_all_caps || ends_with_file {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_code_file() {
        assert!(is_code_file("src/main.rs"));
        assert!(is_code_file("lib/utils.ts"));
        assert!(is_code_file("app.py"));
        assert!(!is_code_file("README.md"));
        assert!(!is_code_file("image.png"));
    }

    #[test]
    fn test_categorize_diagnostic_by_error_code() {
        // Rust error codes → TypeCheckError
        assert_eq!(
            categorize_diagnostic("E0277: trait bound not satisfied", "src/lib.rs"),
            IssueCategory::TypeCheckError
        );

        // TypeScript error codes → TypeCheckError
        assert_eq!(
            categorize_diagnostic(
                "TS2322: Type 'string' is not assignable to type 'number'",
                "app.ts"
            ),
            IssueCategory::TypeCheckError
        );

        // Unknown code, static language → TypeCheckError (from file extension)
        assert_eq!(
            categorize_diagnostic("cannot find name 'foo'", "src/main.rs"),
            IssueCategory::TypeCheckError
        );

        // Unknown code, dynamic language → LintError (from file extension)
        assert_eq!(
            categorize_diagnostic("undefined variable", "script.py"),
            IssueCategory::LintError
        );

        // ESLint style rule → LintError
        assert_eq!(
            categorize_diagnostic("eslint: no-unused-vars", "app.js"),
            IssueCategory::LintError
        );

        // Unknown extension → BuildError (conservative default)
        assert_eq!(
            categorize_diagnostic("some error", "unknown.xyz"),
            IssueCategory::BuildError
        );
    }

    #[test]
    fn test_semantic_validation_unavailable() {
        let validation = SemanticValidation::unavailable();
        assert!(!validation.symora_available);
        assert!(!validation.has_issues());
    }
}
