use std::path::Path;

use tracing::warn;

use super::super::issue::{Issue, IssueCategory};
use crate::domain::Severity;
use super::super::responses::AiReviewResponse;
use super::super::Verification;
use super::{ConvergentVerifier, FixPromptParams};
use crate::error::Result;

impl ConvergentVerifier {
    pub(super) fn build_fix_prompt(&self, params: &FixPromptParams<'_>) -> String {
        let mission_ctx = params
            .mission
            .map(|m| format!("Mission: {}\n\n", m.description))
            .unwrap_or_default();

        let history_ctx = params
            .similar_fixes_ctx
            .map(|ctx| format!("{}\n", ctx))
            .unwrap_or_default();

        let pattern_ctx_str = params
            .pattern_ctx
            .map(|ctx| format!("\n{}\n", ctx.format_for_llm()))
            .unwrap_or_default();

        let issue = params.issue;
        let affected_files: Vec<&str> = issue.file.as_deref().into_iter().collect();
        let project_ctx = Self::detect_project_context(
            params.working_dir,
            &affected_files,
            issue.file.as_deref(),
        );

        format!(
            r"## Fix Required

{mission_ctx}Issue:
- Category: {:?}
- Severity: {:?}
- Message: {}
{}{}{}
{pattern_ctx_str}Strategy: {:?} - {}
{history_ctx}
Attempt {} of {}. Apply minimal, focused fix.",
            issue.category,
            issue.severity,
            issue.message,
            issue
                .file
                .as_ref()
                .map(|f| format!("- File: {}\n", f))
                .unwrap_or_default(),
            issue
                .line
                .map(|l| format!("- Line: {}\n", l))
                .unwrap_or_default(),
            project_ctx,
            params.strategy,
            params.strategy.prompt_hint(),
            params.attempts + 1,
            self.config.max_fix_attempts_per_issue,
        )
    }

    /// Detect project context: build system and languages from affected files.
    pub(super) fn detect_project_context(
        working_dir: &Path,
        affected_files: &[&str],
        primary_file: Option<&str>,
    ) -> String {
        let mut ctx = String::new();

        if let Some(build_system) = crate::config::BuildSystem::detect(working_dir) {
            ctx.push_str(&format!("- Build System: {:?}\n", build_system));
        }

        let mut languages: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for file in affected_files {
            if let Some(lang) = Self::extension_to_language(file) {
                languages.insert(lang);
            }
        }

        if let Some(primary) = primary_file
            && let Some(hints) = Self::get_language_hints(primary)
        {
            ctx.push_str(&hints);
        }

        if languages.len() > 1 {
            let langs: Vec<_> = languages.into_iter().collect();
            ctx.push_str(&format!(
                "- Note: Polyglot project ({}) - ensure cross-language compatibility\n",
                langs.join(", ")
            ));
        }

        ctx
    }

    pub(super) fn extension_to_language(file_path: &str) -> Option<&'static str> {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())?;

