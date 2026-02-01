//! Verification archive for checkpoint persistence.
//!
//! This module captures verification state for durable execution,
//! enabling recovery without losing learned patterns and fix history.
//!
//! Key components:
//! - `TaskVerificationArchive`: Per-task verification summary
//! - `VerificationArchive`: Mission-wide verification history
//! - `LearnedPattern`: Patterns extracted from verification for future use

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::history::{ConvergenceState, FixStrategy, VerificationHistory};
use super::issue::IssueCategory;

/// Calculate rate as ratio of successful to total, with configurable default for zero denominator.
/// Used consistently across all rate calculations (success_rate, fix_rate, convergence_rate).
#[inline]
fn rate(successful: u32, total: u32, default_on_zero: f32) -> f32 {
    if total == 0 {
        default_on_zero
    } else {
        successful as f32 / total as f32
    }
}

/// Checkpoint-friendly summary of verification for a single task.
/// Extracted from VerificationHistory and ConvergenceState.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskVerificationArchive {
    pub task_id: String,
    pub total_rounds: u32,
    pub outcome: VerificationOutcome,
    pub learned_patterns: Vec<LearnedPattern>,
    pub issue_summary: IssueSummary,
    pub fix_summary: FixSummary,
    pub duration_total_ms: u64,
    pub completed_at: DateTime<Utc>,
}

impl TaskVerificationArchive {
    /// Create a new archive from verification results.
    pub fn from_verification(
        task_id: impl Into<String>,
        history: &VerificationHistory,
        state: &ConvergenceState,
        outcome: VerificationOutcome,
    ) -> Self {
        Self::from_verification_with_threshold(
            task_id,
            history,
            state,
            outcome,
            state.persistent_issue_threshold,
        )
    }

    /// Create archive with custom recurring issue threshold.
    pub fn from_verification_with_threshold(
        task_id: impl Into<String>,
        history: &VerificationHistory,
        state: &ConvergenceState,
        outcome: VerificationOutcome,
        recurring_threshold: u32,
    ) -> Self {
        let learned_patterns = Self::extract_patterns(state, recurring_threshold);
        let issue_summary = Self::build_issue_summary(history, state);
        let fix_summary = Self::build_fix_summary(history);
        let duration_total_ms: u64 = history.rounds.iter().map(|r| r.duration_ms).sum();

        Self {
            task_id: task_id.into(),
            total_rounds: history.total_rounds_processed(),
            outcome,
            learned_patterns,
            issue_summary,
            fix_summary,
            duration_total_ms,
            completed_at: Utc::now(),
        }
    }

    fn extract_patterns(state: &ConvergenceState, recurring_threshold: u32) -> Vec<LearnedPattern> {
        let mut patterns = Vec::new();

        for entry in state
            .issue_registry
            .persistent_issues(super::history::GlobalIssueRegistry::DEFAULT_PERSISTENT_THRESHOLD)
        {
            let pattern_type = if entry.resolved {
                PatternType::SuccessfulFixStrategy
            } else if entry.total_occurrences > recurring_threshold {
                PatternType::RecurringIssue
            } else {
                continue;
            };

            // Calculate confidence based on occurrence count.
            // More occurrences = higher confidence (capped at 0.9).
            let confidence = (0.5 + (entry.total_occurrences as f32 * 0.1)).min(0.9);

            patterns.push(LearnedPattern {
                pattern_type,
                description: format!(
                    "{} issue '{}' seen {} times",
                    entry.category, entry.signature, entry.total_occurrences
                ),
                confidence,
                source_issue_signature: Some(entry.signature.clone()),
                effective_strategy: if entry.resolved {
                    entry.last_strategy
                } else {
                    None
                },
                category: Some(entry.category.clone()),
            });
        }

        patterns
    }

    fn build_issue_summary(
        history: &VerificationHistory,
        state: &ConvergenceState,
    ) -> IssueSummary {
        let mut by_category: HashMap<IssueCategory, u32> = HashMap::new();

        let mut total_found = 0u32;
        for round in &history.rounds {
            for issue in &round.issues_found {
                total_found += 1;
                *by_category.entry(issue.category.clone()).or_default() += 1;
            }
        }

        let total_fixed = history.total_issues_fixed() as u32;
        let persistent_signatures: Vec<String> = state
            .issue_registry
            .persistent_issues(super::history::GlobalIssueRegistry::DEFAULT_PERSISTENT_THRESHOLD)
            .iter()
            .map(|e| e.signature.clone())
            .collect();

        IssueSummary {
            total_found,
            total_fixed,
            by_category: by_category.into_iter().collect(),
            persistent_signatures,
        }
    }

