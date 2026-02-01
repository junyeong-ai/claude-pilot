use std::path::Path;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::history::GlobalIssueEntry;
use crate::agent::TaskAgent;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedAction {
    ContinueRetry,
    EscalateToHuman,
    SkipAndContinue,
    AbortMission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationSource {
    Signal,
    Llm,
    Fallback,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SolvabilityResult {
    pub index: usize,
    pub solvable: bool,
    pub action: SuggestedAction,
    pub reason: String,
    #[serde(skip)]
    pub source: Option<ClassificationSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct SolvabilityResponse {
    analyses: Vec<SolvabilityResult>,
}

pub struct SolvabilityAnalyzer {
    agent: Arc<TaskAgent>,
    working_dir: std::path::PathBuf,
}

impl SolvabilityAnalyzer {
    pub fn new(agent: Arc<TaskAgent>, working_dir: &Path) -> Self {
        Self {
            agent,
            working_dir: working_dir.to_path_buf(),
        }
    }

    pub async fn analyze(&self, issues: &[&GlobalIssueEntry]) -> Result<Vec<SolvabilityResult>> {
        if issues.is_empty() {
            return Ok(vec![]);
        }

        debug!(count = issues.len(), "Analyzing issue solvability");

        let mut results = Vec::with_capacity(issues.len());
        let mut ambiguous_indices = Vec::new();

        // Phase 1: Signal-based classification for deterministic cases
        for (i, issue) in issues.iter().enumerate() {
            if let Some(result) = self.classify_by_signature(i, issue) {
                results.push((i, result));
            } else {
                ambiguous_indices.push(i);
            }
        }

        let signal_classified = results.len();
        debug!(
            signal_classified,
            ambiguous = ambiguous_indices.len(),
            "Signal-based classification complete"
        );

        // Phase 2: LLM for ambiguous cases only
        if !ambiguous_indices.is_empty() {
            let ambiguous_issues: Vec<_> = ambiguous_indices.iter().map(|&i| issues[i]).collect();

            let llm_results = self
                .analyze_with_llm(&ambiguous_issues, &ambiguous_indices)
                .await;
            results.extend(llm_results.into_iter().map(|r| (r.index, r)));
        }

        // Sort by original index and extract results
        results.sort_by_key(|(i, _)| *i);
        let final_results: Vec<_> = results.into_iter().map(|(_, r)| r).collect();

        info!(
            signal_classified,
            llm_classified = final_results.len() - signal_classified,
            "Solvability analysis complete"
        );

        Ok(final_results)
    }

    /// Classify using deterministic signals only.
    /// Returns None for ambiguous cases that need LLM judgment.
    ///
    /// IMPORTANT: This function uses ONLY universal signals that work across:
    /// - All programming languages
    /// - All locales and error message languages
    /// - All platforms (Windows, macOS, Linux)
    ///
    /// English text patterns are NOT used because:
    /// - Error messages vary by locale
    /// - Non-English toolchains exist
    /// - Even English messages vary across tool versions
    fn classify_by_signature(
        &self,
        index: usize,
        issue: &GlobalIssueEntry,
    ) -> Option<SolvabilityResult> {
        // Only use HTTP status codes as universal signals
        // These are standardized and language-agnostic
        let sig = &issue.signature;

        // HTTP 401/403: Authentication issues (universal HTTP standard)
        if sig.contains("401") || sig.contains("403") {
            return Some(SolvabilityResult {
                index,
                solvable: false,
                action: SuggestedAction::EscalateToHuman,
                reason: "HTTP 401/403: Authentication/authorization issue".into(),
                source: Some(ClassificationSource::Signal),
            });
        }

        // HTTP 404: Resource not found (universal HTTP standard)
        if sig.contains("404") {
            return Some(SolvabilityResult {
                index,
                solvable: false,
                action: SuggestedAction::SkipAndContinue,
                reason: "HTTP 404: Resource not found".into(),
                source: Some(ClassificationSource::Signal),
            });
        }

        // High attempt count with low resolution rate → statistical signal
        // This is language-agnostic (based on numbers, not text)
        if issue.fix_attempts >= 5 && issue.total_occurrences >= 3 {
            return Some(SolvabilityResult {
                index,
                solvable: false,
                action: SuggestedAction::EscalateToHuman,
                reason: format!(
                    "Persistent: {} attempts, {} occurrences (statistical signal)",
                    issue.fix_attempts, issue.total_occurrences
                ),
                source: Some(ClassificationSource::Signal),
            });
        }

        // All other cases: LLM has better semantic understanding
        // Categories like BuildError, TestFailure etc. provide context to LLM
        None
    }

    async fn analyze_with_llm(
        &self,
        issues: &[&GlobalIssueEntry],
        original_indices: &[usize],
    ) -> Vec<SolvabilityResult> {
        let prompt = self.build_prompt(issues);

        match self
            .agent
            .run_prompt_structured::<SolvabilityResponse>(&prompt, &self.working_dir)
            .await
        {
            Ok(response) => response
                .analyses
                .into_iter()
                .enumerate()
                .map(|(i, mut r)| {
                    r.index = original_indices.get(i).copied().unwrap_or(i);
                    r.source = Some(ClassificationSource::Llm);
                    r
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "LLM solvability analysis failed");
                // Fallback: mark as unknown, don't assume solvable
                original_indices
                    .iter()
                    .map(|&i| SolvabilityResult {
                        index: i,
                        solvable: true, // Conservative: allow retry
                        action: SuggestedAction::ContinueRetry,
                        reason: "LLM analysis unavailable, allowing retry".into(),
                        source: Some(ClassificationSource::Fallback),
                    })
                    .collect()
            }
        }
    }

    fn build_prompt(&self, issues: &[&GlobalIssueEntry]) -> String {
        let issues_text = issues
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // Include category characteristics for better LLM context
                let chars = e.category.characteristics();
                let char_info = format!(
                    "characteristics: {} determinism, {} auto-fix likelihood",
                    match chars.determinism {
                        crate::verification::issue::DeterminismLevel::High => "high",
                        crate::verification::issue::DeterminismLevel::Medium => "medium",
                        crate::verification::issue::DeterminismLevel::Low => "low",
                        crate::verification::issue::DeterminismLevel::Subjective => "subjective",
                    },
                    match chars.auto_fixable {
                        crate::verification::issue::AutoFixability::Likely => "likely",
                        crate::verification::issue::AutoFixability::Possible => "possible",
                        crate::verification::issue::AutoFixability::Unlikely => "unlikely",
                    }
                );

                format!(
                    r"{}. Category: {:?}
   Signature: {}
   Occurrences: {} | Fix attempts: {} | Strategies tried: {:?}
   {}",
                    i,
                    e.category,
                    e.signature,
                    e.total_occurrences,
                    e.fix_attempts,
                    e.tried_strategies,
                    char_info
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        format!(
            r"Analyze if these issues can be resolved by an AI coding agent.

## Issues
{issues_text}

## Decision Criteria
For each issue, consider:
1. **Code vs External**: Is this a code issue (syntax, logic, types) or external (auth, network, missing tools)?
2. **Determinism**: Deterministic issues (build errors) are usually solvable; subjective ones (logic) may need human input
3. **Auto-fix likelihood**: Based on category and past attempts
4. **Persistence**: Many failed attempts suggest fundamental blockers

## Response Format
For each issue provide:
- solvable: true if AI agent can fix, false otherwise
- action: continue_retry | escalate_to_human | skip_and_continue | abort_mission
- reason: Explain your reasoning based on the signals above"
        )
    }
}

#[derive(Debug, Clone)]
pub enum EarlyTerminationReason {
    AllStrategiesExhausted,
    AllUnsolvable,
    CriticalUnsolvable,
    Oscillation,
}