        match ext {
            "rs" => Some("Rust"),
            "ts" | "tsx" => Some("TypeScript"),
            "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
            "py" | "pyi" => Some("Python"),
            "go" => Some("Go"),
            "java" => Some("Java"),
            "kt" | "kts" => Some("Kotlin"),
            "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
            "c" | "h" => Some("C"),
            "swift" => Some("Swift"),
            "rb" => Some("Ruby"),
            "php" => Some("PHP"),
            "cs" => Some("C#"),
            "scala" => Some("Scala"),
            _ => None,
        }
    }

    pub(super) fn get_language_hints(file_path: &str) -> Option<String> {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())?;

        let (lang, hints) = match ext {
            "rs" => (
                "Rust",
                "Check ownership/borrowing; Verify trait implementations; Handle Result/Option",
            ),
            "ts" | "tsx" => (
                "TypeScript",
                "Check type annotations; Verify strict null checks; Handle async/await",
            ),
            "js" | "jsx" => (
                "JavaScript",
                "Check undefined/null; Verify async patterns; Consider compatibility",
            ),
            "py" | "pyi" => (
                "Python",
                "Check type hints; Verify indentation; Consider version compatibility",
            ),
            "go" => (
                "Go",
                "Check error handling; Verify goroutine safety; Handle nil",
            ),
            "java" => (
                "Java",
                "Check null safety; Verify exception handling; Consider generics",
            ),
            "kt" | "kts" => (
                "Kotlin",
                "Check nullable types; Verify suspend functions; Consider Java interop",
            ),
            "cpp" | "cc" | "cxx" => (
                "C++",
                "Check memory management; Verify templates; Use RAII patterns",
            ),
            "c" | "h" => (
                "C",
                "Check pointer safety; Verify memory alloc/dealloc; Consider platform",
            ),
            "swift" => (
                "Swift",
                "Check optional unwrapping; Verify protocols; Handle ARC",
            ),
            "rb" => (
                "Ruby",
                "Check method visibility; Verify blocks/procs; Handle nil",
            ),
            _ => return None,
        };

        Some(format!("- Language: {} ({})\n", lang, hints))
    }

    /// Extract issues from verification using LLM-based IssueExtractor.
    pub(super) async fn extract_issues(&self, verification: &Verification) -> Vec<Issue> {
        const CHECK_FAILURE_CONFIDENCE: f32 = 0.95;
        const FALLBACK_ISSUE_CONFIDENCE: f32 = 0.60;
        const INCOMPLETE_CHECK_CONFIDENCE: f32 = 0.95;

        let mut issues: Vec<Issue> = Vec::new();

        for check_result in verification.failed_checks() {
            let output = check_result.output.as_deref().unwrap_or("");
            if output.trim().is_empty() {
                issues.push(Issue::new(
                    IssueCategory::from(check_result.check),
                    Severity::Error,
                    format!("{} failed (no output)", check_result.name),
                    CHECK_FAILURE_CONFIDENCE,
                ));
                continue;
            }

            let extracted = match self
                .issue_extractor
                .extract(output, check_result.check)
                .await
            {
                Ok(extracted) if !extracted.is_empty() => extracted,
                Ok(_) => Vec::new(),
                Err(e) => {
                    warn!(error = %e, check = ?check_result.check, "Issue extraction failed");
                    Vec::new()
                }
            };

            if extracted.is_empty() {
                issues.push(Issue::new(
                    IssueCategory::from(check_result.check),
                    Severity::Error,
                    format!(
                        "{} failed: {}",
                        check_result.name,
                        crate::utils::truncate_str(output, 500)
                    ),
                    FALLBACK_ISSUE_CONFIDENCE,
                ));
            } else {
                issues.extend(extracted);
            }
        }

        if verification.checks.is_empty() && !verification.passed {
            issues.push(Issue::new(
                IssueCategory::Other,
                Severity::Error,
                "Incomplete verification: no checks were performed. Task may not have been implemented.",
                INCOMPLETE_CHECK_CONFIDENCE,
            ));
        }

        issues
    }

    /// Run AI review with perspective differentiation for adversarial 2-pass verification.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_ai_review(
        &self,
        description: &str,
        file_changes: Option<(&[String], &[String])>,
        diff: Option<&str>,
        working_dir: &Path,
        adversarial: bool,
        first_pass_context: Option<&str>,
        semantic_context: Option<&str>,
    ) -> Result<(Vec<Issue>, String)> {
        let file_section = match file_changes {
            Some((created, modified)) if !created.is_empty() || !modified.is_empty() => {
                let mut section = String::from("\n## Files Changed\n");
                if !created.is_empty() {
                    section.push_str("Created:\n");
                    for f in created {
                        section.push_str(&format!("  - {}\n", f));
                    }
                }
                if !modified.is_empty() {
                    section.push_str("Modified:\n");
                    for f in modified {
                        section.push_str(&format!("  - {}\n", f));
                    }
                }
                section
            }
            _ => String::from(
                "\n(No specific file changes provided - use Glob to find relevant files)\n",
            ),
        };

        let diff_section = if let Some(d) = diff {
            let max_len = self.config.diff_truncation_length;
            let truncated = if d.len() > max_len {
                format!(
                    "{}...\n(diff truncated at {} chars, use Read tool for full content)",
                    crate::utils::truncate_str(d, max_len),
                    max_len
                )
            } else {
                d.to_string()
            };
            format!(
                "\n## Actual Changes (git diff)\n```diff\n{}\n```\n\nFocus your review on these specific changes.\n",
                truncated
            )
        } else {
            String::from("\n(No diff available - use Read tool to examine files)\n")
        };

        let semantic_section = semantic_context
            .filter(|s| !s.is_empty())
            .map(|s| format!("\n{}\n", s))
            .unwrap_or_default();

        let language_detection = detect_primary_language(file_changes);
        let checklist_section = format!(
            r"
## Review Guidelines
{}
Evaluate the changes for quality issues:
1. **Correctness**: Does the implementation match requirements?
2. **Edge Cases**: Are boundary conditions and error paths handled?
3. **Code Quality**: Are there maintainability or readability concerns?

Report issues with severity (Critical/Error/Warning/Info).
",
            language_detection
                .map(|d| format!("{}\n", d.format_for_llm()))
                .unwrap_or_default()
        );

        let prompt = if adversarial {
            let first_pass_section = first_pass_context
                .map(|ctx| {
                    format!(
                        r"
## First Pass Results
{}

Focus on dimensions the first pass marked as PASS but may have missed issues.
",
                        ctx
                    )
                })
                .unwrap_or_default();

            format!(
                r"## Adversarial Code Review (2nd Pass): {}
{file_section}{diff_section}{semantic_section}{first_pass_section}{checklist_section}
You are a skeptical reviewer. Assume bugs exist that the first pass missed.

INSTRUCTIONS:
1. Re-examine each checklist dimension critically
2. Use Read tool to verify claims from first pass
3. Focus on edge cases and error paths not explicitly tested
4. Mark passed=false if you find ANY issue in that dimension",
                description
            )
        } else {
            format!(
                r"## Systematic Code Review (1st Pass): {}
{file_section}{diff_section}{semantic_section}{checklist_section}
INSTRUCTIONS:
1. Examine the diff and complete EVERY checklist item
2. Use Read tool for context around changes
3. For each dimension: set checked=true, evaluate passed, list files_reviewed
4. Add issues array for any problems found

The adversarial 2nd pass will challenge your PASS decisions.",
                description
            )
        };

        let response: AiReviewResponse = self.agent.run_prompt_review(&prompt, working_dir).await?;

        if response.no_issues() {
            let context = response.checked_areas().join("\n");
            return Ok((Vec::new(), context));
        }

        let issues = self.build_issues(&response);
        let context = response.checked_areas().join("\n");
        Ok((issues, context))
    }

    pub(super) fn build_issues(&self, response: &AiReviewResponse) -> Vec<Issue> {
        const CHECKLIST_FAILURE_CONFIDENCE: f32 = 0.70;

        let mut issues: Vec<Issue> = response
            .issues
            .iter()
            .map(|ai_issue| {
                let mut issue = Issue::new(
                    ai_issue.category.clone(),
                    ai_issue.severity,
                    &ai_issue.message,
                    ai_issue.confidence,
                );
                if let Some(ref file) = ai_issue.file {
                    issue = issue.with_location(file, ai_issue.line.map(|l| l as usize));
                }
                issue
            })
            .collect();

        for (dimension, item) in response.checklist.failed_items() {
            let message = if let Some(notes) = &item.notes {
                format!(
                    "Checklist dimension '{}' failed review: {}",
                    dimension, notes
                )
            } else {
                format!("Checklist dimension '{}' failed review", dimension)
            };
            issues.push(Issue::new(
                IssueCategory::Other,
                Severity::Warning,
                message,
                CHECKLIST_FAILURE_CONFIDENCE,
            ));
        }

        issues
    }
}

