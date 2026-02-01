use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{debug, info, warn};

use super::history::{
    ConvergenceState, DriftAnalysis, FixAttempt, FixStrategy, OscillationContext, ProgressTrend,
    VerificationHistory, VerificationRound,
};
use super::issue::{Issue, IssueCategory, IssueExtractor, IssueSeverity};
use super::pattern_bank::{ErrorSignature, PatternBank};
use super::responses::AiReviewResponse;
use super::semantic::SemanticValidator;
use super::verifier::Verifier;
use super::{Verification, VerificationScope};
use crate::agent::{LlmExecutor, TaskAgent};
use crate::config::{ConvergentVerificationConfig, SearchConfig};
use crate::error::Result;
use crate::mission::Mission;
use crate::search::{FixDetail, FixLoader, FixRecord, FixSearcher, SearchQuery};
use crate::utils::truncate_str;

#[derive(Debug, Clone)]
pub struct ConvergenceResult {
    pub converged: bool,
    pub total_rounds: u32,
    pub history: VerificationHistory,
    pub state: ConvergenceState,
    pub final_issues: Vec<Issue>,
    pub persistent_issues: Vec<Issue>,
    pub termination_reason: Option<super::solvability::EarlyTerminationReason>,
}

/// Parameters for building a fix prompt.
struct FixPromptParams<'a> {
    issue: &'a Issue,
    mission: Option<&'a Mission>,
    strategy: FixStrategy,
    attempts: u32,
    similar_fixes_ctx: Option<&'a str>,
    pattern_ctx: Option<&'a PatternContext>,
    working_dir: &'a Path,
}

/// Pattern context for LLM decision-making.
///
/// Contains pattern information with confidence scores so LLM can assess utility.
/// Not a hard gate - LLM sees all relevant patterns and decides based on context.
#[derive(Debug, Clone)]
pub struct PatternContext {
    pub pattern_id: String,
    pub strategy: FixStrategy,
    pub confidence: f32,
    pub success_rate: f32,
    pub total_uses: u32,
}

impl PatternContext {
    /// Format for LLM context.
    ///
    /// Provides confidence information so LLM can weigh the suggestion appropriately.
    /// Lower confidence patterns are still included - LLM judges their utility.
    pub fn format_for_llm(&self) -> String {
        let confidence_level = if self.confidence >= 0.8 {
            "HIGH"
        } else if self.confidence >= 0.6 {
            "MODERATE"
        } else if self.confidence >= 0.4 {
            "LOW"
        } else {
            "EXPERIMENTAL"
        };

        format!(
            r"**Historical Pattern** (confidence: {} - {:.0}%)
- Suggested strategy: {:?}
- Success rate: {:.0}% ({} applications)
Note: Use this as guidance. Your judgment on the specific context takes precedence.",
            confidence_level,
            self.confidence * 100.0,
            self.strategy,
            self.success_rate * 100.0,
            self.total_uses,
        )
    }
}

impl ConvergenceResult {
    pub fn converged(history: VerificationHistory, state: ConvergenceState) -> Self {
        Self {
            converged: true,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues: Vec::new(),
            termination_reason: None,
        }
    }

    pub fn not_converged(history: VerificationHistory, state: ConvergenceState) -> Self {
        let persistent_issues = Self::extract_persistent_issues(&state);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: None,
        }
    }

    pub fn early_terminated(
        history: VerificationHistory,
        state: ConvergenceState,
        reason: super::solvability::EarlyTerminationReason,
    ) -> Self {
        let persistent_issues = Self::extract_persistent_issues(&state);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: Some(reason),
        }
    }

    pub fn oscillating(history: VerificationHistory, state: ConvergenceState) -> Self {
        let persistent_issues = Self::extract_persistent_issues(&state);
        Self {
            converged: false,
            total_rounds: history.total_rounds_processed(),
            history,
            state,
            final_issues: Vec::new(),
            persistent_issues,
            termination_reason: Some(super::solvability::EarlyTerminationReason::Oscillation),
        }
    }

    fn extract_persistent_issues(state: &ConvergenceState) -> Vec<Issue> {
        const PERSISTENT_ISSUE_CONFIDENCE: f32 = 0.90;
        state
            .issue_registry
            .persistent_issues(super::history::GlobalIssueRegistry::DEFAULT_PERSISTENT_THRESHOLD)
            .iter()
            .map(|entry| {
                Issue::new(
                    entry.category.clone(),
                    IssueSeverity::Error,
                    format!(
                        "{} (seen {} times)",
                        entry.signature, entry.total_occurrences
                    ),
                    PERSISTENT_ISSUE_CONFIDENCE,
                )
            })
            .collect()
    }
}

