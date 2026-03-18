use std::path::Path;
use std::time::Duration;

use tracing::{debug, info, warn};

use super::super::checks::VerificationScope;
use super::super::history::{ConvergenceState, FixAttempt, FixStrategy};
use super::super::issue::{Issue, IssueCategory};
use crate::domain::Severity;
use super::super::pattern_bank::ErrorSignature;
use super::types::PatternContext;
use super::ConvergentVerifier;
use crate::error::Result;
use crate::mission::Mission;
use crate::search::{FixDetail, FixLoader, FixRecord, SearchQuery};
use crate::utils::truncate_str;

impl ConvergentVerifier {
    pub(super) async fn fix_issues_batched(
        &self,
        issues: &[Issue],
        state: &mut ConvergenceState,
        mission: Option<&Mission>,
        scope: &VerificationScope,
        working_dir: &Path,
    ) -> Result<(Vec<FixAttempt>, Vec<Issue>)> {
        let mut all_fixes = Vec::new();
        let mut all_exhausted = Vec::new();

        let sorted_issues = self.sort_by_priority(issues);

        for batch in sorted_issues.chunks(self.config.max_issues_per_batch) {
            let (fixes, exhausted) = self.fix_batch(batch, state, mission, working_dir).await?;
            all_fixes.extend(fixes);
            all_exhausted.extend(exhausted);

            if all_fixes.iter().any(|f| f.success) {
                let quick_verify = self.base_verifier.verify_scoped(scope, working_dir).await;
                if quick_verify.passed {
                    info!("Early exit: issues resolved mid-batch");
                    break;
                }
            }
        }

        Ok((all_fixes, all_exhausted))
    }

    pub(super) async fn fix_issues_isolated(
        &self,
        issues: &[Issue],
        state: &mut ConvergenceState,
        mission: Option<&Mission>,
        working_dir: &Path,
    ) -> Result<(Vec<FixAttempt>, Vec<Issue>)> {
        let sorted_issues = self.sort_by_priority(issues);
        let batch: Vec<_> = sorted_issues
            .iter()
            .take(self.config.max_issues_per_batch)
            .copied()
            .collect();
        self.fix_batch(&batch, state, mission, working_dir).await
    }

    async fn fix_batch(
        &self,
        issues: &[&Issue],
        state: &mut ConvergenceState,
        mission: Option<&Mission>,
        working_dir: &Path,
    ) -> Result<(Vec<FixAttempt>, Vec<Issue>)> {
        let mut fixes = Vec::new();
        let mut exhausted = Vec::new();
        let mission_id = mission.map(|m| m.id.as_str());

        for issue in issues {
            let attempts = state.global_fix_attempts(issue);
            if attempts >= self.config.max_fix_attempts_per_issue {
                warn!(issue_id = %issue.id, attempts, "Issue exhausted: max attempts");
                exhausted.push((*issue).clone());
                continue;
            }

            // Get strategy with pattern context (pattern context is informational, not gating)
            let (strategy, pattern_ctx) = self.select_strategy(issue, state);

            let strategy = match strategy {
                Some(s) => s,
                None => {
                    warn!(issue_id = %issue.id, "Issue exhausted: no strategies left");
                    exhausted.push((*issue).clone());
                    continue;
                }
            };

            state.record_fix_attempt(issue, strategy);

            // Search for similar past fixes to include in context
            let similar_fixes_ctx = self.search_similar_fixes(issue).await;

            let prompt = self.build_fix_prompt(&super::FixPromptParams {
                issue,
                mission,
                strategy,
                attempts,
                similar_fixes_ctx: similar_fixes_ctx.as_deref(),
                pattern_ctx: pattern_ctx.as_ref(),
                working_dir,
            });
            let fix_timeout = Duration::from_secs(self.config.fix_timeout_secs);
            let max_output_len = self.config.max_fix_output_length;

            let fix_result =
                tokio::time::timeout(fix_timeout, self.agent.run_prompt(&prompt, working_dir))
                    .await;

            let fix = match fix_result {
                Ok(Ok(output)) => FixAttempt::new(&issue.id, strategy, true)
                    .with_output_limit(output, max_output_len),
                Ok(Err(e)) => FixAttempt::new(&issue.id, strategy, false)
                    .with_output_limit(e.to_string(), max_output_len),
                Err(_) => {
                    warn!(issue_id = %issue.id, timeout_secs = fix_timeout.as_secs(), "Fix attempt timed out");
                    FixAttempt::new(&issue.id, strategy, false).with_output(format!(
                        "Fix timed out after {} seconds",
                        fix_timeout.as_secs()
                    ))
                }
            };

            // Record fix result to search index for future reference
            self.record_fix_result(issue, &fix, mission_id).await;

            // Record to PatternBank if available
            if let Some(ref bank) = self.pattern_bank
                && let Some(ref ctx) = pattern_ctx
                && let Err(e) = bank
                    .record_application(&ctx.pattern_id, &issue.id, fix.success)
                    .await
            {
                warn!(
                    pattern_id = %ctx.pattern_id,
                    issue_id = %issue.id,
                    error = %e,
                    "Failed to record pattern application"
                );
            }

            fixes.push(fix);
        }

        Ok((fixes, exhausted))
    }