    fn build_fix_summary(history: &VerificationHistory) -> FixSummary {
        let mut strategy_stats: HashMap<FixStrategy, (u32, u32)> = HashMap::new();

        let mut total_attempts = 0u32;
        let mut successful_attempts = 0u32;

        for round in &history.rounds {
            for fix in &round.fix_attempts {
                total_attempts += 1;
                if fix.success {
                    successful_attempts += 1;
                }

                let entry = strategy_stats.entry(fix.strategy).or_insert((0, 0));
                if fix.success {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
            }
        }

        FixSummary {
            total_attempts,
            successful_attempts,
            strategy_effectiveness: strategy_stats
                .into_iter()
                .map(|(s, (succ, fail))| StrategyEffectiveness {
                    strategy: s,
                    successes: succ,
                    failures: fail,
                })
                .collect(),
        }
    }

    /// Get the success rate for this task's verification.
    pub fn success_rate(&self) -> f32 {
        rate(
            self.fix_summary.successful_attempts,
            self.fix_summary.total_attempts,
            0.0,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    /// Successfully passed required_clean_rounds
    Converged,
    /// All fix strategies exhausted for persistent issues
    ExhaustedStrategies,
    /// Hit total_timeout_secs limit
    Timeout,
    /// Hit max_rounds limit without convergence
    MaxRoundsReached,
    /// Escalated to human intervention
    Escalated,
}

impl VerificationOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Converged)
    }

    pub fn from_convergence_state(state: &ConvergenceState) -> Self {
        if state.is_converged {
            Self::Converged
        } else if state.all_strategies_exhausted() {
            Self::ExhaustedStrategies
        } else if state.is_exhausted() {
            Self::MaxRoundsReached
        } else {
            // Default - shouldn't typically reach here during normal flow
            Self::MaxRoundsReached
        }
    }
}

/// A pattern learned during verification that should inform future attempts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedPattern {
    pub pattern_type: PatternType,
    pub description: String,
    pub confidence: f32,
    pub source_issue_signature: Option<String>,
    pub effective_strategy: Option<FixStrategy>,
    pub category: Option<IssueCategory>,
}

impl LearnedPattern {
    pub fn successful_strategy(
        signature: impl Into<String>,
        strategy: FixStrategy,
        category: IssueCategory,
    ) -> Self {
        Self {
            pattern_type: PatternType::SuccessfulFixStrategy,
            description: format!("{} works for {} issues", strategy, category),
            confidence: 0.8,
            source_issue_signature: Some(signature.into()),
            effective_strategy: Some(strategy),
            category: Some(category),
        }
    }

    pub fn failed_strategy(
        signature: impl Into<String>,
        strategy: FixStrategy,
        category: IssueCategory,
    ) -> Self {
        Self {
            pattern_type: PatternType::FailedFixStrategy,
            description: format!("{} failed for {} issues", strategy, category),
            confidence: 0.7, // Moderate confidence for failed patterns
            source_issue_signature: Some(signature.into()),
            effective_strategy: Some(strategy),
            category: Some(category),
        }
    }

    pub fn recurring_issue(signature: impl Into<String>, occurrences: u32) -> Self {
        // Confidence scales with occurrences (capped at 0.95)
        let confidence = (0.6 + (occurrences as f32 * 0.05)).min(0.95);
        Self {
            pattern_type: PatternType::RecurringIssue,
            description: format!("Issue recurred {} times", occurrences),
            confidence,
            source_issue_signature: Some(signature.into()),
            effective_strategy: None,
            category: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternType {
    /// A fix strategy that consistently worked for this issue type
    SuccessfulFixStrategy,
    /// A fix strategy that consistently failed for this issue type
    FailedFixStrategy,
    /// An issue that keeps reappearing after fixes
    RecurringIssue,
}

/// Summary of issues encountered during verification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueSummary {
    pub total_found: u32,
    pub total_fixed: u32,
    pub by_category: Vec<(IssueCategory, u32)>,
    pub persistent_signatures: Vec<String>,
}

impl IssueSummary {
    pub fn fix_rate(&self) -> f32 {
        rate(self.total_fixed, self.total_found, 1.0)
    }
}

/// Summary of fix attempts during verification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixSummary {
    pub total_attempts: u32,
    pub successful_attempts: u32,
    pub strategy_effectiveness: Vec<StrategyEffectiveness>,
}

impl FixSummary {
    pub fn success_rate(&self) -> f32 {
        rate(self.successful_attempts, self.total_attempts, 0.0)
    }