/// Language detection result with confidence for LLM context.
#[derive(Clone)]
pub(crate) struct DetectedLanguage {
    pub(crate) name: &'static str,
    pub(crate) confidence: LanguageConfidence,
}

pub(crate) struct LanguageDetection {
    pub(crate) languages: Vec<DetectedLanguage>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LanguageConfidence {
    High,
    Medium,
    Low,
}

impl LanguageDetection {
    pub(crate) fn format_for_llm(&self) -> String {
        if self.languages.is_empty() {
            return String::new();
        }

        let formatted: Vec<String> = self
            .languages
            .iter()
            .take(4)
            .map(|l| {
                let conf = match l.confidence {
                    LanguageConfidence::High => "verified",
                    LanguageConfidence::Medium => "likely",
                    LanguageConfidence::Low => "possible",
                };
                format!("{} ({})", l.name, conf)
            })
            .collect();

        format!(
            "Detected languages: {}. Review changes considering relevant language concerns.",
            formatted.join(", ")
        )
    }
}

/// Detects primary language from file extensions with weighted confidence.
pub(crate) fn detect_primary_language(
    file_changes: Option<(&[String], &[String])>,
) -> Option<LanguageDetection> {
    let (created, modified) = file_changes?;
    let all_files: Vec<_> = created.iter().chain(modified.iter()).collect();

    if all_files.is_empty() {
        return None;
    }

    let build_markers = [
        ("Cargo.toml", "Rust", 3),
        ("Cargo.lock", "Rust", 3),
        ("package.json", "JavaScript", 2),
        ("tsconfig.json", "TypeScript", 3),
        ("go.mod", "Go", 3),
        ("go.sum", "Go", 3),
        ("pom.xml", "Java", 3),
        ("build.gradle", "Java", 2),
        ("build.gradle.kts", "Kotlin", 3),
        ("requirements.txt", "Python", 2),
        ("pyproject.toml", "Python", 3),
        ("setup.py", "Python", 3),
        ("Gemfile", "Ruby", 3),
        ("mix.exs", "Elixir", 3),
        ("pubspec.yaml", "Dart", 3),
    ];

    let mut scores: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut has_build_marker = false;

    for file in &all_files {
        let filename = std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        for (marker, lang, weight) in &build_markers {
            if filename == *marker {
                *scores.entry(lang).or_default() += *weight;
                has_build_marker = true;
            }
        }
    }

    let ext_map: &[(&[&str], &'static str, usize)] = &[
        (&["rs"], "Rust", 2),
        (&["py", "pyi"], "Python", 2),
        (&["ts", "tsx", "mts"], "TypeScript", 2),
        (&["js", "jsx", "mjs"], "JavaScript", 2),
        (&["go"], "Go", 2),
        (&["java"], "Java", 2),
        (&["kt", "kts"], "Kotlin", 2),
        (&["rb"], "Ruby", 2),
        (&["cpp", "cc", "cxx"], "C++", 2),
        (&["hpp", "hxx", "h"], "C++", 1),
        (&["c"], "C", 2),
        (&["swift"], "Swift", 2),
        (&["cs"], "C#", 2),
        (&["php"], "PHP", 2),
        (&["scala"], "Scala", 2),
        (&["ex", "exs"], "Elixir", 2),
        (&["hs", "lhs"], "Haskell", 2),
        (&["clj", "cljs", "cljc"], "Clojure", 2),
        (&["lua"], "Lua", 2),
        (&["zig"], "Zig", 2),
        (&["nim"], "Nim", 2),
        (&["dart"], "Dart", 2),
        (&["vue"], "Vue", 2),
        (&["svelte"], "Svelte", 2),
    ];

    for file in &all_files {
        if let Some(ext) = std::path::Path::new(file)
            .extension()
            .and_then(|e| e.to_str())
        {
            for (exts, lang, weight) in ext_map {
                if exts.contains(&ext) {
                    *scores.entry(lang).or_default() += *weight;
                }
            }
        }
    }

    if scores.is_empty() {
        return None;
    }

    let mut sorted: Vec<_> = scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    let languages: Vec<DetectedLanguage> = sorted
        .into_iter()
        .filter(|(_, score)| *score >= 2)
        .map(|(lang, score)| {
            let confidence = if has_build_marker && score >= 3 {
                LanguageConfidence::High
            } else if score >= 4 {
                LanguageConfidence::Medium
            } else {
                LanguageConfidence::Low
            };
            DetectedLanguage {
                name: lang,
                confidence,
            }
        })
        .collect();

    if languages.is_empty() {
        return None;
    }

    Some(LanguageDetection { languages })
}