/// Structured response for oscillation termination decision.
/// Uses boolean instead of string matching to avoid English keyword parsing.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct OscillationDecision {
    /// true = terminate (stuck), false = continue (making progress)
    pub should_terminate: bool,
    /// Brief explanation for the decision
    #[serde(default)]
    pub reasoning: Option<String>,
}

pub struct ConvergentVerifier {
    config: ConvergentVerificationConfig,
    search_config: SearchConfig,
    base_verifier: Verifier,
    agent: Arc<TaskAgent>,
    index_dir: PathBuf,
    fix_searcher: FixSearcher,
    semantic_validator: SemanticValidator,
    issue_extractor: IssueExtractor,
    solvability: super::solvability::SolvabilityAnalyzer,
    pattern_bank: Option<Arc<PatternBank>>,
}

/// AI review context for convergent verification loop.
struct ReviewContext<'a> {
    mission: Option<&'a Mission>,
    description: &'a str,
    files_created: &'a [String],
    files_modified: &'a [String],
    diff: Option<&'a str>,
}

impl<'a> ReviewContext<'a> {
    fn from_mission(
        mission: &'a Mission,
        files_created: &'a [String],
        files_modified: &'a [String],
        diff: Option<&'a str>,
    ) -> Self {
        Self {
            mission: Some(mission),
            description: &mission.description,
            files_created,
            files_modified,
            diff,
        }
    }

    fn without_mission(
        description: &'a str,
        files_created: &'a [String],
        files_modified: &'a [String],
        diff: Option<&'a str>,
    ) -> Self {
        Self {
            mission: None,
            description,
            files_created,
            files_modified,
            diff,
        }
    }
}

impl ConvergentVerifier {
    pub fn new(
        config: ConvergentVerificationConfig,
        search_config: SearchConfig,
        base_verifier: Verifier,
        agent: Arc<TaskAgent>,
        index_dir: PathBuf,
        working_dir: &Path,
    ) -> Self {
        let fix_searcher = FixSearcher::new(&index_dir);
        let semantic_validator = SemanticValidator::new(working_dir, search_config.timeout_secs);
        let issue_extractor = IssueExtractor::new(Arc::clone(&agent), working_dir);
        let solvability =
            super::solvability::SolvabilityAnalyzer::new(Arc::clone(&agent), working_dir);

        Self {
            config,
            search_config,
            base_verifier,
            agent,
            index_dir,
            fix_searcher,
            semantic_validator,
            issue_extractor,
            solvability,
            pattern_bank: None,
        }
    }

    pub fn with_pattern_bank(mut self, pattern_bank: Arc<PatternBank>) -> Self {
        self.pattern_bank = Some(pattern_bank);
        self
    }

    pub fn new_without_search(
        config: ConvergentVerificationConfig,
        base_verifier: Verifier,
        agent: Arc<TaskAgent>,
        working_dir: &Path,
    ) -> Self {
        let temp_dir = std::env::temp_dir().join("claude-pilot-search");
        let fix_searcher = FixSearcher::new(&temp_dir);
        let semantic_validator = SemanticValidator::new(working_dir, 10);
        let issue_extractor = IssueExtractor::new(Arc::clone(&agent), working_dir);
        let solvability =
            super::solvability::SolvabilityAnalyzer::new(Arc::clone(&agent), working_dir);

        Self {
            config,
            search_config: SearchConfig {
                enabled: false,
                ..Default::default()
            },
            base_verifier,
            agent,
            index_dir: temp_dir,
            fix_searcher,
            semantic_validator,
            issue_extractor,
            solvability,
            pattern_bank: None,
        }
    }