    /// Select fix strategy with pattern context for LLM.
    ///
    /// Returns pattern information as context (not hard-gated by confidence threshold).
    /// LLM decides how to weigh the pattern suggestion based on confidence level.
    pub(super) fn select_strategy(
        &self,
        issue: &Issue,
        state: &ConvergenceState,
    ) -> (Option<FixStrategy>, Option<PatternContext>) {
        // Try PatternBank first if available
        if let Some(ref bank) = self.pattern_bank {
            let signature = ErrorSignature::from_issue(issue);
            let matches = bank.retrieve(&signature);

            // Find best matching pattern that hasn't been tried
            for pmatch in &matches {
                let strategy = pmatch.pattern.strategy;
                let confidence = pmatch.pattern.confidence;
                let success_rate = pmatch.pattern.success_rate();

                let tried = state
                    .issue_registry
                    .entry(&issue.signature())
                    .map(|e| e.tried_strategies.contains(&strategy))
                    .unwrap_or(false);

                if tried {
                    continue;
                }

                let context = PatternContext {
                    pattern_id: pmatch.pattern.id.clone(),
                    strategy,
                    confidence,
                    success_rate,
                    total_uses: pmatch.pattern.total_uses(),
                };

                debug!(
                    issue_id = %issue.id,
                    strategy = ?strategy,
                    confidence,
                    success_rate,
                    "Pattern suggestion (LLM will evaluate based on confidence)"
                );

                return (Some(strategy), Some(context));
            }
        }

        // Fall back to convergence state strategy selection
        (state.next_strategy(issue), None)
    }

