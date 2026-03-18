use std::path::Path;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use super::super::checks::VerificationScope;
use super::super::history::{
    ConvergenceState, DriftAnalysis, OscillationContext, ProgressTrend, VerificationHistory,
    VerificationRound,
};
use super::super::Verification;
use super::types::{ConvergenceResult, OscillationDecision};
use super::{ConvergentVerifier, ReviewContext};
use crate::agent::LlmExecutor;
use crate::error::Result;

impl ConvergentVerifier {
    pub(super) async fn run_convergent_loop_core(
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
                return Ok(ConvergenceResult::not_converged(
                    history,
                    state.clone(),
                    self.config.persistent_issue_confidence,
                ));
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
                let context = state.oscillation_context();

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
                    return Ok(ConvergenceResult::oscillating(
                        history,
                        state,
                        self.config.persistent_issue_confidence,
                    ));
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
                    return Ok(ConvergenceResult::early_terminated(
                        history,
                        state,
                        reason,
                        self.config.persistent_issue_confidence,
                    ));
                }
            }

            // Adaptive extension: grant extra rounds when meaningful progress is being made
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
        Ok(ConvergenceResult::not_converged(history, state.clone(), self.config.persistent_issue_confidence))
    }

    /// Check if error is a context overflow.
    /// Uses the structured ExecutionError type instead of string matching.
    pub(super) fn is_context_overflow_error(e: &crate::error::PilotError) -> bool {
        use crate::error::ExecutionError;

        if let crate::error::PilotError::AgentExecution(msg) = e {
            matches!(
                ExecutionError::from_message(msg),
                ExecutionError::ContextOverflow { .. }
            )
        } else {
            false
        }
    }

    pub(super) async fn check_early_termination(
        &self,
        state: &ConvergenceState,
        _round: u32,
    ) -> Option<super::super::solvability::EarlyTerminationReason> {
        use super::super::solvability::{EarlyTerminationReason, SuggestedAction};

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

    pub(super) async fn should_terminate_oscillation(
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

    /// Determine if we should grant additional rounds based on observed progress.
    pub(super) fn should_extend_rounds(&self, state: &ConvergenceState) -> bool {
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

    pub(super) async fn run_verification(
        &self,
        scope: &VerificationScope,
        working_dir: &Path,
    ) -> (Verification, u64) {
        let start = Instant::now();
        let verification = self.base_verifier.verify_scoped(scope, working_dir).await;
        (verification, start.elapsed().as_millis() as u64)
    }

    pub(super) fn log_drift(&self, round: u32, drift: &DriftAnalysis) {
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