    /// Primary verification method for mission-based execution.
    pub async fn verify_until_convergent_with_mission_changes(
        &self,
        mission: &Mission,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let review_ctx = ReviewContext::from_mission(mission, files_created, files_modified, diff);
        self.run_convergent_loop_core(
            &mission.id,
            review_ctx,
            VerificationScope::Full,
            working_dir,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn verify_until_convergent_task_scoped(
        &self,
        mission: &Mission,
        test_patterns: Vec<String>,
        module: Option<String>,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let review_ctx = ReviewContext::from_mission(mission, files_created, files_modified, diff);
        self.run_convergent_loop_core(
            &mission.id,
            review_ctx,
            VerificationScope::for_task(test_patterns, module),
            working_dir,
            None,
        )
        .await
    }

    pub async fn verify_until_convergent_with_changes(
        &self,
        description: &str,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let id = format!("direct-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let review_ctx =
            ReviewContext::without_mission(description, files_created, files_modified, diff);
        self.run_convergent_loop_core(&id, review_ctx, VerificationScope::Full, working_dir, None)
            .await
    }

    pub async fn verify_until_convergent_recovery(
        &self,
        mission: &Mission,
        max_rounds_override: Option<u32>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let id = format!("recovery-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let review_ctx = ReviewContext::without_mission(&mission.description, &[], &[], None);
        self.run_convergent_loop_core(
            &id,
            review_ctx,
            VerificationScope::Full,
            working_dir,
            max_rounds_override,
        )
        .await
    }

    async fn run_convergent_loop_core(
        &self,
        id: &str,
        review_context: ReviewContext<'_>,
        scope: VerificationScope,
        working_dir: &Path,
        max_rounds_override: Option<u32>,
    ) -> Result<ConvergenceResult> {
        let base_max_rounds = max_rounds_override.unwrap_or(self.config.max_rounds);
        let mut effective_max_rounds = base_max_rounds;
        let mut extensions_granted: u32 = 0;
        let mut history = VerificationHistory::with_capacity(id, self.config.max_history_rounds);
        let mut state = ConvergenceState::from_config(id, &self.config);

        let total_timeout = Duration::from_secs(self.config.total_timeout_secs);
        let start_time = Instant::now();

        let mut round: u32 = 0;
        while round < effective_max_rounds {
            round += 1;
            if start_time.elapsed() > total_timeout {
                warn!(
                    id,
                    elapsed_secs = start_time.elapsed().as_secs(),
                    timeout_secs = total_timeout.as_secs(),
                    "Convergent verification total timeout exceeded"
                );
                history.mark_completed();
                return Ok(ConvergenceResult::not_converged(history, state.clone()));
            }

            info!(round, id, "Verification round");

            let (verification, duration_ms) = self.run_verification(&scope, working_dir).await;
            let mut issues = self.extract_issues(&verification).await;

            // Semantic validation (LSP diagnostics)
            let all_files: Vec<String> = review_context
                .files_created
                .iter()
                .chain(review_context.files_modified.iter())
                .cloned()
                .collect();
            let semantic_result = self.semantic_validator.validate_files(&all_files).await;
            if semantic_result.has_issues() {
                issues.extend(semantic_result.diagnostic_issues.clone());
            }

            // Build/test status determines if AI review is needed
            let build_test_clean = issues.is_empty() && verification.passed;

            // AI review runs only when build/test passes (cost optimization)
            // When build/test fails, those issues take priority over style issues
            if self.config.include_ai_review && build_test_clean {
                let adversarial = state.consecutive_clean > 0;
                let first_pass_ctx = if adversarial {
                    state.first_pass_context().map(|s| s.to_string())
                } else {
                    None
                };

                let semantic_context = semantic_result.to_review_context();
                let file_changes = (review_context.files_created, review_context.files_modified);
                let (ai_issues, raw_output) = self
                    .run_ai_review(
                        review_context.description,
                        Some(file_changes),
                        review_context.diff,
                        working_dir,
                        adversarial,
                        first_pass_ctx.as_deref(),
                        Some(&semantic_context),
                    )
                    .await?;

                if !adversarial && ai_issues.is_empty() {
                    state.set_first_pass_context(raw_output);
                }

                issues.extend(ai_issues);
            }

            let is_clean = issues.is_empty() && verification.passed;
            history.add_round(
                VerificationRound::new(round, issues.clone()).with_duration(duration_ms),
            );
            state.record_round(is_clean, issues.clone());

            if let Some(drift) = state.last_drift() {
                self.log_drift(round, drift);
            }

            if state.has_consistent_regression() {
                warn!(
                    rounds = state.total_rounds,
                    "Consistent regression detected"
                );
            }

            // Check for oscillation (fix-break-fix cycle)
            let oscillation_threshold = self.config.oscillation_threshold;
            if state.is_oscillating(oscillation_threshold) {
                let context = state.get_oscillation_context();

                // LLM evaluates if this is true stuck state or expected iteration
                if self
                    .should_terminate_oscillation(&context, working_dir)
                    .await
                {
                    warn!(
                        rounds = state.total_rounds,
                        oscillation_count = context.oscillation_count,
                        trend = ?context.progress_trend,
                        "Oscillation confirmed by LLM - verification not converging"
                    );
                    history.mark_completed();
                    return Ok(ConvergenceResult::oscillating(history, state));
                }
                info!(
                    rounds = state.total_rounds,
                    trend = ?context.progress_trend,
                    "LLM determined oscillation is expected iteration - continuing"
                );
            }

            if is_clean {
                info!(
                    consecutive_clean = state.consecutive_clean,
                    required = self.config.required_clean_rounds,
                    "Clean"
                );
                if state.is_converged {
                    info!("Converged");
                    history.mark_completed();
                    return Ok(ConvergenceResult::converged(history, state));
                }
            } else if self.config.auto_fix_enabled {
                let mission = review_context.mission;
                let fix_result = self
                    .fix_issues_batched(&issues, &mut state, mission, &scope, working_dir)
                    .await;

                let (fixes, exhausted) = match fix_result {
                    Ok(result) => result,
                    Err(e) => {
                        warn!(error = %e, round, "Fix attempt failed");

                        if Self::is_context_overflow_error(&e) {
                            info!("Retrying with isolated session");
                            match self
                                .fix_issues_isolated(&issues, &mut state, mission, working_dir)
                                .await
                            {
                                Ok(r) => r,
                                Err(e2) => {
                                    warn!(error = %e2, "Isolated fix also failed, continuing");
                                    (vec![], vec![])
                                }
                            }
                        } else {
                            (vec![], vec![])
                        }
                    }
                };

                history.record_fixes(round, &fixes);

                // Log exhausted issues as signal but don't hard terminate.
                // LLM-based solvability analysis decides if truly unsolvable.
                if !exhausted.is_empty() {
                    info!(
                        exhausted_count = exhausted.len(),
                        total_issues = issues.len(),
                        "Issues with exhausted strategies (solvability check will assess)"
                    );
                }

                // Early termination check via LLM-based solvability analysis
                if let Some(reason) = self.check_early_termination(&state, round).await {
                    warn!(round, reason = ?reason, "Early termination triggered");
                    history.mark_completed();
                    return Ok(ConvergenceResult::early_terminated(history, state, reason));
                }
            }

            // Adaptive extension: grant extra rounds when meaningful progress is being made
            // This allows complex tasks to get more attempts without penalizing simple tasks
            if round == effective_max_rounds
                && self.config.allow_adaptive_extension
                && extensions_granted < self.config.max_adaptive_extensions
                && self.should_extend_rounds(&state)
            {
                extensions_granted += 1;
                let extension = self.config.adaptive_extension_rounds;
                effective_max_rounds += extension;
                info!(
                    round,
                    extensions_granted,
                    new_max_rounds = effective_max_rounds,
                    "Adaptive extension: granting {} additional rounds (progress detected)",
                    extension
                );
            }
        }

        warn!(
            round,
            effective_max_rounds,
            extensions_granted,
            "Max rounds exceeded (including {} adaptive extensions)",
            extensions_granted
        );
        history.mark_completed();
        Ok(ConvergenceResult::not_converged(history, state.clone()))
    }

    /// Check if error is a context overflow.
    /// Uses the structured ExecutionError type instead of string matching.
    fn is_context_overflow_error(e: &crate::error::PilotError) -> bool {
        use crate::error::ExecutionError;

        // Prefer structured error type detection
        if let crate::error::PilotError::AgentExecution(msg) = e {
            matches!(
                ExecutionError::from_message(msg),
                ExecutionError::ContextOverflow { .. }
            )
        } else {
            false
        }
    }

    async fn check_early_termination(
        &self,
        state: &ConvergenceState,
        _round: u32,
    ) -> Option<super::solvability::EarlyTerminationReason> {
        use super::solvability::{EarlyTerminationReason, SuggestedAction};

        let threshold = self.config.persistent_issue_threshold;
        let persistent = state.issue_registry.persistent_issues(threshold);

        if persistent.is_empty() {
            return None;
        }

        // Only analyze if issues persist beyond threshold + buffer
        let analyze_candidates: Vec<_> = persistent
            .iter()
            .filter(|p| p.total_occurrences >= threshold + 2)
            .copied()
            .collect();

        if analyze_candidates.is_empty() {
            return None;
        }

        let analyses = self.solvability.analyze(&analyze_candidates).await.ok()?;

        if analyses
            .iter()
            .any(|a| a.action == SuggestedAction::AbortMission)
        {
            return Some(EarlyTerminationReason::CriticalUnsolvable);
        }

        let unsolvable_count = analyses.iter().filter(|a| !a.solvable).count();
        if unsolvable_count == analyze_candidates.len() && unsolvable_count > 0 {
            return Some(EarlyTerminationReason::AllUnsolvable);
        }

        None
    }

    /// Determine if we should grant additional rounds based on observed progress.
    /// This adapts to task complexity by observing results rather than using fixed limits.
    ///
    async fn should_terminate_oscillation(
        &self,
        context: &OscillationContext,
        working_dir: &Path,
    ) -> bool {
        // Fast path: if making clear progress, don't terminate
        if context.progress_trend == ProgressTrend::Improving && context.net_progress > 0 {
            return false;
        }

        // Fast path: if heavily regressing with no progress, terminate
        if context.progress_trend == ProgressTrend::Regressing
            && context.oscillation_count >= self.config.oscillation_threshold + 2
        {
            return true;
        }

        // Ambiguous cases: ask LLM to evaluate with structured response
        let prompt = format!(
            r"Evaluate if this oscillation pattern indicates a stuck state or expected iteration.

## Context
{}

## Decision Criteria
- should_terminate=false if: progress is being made, issues are partially resolving, or complexity justifies iterations
- should_terminate=true if: same issues keep reappearing without progress, fundamentally stuck",
            context.format_for_llm()
        );

        match self
            .agent
            .execute_structured::<OscillationDecision>(&prompt, working_dir)
            .await
        {
            Ok(decision) => {
                debug!(
                    should_terminate = decision.should_terminate,
                    reasoning = ?decision.reasoning,
                    "LLM oscillation evaluation"
                );
                decision.should_terminate
            }
            Err(e) => {
                warn!(error = %e, "LLM oscillation evaluation failed, using threshold-based fallback");
                context.oscillation_count > self.config.oscillation_threshold
            }
        }
    }

    /// Extension is granted when:
    /// 1. There's positive drift (more issues resolved than introduced)
    /// 2. No oscillation detected (not stuck in fix-break-fix cycle)
    /// 3. Issues are actually being resolved (not stagnant)
    fn should_extend_rounds(&self, state: &ConvergenceState) -> bool {
        let (should_extend, net_resolved, net_introduced) =
            state.calculate_extension_eligibility(self.config.oscillation_threshold);

        if should_extend {
            let net_progress = net_resolved - net_introduced;
            info!(
                net_resolved,
                net_introduced, net_progress, "Extension criteria met: positive drift detected"
            );
        } else if state.oscillation_count() >= self.config.oscillation_threshold {
            debug!("No extension: oscillation detected");
        } else if net_resolved == 0 && net_introduced == 0 {
            debug!("No extension: no drift history");
        } else {
            debug!(
                net_resolved,
                net_introduced, "No extension: no net progress"
            );
        }

        should_extend
    }

    async fn run_verification(
        &self,
        scope: &VerificationScope,
        working_dir: &Path,
    ) -> (Verification, u64) {
        let start = Instant::now();
        let verification = self.base_verifier.verify_scoped(scope, working_dir).await;
        (verification, start.elapsed().as_millis() as u64)
    }

    async fn fix_issues_batched(
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

    async fn fix_issues_isolated(
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

            let prompt = self.build_fix_prompt(&FixPromptParams {
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
            {
                let _ = bank
                    .record_application(&ctx.pattern_id, &issue.id, fix.success)
                    .await;
            }

            fixes.push(fix);
        }

        Ok((fixes, exhausted))
    }

    /// Select fix strategy with pattern context for LLM.
    ///
    /// Returns pattern information as context (not hard-gated by confidence threshold).
    /// LLM decides how to weigh the pattern suggestion based on confidence level.
    fn select_strategy(
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

    fn build_fix_prompt(&self, params: &FixPromptParams<'_>) -> String {
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

        // Detect project context: build system + language hints
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
    /// Provides comprehensive context for LLM fix suggestions.
    fn detect_project_context(
        working_dir: &Path,
        affected_files: &[&str],
        primary_file: Option<&str>,
    ) -> String {
        let mut ctx = String::new();

        // Detect build system
        if let Some(build_system) = crate::config::BuildSystem::detect(working_dir) {
            ctx.push_str(&format!("- Build System: {:?}\n", build_system));
        }

        // Detect all languages from affected files
        let mut languages: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for file in affected_files {
            if let Some(lang) = Self::extension_to_language(file) {
                languages.insert(lang);
            }
        }

        // Add primary file's language with detailed hints
        if let Some(primary) = primary_file
            && let Some(hints) = Self::get_language_hints(primary)
        {
            ctx.push_str(&hints);
        }

        // Note if polyglot project
        if languages.len() > 1 {
            let langs: Vec<_> = languages.into_iter().collect();
            ctx.push_str(&format!(
                "- Note: Polyglot project ({}) - ensure cross-language compatibility\n",
                langs.join(", ")
            ));
        }

        ctx
    }

    /// Map file extension to language name.
    fn extension_to_language(file_path: &str) -> Option<&'static str> {
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

    /// Get language-specific fix hints for a file.
    fn get_language_hints(file_path: &str) -> Option<String> {
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

    /// Sort issues by priority for fix ordering.
    ///
    /// Multi-dimensional sort considering:
    /// 1. Category (build errors first - they block everything)
    /// 2. Severity (critical before warnings)
    /// 3. Confidence (high confidence issues first - more reliable diagnosis)
    ///
    /// NOTE: Determinism is intentionally NOT used as a sorting factor.
    /// A subjective but Critical security issue should not be deprioritized
    /// behind a deterministic but Warning-level lint error. LLM can make
    /// nuanced judgments when issues have equal category/severity/confidence.
    fn sort_by_priority<'a>(&self, issues: &'a [Issue]) -> Vec<&'a Issue> {
        use crate::verification::issue::IssueSeverity;

        let mut sorted: Vec<_> = issues.iter().collect();
        sorted.sort_by(|a, b| {
            // Primary: Category order (blocking issues first)
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

            // Secondary: Severity (higher severity first)
            let sev_order = |s: &IssueSeverity| match s {
                IssueSeverity::Critical => 0,
                IssueSeverity::Error => 1,
                IssueSeverity::Warning => 2,
                IssueSeverity::Info => 3,
            };

            // Tertiary: Confidence (higher confidence first)
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

    async fn record_fix_result(&self, issue: &Issue, fix: &FixAttempt, mission_id: Option<&str>) {
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

        tokio::spawn(async move {
            if let Err(e) = detail.save(&detail_path).await {
                warn!(error = %e, record_id = %record_id, "Failed to save fix detail");
                return;
            }

            match record.to_jsonl() {
                Ok(line) => {
                    use tokio::fs::OpenOptions;
                    use tokio::io::AsyncWriteExt;

                    if let Some(parent) = jsonl_path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
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
        });
    }

    async fn search_similar_fixes(&self, issue: &Issue) -> Option<String> {
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

    /// Extract issues from verification using LLM-based IssueExtractor.
    /// Provides universal language support for all build systems.
    async fn extract_issues(&self, verification: &Verification) -> Vec<Issue> {
        const CHECK_FAILURE_CONFIDENCE: f32 = 0.95;
        const FALLBACK_ISSUE_CONFIDENCE: f32 = 0.60;
        const INCOMPLETE_CHECK_CONFIDENCE: f32 = 0.95;

        let mut issues: Vec<Issue> = Vec::new();

        for check_result in verification.failed_checks() {
            let output = check_result.output.as_deref().unwrap_or("");
            if output.trim().is_empty() {
                issues.push(Issue::new(
                    IssueCategory::from(check_result.check),
                    IssueSeverity::Error,
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
                    IssueSeverity::Error,
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
                IssueSeverity::Error,
                "Incomplete verification: no checks were performed. Task may not have been implemented.",
                INCOMPLETE_CHECK_CONFIDENCE,
            ));
        }

        issues
    }

    /// Run AI review with perspective differentiation for adversarial 2-pass verification.
    /// - Standard review (adversarial=false): Check if code meets requirements
    /// - Adversarial review (adversarial=true): Actively seek hidden flaws with 1st pass context
    ///
    /// Uses run_prompt_review which provides Read/Glob/Grep access for actual code analysis.
    /// When file_changes and diff are provided, the AI knows exactly what to review.
    /// Returns (parsed_issues, context_for_next_pass) to enable context passing between passes.
    #[allow(clippy::too_many_arguments)]
    async fn run_ai_review(
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

        // Build diff section - this shows EXACTLY what changed
        let diff_section = if let Some(d) = diff {
            // Truncate very large diffs to avoid context overflow
            // Uses configurable truncation length instead of hardcoded 10000
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

        // Provide detected language context with confidence to help LLM
        // focus on relevant review dimensions without making assumptions.
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

    fn build_issues(&self, response: &AiReviewResponse) -> Vec<Issue> {
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

        // Include checklist dimension failures with their detailed notes.
        // The notes field contains LLM's reasoning for why the dimension failed,
        // which is critical context for fix strategies.
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
                IssueSeverity::Warning,
                message,
                CHECKLIST_FAILURE_CONFIDENCE,
            ));
        }

        issues
    }

    fn log_drift(&self, round: u32, drift: &DriftAnalysis) {
        if drift.new_issue_count == 0 && drift.resolved_issue_count == 0 {
            return;
        }

        let (level, msg) = if drift.has_regression {
            ("warn", "Regression: more issues introduced than resolved")
        } else if drift.drift_score > 0 {
            ("info", "Progress: net issues reduced")
        } else {
            ("warn", "Negative drift: net issues increased")
        };

        if level == "warn" {
            warn!(
                round,
                new = drift.new_issue_count,
                resolved = drift.resolved_issue_count,
                score = drift.drift_score,
                "{}",
                msg
            );
        } else {
            info!(
                round,
                new = drift.new_issue_count,
                resolved = drift.resolved_issue_count,
                score = drift.drift_score,
                "{}",
                msg
            );
        }
    }
}

/// Language detection result with confidence for LLM context.
/// Single language detection with confidence level.
#[derive(Clone)]
struct DetectedLanguage {
    name: &'static str,
    confidence: LanguageConfidence,
}

/// All detected languages in a project.
struct LanguageDetection {
    languages: Vec<DetectedLanguage>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LanguageConfidence {
    High,   // Build system marker present
    Medium, // Multiple source files
    Low,    // Single file or ambiguous
}

impl LanguageDetection {
    fn format_for_llm(&self) -> String {
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
/// Uses multiple signals: build system markers, file extensions, and frequency.
fn detect_primary_language(
    file_changes: Option<(&[String], &[String])>,
) -> Option<LanguageDetection> {
    let (created, modified) = file_changes?;
    let all_files: Vec<_> = created.iter().chain(modified.iter()).collect();

    if all_files.is_empty() {
        return None;
    }

    // Build system markers provide HIGH confidence (weight: 3)
    // These are definitive signals about project language
    let build_markers = [
        ("Cargo.toml", "Rust", 3),
        ("Cargo.lock", "Rust", 3),
        ("package.json", "JavaScript", 2), // Could be TS, so medium weight
        ("tsconfig.json", "TypeScript", 3),
        ("go.mod", "Go", 3),
        ("go.sum", "Go", 3),
        ("pom.xml", "Java", 3),
        ("build.gradle", "Java", 2), // Could be Kotlin
        ("build.gradle.kts", "Kotlin", 3),
        ("requirements.txt", "Python", 2),
        ("pyproject.toml", "Python", 3),
        ("setup.py", "Python", 3),
        ("Gemfile", "Ruby", 3),
        ("mix.exs", "Elixir", 3),
        ("pubspec.yaml", "Dart", 3),
    ];

    // Count weighted scores per language
    let mut scores: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut has_build_marker = false;

    // Check build markers first
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

    // Extension to language mapping with weights
    // Source files get weight 2, configs/generated files get weight 1
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
        (&["hpp", "hxx", "h"], "C++", 1), // Headers are lower weight
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

    // Score extensions
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

    // Sort by score descending
    let mut sorted: Vec<_> = scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    // Convert scores to DetectedLanguage with confidence levels
    let languages: Vec<DetectedLanguage> = sorted
        .into_iter()
        .filter(|(_, score)| *score >= 2) // Only include languages with meaningful presence
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