    pub fn best_strategy(&self) -> Option<(FixStrategy, f32)> {
        self.strategy_effectiveness
            .iter()
            .filter(|s| s.total() >= 2)
            .max_by(|a, b| {
                a.success_rate()
                    .partial_cmp(&b.success_rate())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|s| (s.strategy, s.success_rate()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEffectiveness {
    pub strategy: FixStrategy,
    pub successes: u32,
    pub failures: u32,
}

impl StrategyEffectiveness {
    pub fn total(&self) -> u32 {
        self.successes + self.failures
    }

    pub fn success_rate(&self) -> f32 {
        rate(self.successes, self.total(), 0.0)
    }
}

/// Mission-wide verification archive for checkpoint persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerificationArchive {
    pub mission_id: String,
    pub task_archives: Vec<TaskVerificationArchive>,
    pub global_patterns: Vec<LearnedPattern>,
    pub created_at: DateTime<Utc>,
    pub last_updated_at: DateTime<Utc>,
}

impl VerificationArchive {
    pub fn new(mission_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            mission_id: mission_id.into(),
            task_archives: Vec::new(),
            global_patterns: Vec::new(),
            created_at: now,
            last_updated_at: now,
        }
    }

    /// Default confidence threshold for promoting patterns to global patterns.
    pub const DEFAULT_PATTERN_CONFIDENCE_THRESHOLD: f32 = 0.7;

    /// Archive a task's verification results with default confidence threshold.
    pub fn archive_task(
        &mut self,
        task_id: impl Into<String>,
        history: &VerificationHistory,
        state: &ConvergenceState,
        outcome: VerificationOutcome,
    ) {
        self.archive_task_with_threshold(
            task_id,
            history,
            state,
            outcome,
            Self::DEFAULT_PATTERN_CONFIDENCE_THRESHOLD,
        );
    }

    /// Archive a task's verification results with configurable confidence threshold.
    pub fn archive_task_with_threshold(
        &mut self,
        task_id: impl Into<String>,
        history: &VerificationHistory,
        state: &ConvergenceState,
        outcome: VerificationOutcome,
        pattern_confidence_threshold: f32,
    ) {
        let archive = TaskVerificationArchive::from_verification(task_id, history, state, outcome);

        // Extract patterns with sufficient confidence to global patterns.
        for pattern in &archive.learned_patterns {
            if pattern.confidence >= pattern_confidence_threshold
                && !self.has_similar_pattern(pattern)
            {
                self.global_patterns.push(pattern.clone());
            }
        }

        self.task_archives.push(archive);
        self.last_updated_at = Utc::now();
    }

    fn has_similar_pattern(&self, pattern: &LearnedPattern) -> bool {
        self.global_patterns.iter().any(|p| {
            p.pattern_type == pattern.pattern_type
                && p.source_issue_signature == pattern.source_issue_signature
        })
    }

    /// Get effective strategies for an issue category based on history.
    pub fn effective_strategies_for(&self, category: &IssueCategory) -> Vec<(FixStrategy, f32)> {
        let mut strategy_stats: HashMap<FixStrategy, (u32, u32)> = HashMap::new();

        for archive in &self.task_archives {
            for se in &archive.fix_summary.strategy_effectiveness {
                let entry = strategy_stats.entry(se.strategy).or_insert((0, 0));
                entry.0 += se.successes;
                entry.1 += se.failures;
            }
        }

        // Also consider patterns specific to this category
        for pattern in &self.global_patterns {
            if pattern.category.as_ref() == Some(category)
                && let Some(strategy) = pattern.effective_strategy
            {
                let entry = strategy_stats.entry(strategy).or_insert((0, 0));
                match pattern.pattern_type {
                    PatternType::SuccessfulFixStrategy => entry.0 += 1,
                    PatternType::FailedFixStrategy => entry.1 += 1,
                    _ => {}
                }
            }
        }

        strategy_stats
            .into_iter()
            .filter(|(_, (s, f))| s + f > 0)
            .map(|(strategy, (s, f))| (strategy, s as f32 / (s + f) as f32))
            .collect()
    }

    /// Get overall statistics.
    pub fn statistics(&self) -> ArchiveStatistics {
        let total_tasks = self.task_archives.len();
        let converged_tasks = self
            .task_archives
            .iter()
            .filter(|a| a.outcome == VerificationOutcome::Converged)
            .count();
        let total_rounds: u32 = self.task_archives.iter().map(|a| a.total_rounds).sum();
        let total_duration_ms: u64 = self.task_archives.iter().map(|a| a.duration_total_ms).sum();
        let total_issues: u32 = self
            .task_archives
            .iter()
            .map(|a| a.issue_summary.total_found)
            .sum();
        let fixed_issues: u32 = self
            .task_archives
            .iter()
            .map(|a| a.issue_summary.total_fixed)
            .sum();

        ArchiveStatistics {
            total_tasks,
            converged_tasks,
            total_rounds,
            total_duration_ms,
            total_issues,
            fixed_issues,
            global_patterns_count: self.global_patterns.len(),
        }
    }

    /// Compact the archive by keeping only recent tasks and high-confidence patterns.
    pub fn compact(&mut self, max_tasks: usize, max_patterns: usize) {
        // Keep most recent tasks
        if self.task_archives.len() > max_tasks {
            self.task_archives
                .sort_by(|a, b| b.completed_at.cmp(&a.completed_at));
            self.task_archives.truncate(max_tasks);
        }

        // Keep highest confidence patterns
        if self.global_patterns.len() > max_patterns {
            self.global_patterns.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            self.global_patterns.truncate(max_patterns);
        }

        self.last_updated_at = Utc::now();
    }

    /// Summary for logging/debugging.
    pub fn summary(&self) -> String {
        let stats = self.statistics();
        format!(
            "tasks: {}/{} converged, rounds: {}, issues: {}/{} fixed, patterns: {}",
            stats.converged_tasks,
            stats.total_tasks,
            stats.total_rounds,
            stats.fixed_issues,
            stats.total_issues,
            stats.global_patterns_count
        )
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveStatistics {
    pub total_tasks: usize,
    pub converged_tasks: usize,
    pub total_rounds: u32,
    pub total_duration_ms: u64,
    pub total_issues: u32,
    pub fixed_issues: u32,
    pub global_patterns_count: usize,
}

impl ArchiveStatistics {
    pub fn convergence_rate(&self) -> f32 {
        rate(self.converged_tasks as u32, self.total_tasks as u32, 0.0)
    }

    pub fn fix_rate(&self) -> f32 {
        rate(self.fixed_issues, self.total_issues, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::history::{FixAttempt, VerificationRound};
    use super::super::issue::{Issue, IssueSeverity};
    use super::*;

    fn create_test_history() -> VerificationHistory {
        const TEST_CONFIDENCE: f32 = 0.8;
        let mut history = VerificationHistory::new("M001");

        let issues = vec![Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error[E0308]: type mismatch",
            TEST_CONFIDENCE,
        )];
        let mut round = VerificationRound::new(1, issues);
        round
            .fix_attempts
            .push(FixAttempt::new("build_E0308", FixStrategy::DirectFix, true));
        history.add_round(round);

        history.add_round(VerificationRound::new(2, vec![]));
        history.add_round(VerificationRound::new(3, vec![]));

        history
    }

    fn create_test_state() -> ConvergenceState {
        ConvergenceState::new("M001", 2, 10, 3)
    }

    #[test]
    fn test_task_archive_creation() {
        let history = create_test_history();
        let state = create_test_state();

        let archive = TaskVerificationArchive::from_verification(
            "T001",
            &history,
            &state,
            VerificationOutcome::Converged,
        );

        assert_eq!(archive.task_id, "T001");
        assert_eq!(archive.outcome, VerificationOutcome::Converged);
        assert_eq!(archive.fix_summary.total_attempts, 1);
        assert_eq!(archive.fix_summary.successful_attempts, 1);
    }

    #[test]
    fn test_verification_archive() {
        let mut archive = VerificationArchive::new("M001");

        let history = create_test_history();
        let state = create_test_state();

        archive.archive_task("T001", &history, &state, VerificationOutcome::Converged);
        archive.archive_task("T002", &history, &state, VerificationOutcome::Converged);

        let stats = archive.statistics();
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.converged_tasks, 2);
        assert_eq!(stats.convergence_rate(), 1.0);
    }

    #[test]
    fn test_archive_compaction() {
        let mut archive = VerificationArchive::new("M001");

        let history = create_test_history();
        let state = create_test_state();

        for i in 0..10 {
            archive.archive_task(
                format!("T{:03}", i),
                &history,
                &state,
                VerificationOutcome::Converged,
            );
        }

        assert_eq!(archive.task_archives.len(), 10);

        archive.compact(5, 10);
        assert_eq!(archive.task_archives.len(), 5);
    }

    #[test]
    fn test_strategy_effectiveness() {
        let mut archive = VerificationArchive::new("M001");

        // Add pattern
        archive
            .global_patterns
            .push(LearnedPattern::successful_strategy(
                "build_E0308",
                FixStrategy::DirectFix,
                IssueCategory::BuildError,
            ));

        let strategies = archive.effective_strategies_for(&IssueCategory::BuildError);
        assert!(!strategies.is_empty());
    }
}