    /// Sort issues by priority for fix ordering.
    pub(super) fn sort_by_priority<'a>(&self, issues: &'a [Issue]) -> Vec<&'a Issue> {
        let mut sorted: Vec<_> = issues.iter().collect();
        sorted.sort_by(|a, b| {
            let cat_order = |c: &IssueCategory| match c {
                IssueCategory::BuildError => 0,
                IssueCategory::TypeCheckError => 1,
                IssueCategory::TestFailure => 2,
                IssueCategory::LintError => 3,
                IssueCategory::RuntimeError => 4,
                IssueCategory::SecurityIssue => 5,
                IssueCategory::MissingFile => 6,
                IssueCategory::LogicError => 7,
                IssueCategory::PerformanceIssue => 8,
                IssueCategory::StyleViolation => 9,
                IssueCategory::Custom(_) => 10,
                IssueCategory::Other => 11,
            };

            let sev_order = |s: &Severity| match s {
                Severity::Critical => 0,
                Severity::Error => 1,
                Severity::Warning => 2,
                Severity::Info => 3,
            };

            let conf_order = |c: f32| ((1.0 - c) * 100.0) as i32;

            (
                cat_order(&a.category),
                sev_order(&a.severity),
                conf_order(a.confidence),
            )
                .cmp(&(
                    cat_order(&b.category),
                    sev_order(&b.severity),
                    conf_order(b.confidence),
                ))
        });
        sorted
    }

    pub(super) async fn record_fix_result(
        &self,
        issue: &Issue,
        fix: &FixAttempt,
        mission_id: Option<&str>,
    ) {
        if !self.search_config.enabled {
            return;
        }

        let record_id = format!("fix-{}", uuid::Uuid::new_v4());

        let summary = format!(
            "{:?} fix {} for {:?}: {}",
            fix.strategy,
            if fix.success { "succeeded" } else { "failed" },
            issue.category,
            truncate_str(&issue.message, 80),
        );

        let learnings = if fix.success {
            vec![format!(
                "{:?} strategy worked for {:?}",
                fix.strategy, issue.category
            )]
        } else {
            vec![format!("{:?} strategy failed", fix.strategy)]
        };

        let mut detail = FixDetail::new(
            &record_id,
            &issue.message,
            format!("Applied {:?}", fix.strategy),
        )
        .with_analysis(if fix.success {
            format!(
                "Fix succeeded. Output: {}",
                truncate_str(&fix.output_summary, 300)
            )
        } else {
            format!(
                "Fix failed. Output: {}",
                truncate_str(&fix.output_summary, 300)
            )
        });

        for file in &fix.files_modified {
            detail = detail.with_file(file);
        }

        let detail_tokens = detail.estimate_tokens();
        let detail_filename = format!("details/{}.yaml", record_id);

        let mut record = FixRecord::new(
            record_id.clone(),
            issue.category.clone(),
            fix.strategy,
            fix.success,
            summary,
        )
        .with_files(fix.files_modified.clone())
        .with_learnings(learnings)
        .with_detail(&detail_filename, detail_tokens);

        if let Some(mid) = mission_id {
            record = record.with_mission(mid);
        }

        let index_dir = self.index_dir.clone();
        let detail_path = index_dir.join(&detail_filename);
        let jsonl_path = index_dir.join("fixes.jsonl");

        if let Err(e) = detail.save(&detail_path).await {
            warn!(error = %e, record_id = %record_id, "Failed to save fix detail");
            return;
        }

        match record.to_jsonl() {
            Ok(line) => {
                use tokio::fs::OpenOptions;
                use tokio::io::AsyncWriteExt;

                if let Some(parent) = jsonl_path.parent()
                    && let Err(e) = tokio::fs::create_dir_all(parent).await
                {
                    warn!(error = %e, path = %parent.display(), "Failed to create fix index directory");
                    return;
                }

                match OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&jsonl_path)
                    .await
                {
                    Ok(mut file) => {
                        if let Err(e) = file.write_all(format!("{}\n", line).as_bytes()).await {
                            warn!(error = %e, "Failed to write JSONL");
                        } else {
                            debug!(record_id = %record_id, "Recorded fix");
                        }
                    }
                    Err(e) => warn!(error = %e, "Failed to open JSONL"),
                }
            }
            Err(e) => warn!(error = %e, "Failed to serialize record"),
        }
    }

    pub(super) async fn search_similar_fixes(&self, issue: &Issue) -> Option<String> {
        if !self.search_config.enabled || !self.search_config.include_previous_attempts {
            return None;
        }

        let query = SearchQuery::new()
            .with_category(issue.category.clone())
            .with_limit(10)
            .with_timeout(self.search_config.timeout_secs);

        let search_result = match self.fix_searcher.search(&query).await {
            Ok(r) if !r.is_empty() => r,
            _ => return None,
        };

        let fix_loader = FixLoader::new(&self.index_dir);
        let budget = self.search_config.max_related_code_tokens;

        let mut records: Vec<_> = search_result.records.into_iter().collect();
        records.sort_by(|a, b| b.success.cmp(&a.success));

        let top_records: Vec<_> = records
            .into_iter()
            .take(self.search_config.max_fix_records)
            .collect();
        let loaded = fix_loader.load_with_budget(&top_records, budget, 3).await;

        if loaded.is_empty() {
            return None;
        }

        let mut context = String::from("\n## Previous Similar Fixes\n\n");

        for (i, loaded_fix) in loaded.iter().enumerate() {
            context.push_str(&format!("{}. ", i + 1));
            context.push_str(&loaded_fix.to_prompt_context());
            context.push('\n');
        }

        if context.len() > 50 {
            Some(context)
        } else {
            None
        }
    }
}
