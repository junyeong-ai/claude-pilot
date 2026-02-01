use std::collections::{HashMap, HashSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::issue::{Issue, IssueCategory};
use crate::config::ConvergentVerificationConfig;
use crate::utils::truncate_with_marker;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscillationContext {
    pub oscillation_count: u32,
    pub total_rounds: u32,
    pub reappearing_issues: Vec<String>,
    pub net_progress: i32,
    pub progress_trend: ProgressTrend,
    pub dominant_categories: Vec<IssueCategory>,
    pub pattern_type: OscillationPatternType,
    pub unique_issues_seen: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgressTrend {
    Improving,
    Stagnant,
    Regressing,
}

/// Categorizes the oscillation pattern to help LLM make better decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OscillationPatternType {
    /// Same exact issues keep reappearing (true oscillation, likely stuck)
    TrueOscillation,
    /// Issues reappear but with variations (complex refactoring, may resolve)
    VariantReappearance,
    /// Many different issues appearing (high churn but making progress)
    HighChurnProgress,
    /// Too few rounds to determine pattern
    Indeterminate,
}

impl OscillationPatternType {
    pub fn description(&self) -> &'static str {
        match self {
            Self::TrueOscillation => "same issues keep reappearing unchanged (likely stuck)",
            Self::VariantReappearance => "issues reappear with variations (complex refactoring)",
            Self::HighChurnProgress => {
                "many different issues appearing (high churn but progressing)"
            }
            Self::Indeterminate => "too few rounds to determine pattern",
        }
    }
}

impl OscillationContext {
    pub fn format_for_llm(&self) -> String {
        let trend_desc = match self.progress_trend {
            ProgressTrend::Improving => "showing improvement despite oscillation",
            ProgressTrend::Stagnant => "no clear progress",
            ProgressTrend::Regressing => "getting worse",
        };

        let categories: Vec<_> = self
            .dominant_categories
            .iter()
            .map(|c| format!("{}", c))
            .collect();

        format!(
            r"**Oscillation Analysis**
- Rounds: {} (oscillation count: {})
- Progress trend: {}
- Net progress: {} (positive = improving)
- Pattern type: {}
- Unique issues seen: {}
- Reappearing issues ({}): [{}]
- Dominant categories: [{}]

**Decision guidance**: Consider pattern type when deciding. VariantReappearance and HighChurnProgress may resolve with more iterations. TrueOscillation suggests fundamental blocker.",
            self.total_rounds,
            self.oscillation_count,
            trend_desc,
            self.net_progress,
            self.pattern_type.description(),
            self.unique_issues_seen,
            self.reappearing_issues.len(),
            self.reappearing_issues.join(", "),
            categories.join(", ")
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalIssueRegistry {
    issues: HashMap<String, GlobalIssueEntry>,
    #[serde(default = "GlobalIssueRegistry::default_max_entries")]
    max_entries: usize,
}

impl Default for GlobalIssueRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalIssueEntry {
    pub signature: String,
    pub category: IssueCategory,
    pub first_seen_round: u32,
    pub last_seen_round: u32,
    pub total_occurrences: u32,
    pub fix_attempts: u32,
    pub tried_strategies: HashSet<FixStrategy>,
    pub last_strategy: Option<FixStrategy>,
    pub resolved: bool,
    pub resolution_round: Option<u32>,
}

impl GlobalIssueRegistry {
    const DEFAULT_MAX_ENTRIES: usize = 20;

    fn default_max_entries() -> usize {
        Self::DEFAULT_MAX_ENTRIES
    }

    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_MAX_ENTRIES)
    }

    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            issues: HashMap::new(),
            max_entries,
        }
    }

    pub fn len(&self) -> usize {
        self.issues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn record_issue(&mut self, issue: &Issue, round: u32) {
        let signature = issue.signature();
        self.issues
            .entry(signature.clone())
            .and_modify(|e| {
                e.last_seen_round = round;
                e.total_occurrences += 1;
                e.resolved = false;
                e.resolution_round = None;
            })
            .or_insert(GlobalIssueEntry {
                signature,
                category: issue.category.clone(),
                first_seen_round: round,
                last_seen_round: round,
                total_occurrences: 1,
                fix_attempts: 0,
                tried_strategies: HashSet::new(),
                last_strategy: None,
                resolved: false,
                resolution_round: None,
            });
    }

    pub fn record_fix_attempt(&mut self, signature: &str, strategy: FixStrategy) {
        if let Some(entry) = self.issues.get_mut(signature) {
            entry.fix_attempts += 1;
            entry.tried_strategies.insert(strategy);
            entry.last_strategy = Some(strategy);
        }
    }

    pub fn entry(&self, signature: &str) -> Option<&GlobalIssueEntry> {
        self.issues.get(signature)
    }

    pub fn next_strategy(&self, issue: &Issue) -> Option<FixStrategy> {
        self.next_strategy_for_signature(&issue.signature())
    }

    pub fn next_strategy_for_signature(&self, signature: &str) -> Option<FixStrategy> {
        let entry = self.issues.get(signature)?;
        let priority_order = Self::strategy_priority_for_category(&entry.category);

        priority_order
            .into_iter()
            .find(|strategy| !entry.tried_strategies.contains(strategy))
    }

    /// Returns all available fix strategies.
    fn strategy_priority_for_category(_category: &IssueCategory) -> Vec<FixStrategy> {
        FixStrategy::all_strategies().to_vec()
    }

    pub fn mark_resolved(&mut self, signature: &str, round: u32) {
        if let Some(entry) = self.issues.get_mut(signature) {
            entry.resolved = true;
            entry.resolution_round = Some(round);
        }
    }

    /// Returns the number of fix attempts for the given issue signature.
    pub fn fix_attempts(&self, signature: &str) -> u32 {
        self.issues
            .get(signature)
            .map(|e| e.fix_attempts)
            .unwrap_or(0)
    }

    pub fn is_exhausted(&self, signature: &str, max_attempts: u32) -> bool {
        self.fix_attempts(signature) >= max_attempts
    }

    pub fn update_from_round(
        &mut self,
        current_issues: &[Issue],
        previous_issues: &[Issue],
        round: u32,
    ) {
        let current_sigs: HashSet<String> = current_issues.iter().map(|i| i.signature()).collect();
        let previous_sigs: HashSet<String> =
            previous_issues.iter().map(|i| i.signature()).collect();

        for issue in current_issues {
            self.record_issue(issue, round);
        }

        for sig in previous_sigs.difference(&current_sigs) {
            self.mark_resolved(sig, round);
        }

        self.prune_if_needed();
    }

    pub fn update_from_signatures(
        &mut self,
        current: &HashSet<String>,
        previous: &HashSet<String>,
        round: u32,
    ) {
        for sig in previous.difference(current) {
            self.mark_resolved(sig, round);
        }
        self.prune_if_needed();
    }

    fn prune_if_needed(&mut self) {
        if self.issues.len() <= self.max_entries {
            return;
        }

        let resolved: Vec<_> = self
            .issues
            .iter()
            .filter(|(_, e)| e.resolved)
            .map(|(k, _)| k.clone())
            .collect();

        for key in resolved {
            self.issues.remove(&key);
            if self.issues.len() <= self.max_entries {
                return;
            }
        }
    }

    pub fn unresolved_count(&self) -> usize {
        self.issues.values().filter(|e| !e.resolved).count()
    }

    pub fn total_fix_attempts(&self) -> u32 {
        self.issues.values().map(|e| e.fix_attempts).sum()
    }

    /// Default threshold for persistent issue detection.
    /// Matches `ConvergentVerificationConfig::persistent_issue_threshold` default.
    pub const DEFAULT_PERSISTENT_THRESHOLD: u32 = 3;

    /// Get issues that are considered "persistent" (recurring and unresolved).
    /// An issue is persistent if it has occurred >= threshold times and remains unresolved.
    /// Use `DEFAULT_PERSISTENT_THRESHOLD` when config is not available.
    pub fn persistent_issues(&self, threshold: u32) -> Vec<&GlobalIssueEntry> {
        self.issues
            .values()
            .filter(|e| e.total_occurrences >= threshold && !e.resolved)
            .collect()
    }
}

/// Lightweight drift analysis - stores signatures only to prevent memory bloat
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriftAnalysis {
    pub new_issue_count: usize,
    pub resolved_issue_count: usize,
    pub persistent_issue_count: usize,
    pub has_regression: bool,
    pub drift_score: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationHistory {
    pub mission_id: String,
    pub rounds: VecDeque<VerificationRound>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default = "VerificationHistory::default_max_rounds")]
    max_rounds_kept: usize,
    #[serde(default)]
    total_rounds_count: u32,
}

impl VerificationHistory {
    const DEFAULT_MAX_ROUNDS: usize = 3;

    fn default_max_rounds() -> usize {
        Self::DEFAULT_MAX_ROUNDS
    }

    pub fn new(mission_id: impl Into<String>) -> Self {
        Self::with_capacity(mission_id, Self::DEFAULT_MAX_ROUNDS)
    }

    pub fn with_capacity(mission_id: impl Into<String>, max_rounds_kept: usize) -> Self {
        Self {
            mission_id: mission_id.into(),
            rounds: VecDeque::new(),
            started_at: Utc::now(),
            completed_at: None,
            max_rounds_kept,
            total_rounds_count: 0,
        }
    }

    pub fn add_round(&mut self, round: VerificationRound) {
        self.total_rounds_count += 1;
        self.rounds.push_back(round);

        while self.rounds.len() > self.max_rounds_kept {
            self.rounds.pop_front();
        }
    }

    pub fn record_fixes(&mut self, round_number: u32, fixes: &[FixAttempt]) {
        if let Some(round) = self
            .rounds
            .iter_mut()
            .find(|r| r.round_number == round_number)
        {
            round.fix_attempts.extend(fixes.iter().cloned());
        }
    }

    pub fn mark_completed(&mut self) {
        self.completed_at = Some(Utc::now());
    }

    pub fn total_issues_found(&self) -> usize {
        self.rounds.iter().map(|r| r.issues_found.len()).sum()
    }

    pub fn total_issues_fixed(&self) -> usize {
        self.rounds
            .iter()
            .flat_map(|r| &r.fix_attempts)
            .filter(|f| f.success)
            .count()
    }

    pub fn consecutive_clean_rounds(&self) -> u32 {
        let mut count = 0;
        for round in self.rounds.iter().rev() {
            if round.is_clean {
                count += 1;
            } else {
                break;
            }
        }
        count
    }

    pub fn last_round(&self) -> Option<&VerificationRound> {
        self.rounds.back()
    }

    pub fn total_rounds_processed(&self) -> u32 {
        self.total_rounds_count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationRound {
    pub round_number: u32,
    pub issues_found: Vec<Issue>,
    pub is_clean: bool,
    pub fix_attempts: Vec<FixAttempt>,
    pub duration_ms: u64,
    pub created_at: DateTime<Utc>,
}

impl VerificationRound {
    pub fn new(round_number: u32, issues: Vec<Issue>) -> Self {
        Self {
            round_number,
            is_clean: issues.is_empty(),
            issues_found: issues,
            fix_attempts: Vec::new(),
            duration_ms: 0,
            created_at: Utc::now(),
        }
    }

    pub fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self
    }
}

impl Default for VerificationRound {
    fn default() -> Self {
        Self {
            round_number: 0,
            issues_found: Vec::new(),
            is_clean: true,
            fix_attempts: Vec::new(),
            duration_ms: 0,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixAttempt {
    pub issue_id: String,
    pub strategy: FixStrategy,
    pub success: bool,
    pub output_summary: String,
    pub files_modified: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl FixAttempt {
    const DEFAULT_MAX_OUTPUT_LENGTH: usize = 500;

    pub fn new(issue_id: impl Into<String>, strategy: FixStrategy, success: bool) -> Self {
        Self {
            issue_id: issue_id.into(),
            strategy,
            success,
            output_summary: String::new(),
            files_modified: Vec::new(),
            created_at: Utc::now(),
        }
    }

    pub fn with_output(self, output: impl Into<String>) -> Self {
        self.with_output_limit(output, Self::DEFAULT_MAX_OUTPUT_LENGTH)
    }

    pub fn with_output_limit(mut self, output: impl Into<String>, max_length: usize) -> Self {
        let full = output.into();
        self.output_summary = truncate_with_marker(&full, max_length);
        self
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_modified = files;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixStrategy {
    DirectFix,
    ContextualFix,
    DependencyFix,
    TypeInferenceFix,
    RefactorFix,
    Rollback,
}

impl std::fmt::Display for FixStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirectFix => write!(f, "direct_fix"),
            Self::ContextualFix => write!(f, "contextual_fix"),
            Self::DependencyFix => write!(f, "dependency_fix"),
            Self::TypeInferenceFix => write!(f, "type_inference_fix"),
            Self::RefactorFix => write!(f, "refactor_fix"),
            Self::Rollback => write!(f, "rollback"),
        }
    }
}

impl FixStrategy {
    /// Returns all active fix strategies available for automated attempts.
    /// Excludes Rollback which is a terminal action, not a fix attempt.
    pub fn all_strategies() -> &'static [FixStrategy] {
        const STRATEGIES: &[FixStrategy] = &[
            FixStrategy::DirectFix,
            FixStrategy::ContextualFix,
            FixStrategy::DependencyFix,
            FixStrategy::TypeInferenceFix,
            FixStrategy::RefactorFix,
        ];
        STRATEGIES
    }

    /// Number of active fix strategies. Derived from `all_strategies().len()`.
    #[inline]
    pub fn active_count() -> usize {
        Self::all_strategies().len()
    }

    pub fn prompt_hint(&self) -> &'static str {
        match self {
            Self::DirectFix => {
                "Fix the exact error message by making minimal targeted changes to the indicated location."
            }
            Self::ContextualFix => {
                "Analyze the surrounding code context and fix the issue by understanding the broader intent."
            }
            Self::DependencyFix => {
                "Check and fix any dependency issues, imports, or missing type definitions that cause this error."
            }
            Self::TypeInferenceFix => {
                "Focus on type annotations, generics, and trait bounds to resolve the issue."
            }
            Self::RefactorFix => {
                "Restructure the problematic code section to avoid the issue entirely."
            }
            Self::Rollback => "Revert recent changes to restore working state.",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceState {
    pub mission_id: String,
    pub is_converged: bool,
    pub consecutive_clean: u32,
    pub required_clean: u32,
    pub total_rounds: u32,
    pub max_rounds: u32,
    pub regression_detection_rounds: usize,
    /// Maximum drift history entries to keep
    max_drift_history_window: usize,
    /// Signatures of unresolved issues (lightweight)
    unresolved_signatures: HashSet<String>,
    /// Signatures from previous round for drift detection
    previous_signatures: HashSet<String>,
    /// Recent drift analyses (sliding window)
    drift_history: VecDeque<DriftAnalysis>,
    pub issue_registry: GlobalIssueRegistry,
    pub last_updated_at: DateTime<Utc>,
    /// Context from first pass AI review for adversarial second pass.
    /// Stores what the first pass reviewed so the second pass can critique it.
    #[serde(default)]
    first_pass_review_context: Option<String>,
    /// Oscillation counter - incremented when new issues appear without progress
    #[serde(default)]
    oscillation_count: u32,
    /// Threshold for classifying issues as "persistent" (configurable).
    #[serde(default = "default_persistent_threshold")]
    pub persistent_issue_threshold: u32,
    /// Grace period before oscillation detection starts (configurable).
    /// Issues reappearing after this many rounds count toward oscillation.
    #[serde(default = "default_oscillation_grace")]
    oscillation_grace_rounds: u32,
    /// Extra buffer for regression detection (configurable).
    /// Regression without progress triggers after grace + buffer rounds.
    #[serde(default = "default_oscillation_regression_buffer")]
    oscillation_regression_buffer: u32,
}

fn default_persistent_threshold() -> u32 {
    3
}

fn default_oscillation_grace() -> u32 {
    2
}

fn default_oscillation_regression_buffer() -> u32 {
    1
}

impl ConvergenceState {
    const DEFAULT_MAX_DRIFT_HISTORY: usize = 5;

    /// Create from configuration struct (preferred method).
    pub fn from_config(
        mission_id: impl Into<String>,
        config: &ConvergentVerificationConfig,
    ) -> Self {
        Self {
            mission_id: mission_id.into(),
            is_converged: false,
            consecutive_clean: 0,
            required_clean: config.required_clean_rounds,
            total_rounds: 0,
            max_rounds: config.max_rounds,
            regression_detection_rounds: config.regression_detection_rounds,
            max_drift_history_window: config.max_drift_history_window,
            unresolved_signatures: HashSet::new(),
            previous_signatures: HashSet::new(),
            drift_history: VecDeque::with_capacity(config.max_drift_history_window),
            issue_registry: GlobalIssueRegistry::with_capacity(config.max_issue_registry_entries),
            last_updated_at: Utc::now(),
            first_pass_review_context: None,
            oscillation_count: 0,
            persistent_issue_threshold: config.persistent_issue_threshold,
            oscillation_grace_rounds: config.oscillation_grace_rounds,
            oscillation_regression_buffer: config.oscillation_regression_buffer,
        }
    }

    /// Create with explicit parameters (for testing or special cases).
    pub fn new(
        mission_id: impl Into<String>,
        required_clean: u32,
        max_rounds: u32,
        regression_detection_rounds: usize,
    ) -> Self {
        Self {
            mission_id: mission_id.into(),
            is_converged: false,
            consecutive_clean: 0,
            required_clean,
            total_rounds: 0,
            max_rounds,
            regression_detection_rounds,
            max_drift_history_window: Self::DEFAULT_MAX_DRIFT_HISTORY,
            unresolved_signatures: HashSet::new(),
            previous_signatures: HashSet::new(),
            drift_history: VecDeque::with_capacity(Self::DEFAULT_MAX_DRIFT_HISTORY),
            issue_registry: GlobalIssueRegistry::new(),
            last_updated_at: Utc::now(),
            first_pass_review_context: None,
            oscillation_count: 0,
            persistent_issue_threshold: default_persistent_threshold(),
            oscillation_grace_rounds: default_oscillation_grace(),
            oscillation_regression_buffer: default_oscillation_regression_buffer(),
        }
    }

    pub fn unresolved_issues(&self) -> Vec<String> {
        self.unresolved_signatures.iter().cloned().collect()
    }

    pub fn record_round(&mut self, is_clean: bool, issues: Vec<Issue>) {
        self.total_rounds += 1;

        let current_sigs: HashSet<String> = issues.iter().map(|i| i.signature()).collect();
        self.issue_registry.update_from_signatures(
            &current_sigs,
            &self.previous_signatures,
            self.total_rounds,
        );

        for issue in &issues {
            self.issue_registry.record_issue(issue, self.total_rounds);
        }

        let drift = self.analyze_drift(&current_sigs);
        self.drift_history.push_back(drift);
        while self.drift_history.len() > self.max_drift_history_window {
            self.drift_history.pop_front();
        }

        if is_clean {
            self.consecutive_clean += 1;
            self.oscillation_count = 0;
            if self.consecutive_clean >= self.required_clean {
                self.is_converged = true;
            }
            self.unresolved_signatures.clear();
        } else {
            let resolved = self.unresolved_signatures.difference(&current_sigs).count();
            let new_issues = current_sigs.difference(&self.unresolved_signatures).count();
            let reappeared = current_sigs
                .intersection(&self.previous_signatures)
                .filter(|sig| !self.unresolved_signatures.contains(*sig))
                .count();

            // True oscillation: issues we thought were resolved have reappeared
            // Grace period: only count after configured grace rounds (give fixes time to stabilize)
            let grace = self.oscillation_grace_rounds;
            let regression_threshold = grace + self.oscillation_regression_buffer;

            if reappeared > 0 && self.total_rounds > grace {
                self.oscillation_count += 1;
            } else if new_issues > 0 && resolved == 0 && self.total_rounds > regression_threshold {
                // Regression without progress after extended grace period
                self.oscillation_count += 1;
            } else if resolved > new_issues && resolved > 0 {
                // Making progress, reduce oscillation count
                self.oscillation_count = self.oscillation_count.saturating_sub(1);
            }

            self.consecutive_clean = 0;
            self.unresolved_signatures = current_sigs.clone();
        }

        self.previous_signatures = current_sigs;
        self.last_updated_at = Utc::now();
    }

    pub fn is_oscillating(&self, threshold: u32) -> bool {
        self.oscillation_count >= threshold
    }

    pub fn get_oscillation_context(&self) -> OscillationContext {
        let reappearing_issues: Vec<String> = self
            .issue_registry
            .issues
            .iter()
            .filter(|(_, entry)| entry.total_occurrences > 1 && !entry.resolved)
            .map(|(sig, _)| sig.clone())
            .collect();

        let net_progress: i32 = self.drift_history.iter().map(|d| d.drift_score).sum();

        let progress_trend = if self.drift_history.len() >= 2 {
            let recent: Vec<_> = self.drift_history.iter().rev().take(3).collect();
            let recent_sum: i32 = recent.iter().map(|d| d.drift_score).sum();
            if recent_sum > 0 {
                ProgressTrend::Improving
            } else if recent_sum < -1 {
                ProgressTrend::Regressing
            } else {
                ProgressTrend::Stagnant
            }
        } else {
            ProgressTrend::Stagnant
        };

        let mut category_counts: HashMap<IssueCategory, usize> = HashMap::new();
        for entry in self.issue_registry.issues.values() {
            if !entry.resolved {
                *category_counts.entry(entry.category.clone()).or_default() += 1;
            }
        }
        let mut dominant_categories: Vec<_> = category_counts.into_iter().collect();
        dominant_categories.sort_by(|a, b| b.1.cmp(&a.1));
        let dominant_categories: Vec<_> = dominant_categories
            .into_iter()
            .take(3)
            .map(|(cat, _)| cat)
            .collect();

        // Calculate pattern type based on issue behavior
        let unique_issues_seen = self.issue_registry.issues.len();
        let pattern_type = self.determine_oscillation_pattern(
            &reappearing_issues,
            unique_issues_seen,
            progress_trend,
        );

        OscillationContext {
            oscillation_count: self.oscillation_count,
            total_rounds: self.total_rounds,
            reappearing_issues,
            net_progress,
            progress_trend,
            dominant_categories,
            pattern_type,
            unique_issues_seen,
        }
    }

    /// Determine the oscillation pattern type based on issue behavior.
    fn determine_oscillation_pattern(
        &self,
        reappearing_issues: &[String],
        unique_issues_seen: usize,
        progress_trend: ProgressTrend,
    ) -> OscillationPatternType {
        // Too few rounds to determine pattern
        if self.total_rounds < 4 {
            return OscillationPatternType::Indeterminate;
        }

        let reappearing_count = reappearing_issues.len();

        // High churn but progressing: many unique issues, positive trend
        if unique_issues_seen > reappearing_count * 2 && progress_trend == ProgressTrend::Improving
        {
            return OscillationPatternType::HighChurnProgress;
        }

        // Check if reappearing issues have high occurrence counts (true oscillation)
        let high_recurrence_count = self
            .issue_registry
            .issues
            .values()
            .filter(|e| e.total_occurrences >= 3 && !e.resolved)
            .count();

        if high_recurrence_count > 0 && progress_trend != ProgressTrend::Improving {
            // Same issues keep coming back with no progress
            return OscillationPatternType::TrueOscillation;
        }

        // Issues reappear but situation is changing (variant reappearance)
        if reappearing_count > 0 {
            return OscillationPatternType::VariantReappearance;
        }

        OscillationPatternType::Indeterminate
    }

    /// Check if an issue has exhausted fix attempts globally
    pub fn is_issue_exhausted(&self, issue: &Issue, max_attempts: u32) -> bool {
        self.issue_registry
            .is_exhausted(&issue.signature(), max_attempts)
    }

    /// Record a fix attempt for an issue with the strategy used
    pub fn record_fix_attempt(&mut self, issue: &Issue, strategy: FixStrategy) {
        self.issue_registry
            .record_fix_attempt(&issue.signature(), strategy);
    }

    /// Get the next untried strategy for an issue
    pub fn next_strategy(&self, issue: &Issue) -> Option<FixStrategy> {
        self.issue_registry.next_strategy(issue)
    }

    /// Returns global fix attempts for an issue.
    pub fn global_fix_attempts(&self, issue: &Issue) -> u32 {
        self.issue_registry.fix_attempts(&issue.signature())
    }

    fn analyze_drift(&self, current_sigs: &HashSet<String>) -> DriftAnalysis {
        let new_count = current_sigs.difference(&self.previous_signatures).count();
        let resolved_count = self.previous_signatures.difference(current_sigs).count();
        let persistent_count = current_sigs.intersection(&self.previous_signatures).count();

        let has_regression = new_count > 0 && self.total_rounds > 1 && new_count > resolved_count;
        let drift_score = resolved_count as i32 - new_count as i32;

        DriftAnalysis {
            new_issue_count: new_count,
            resolved_issue_count: resolved_count,
            persistent_issue_count: persistent_count,
            has_regression,
            drift_score,
        }
    }

    pub fn last_drift(&self) -> Option<&DriftAnalysis> {
        self.drift_history.back()
    }

    pub fn has_consistent_regression(&self) -> bool {
        self.drift_history
            .iter()
            .rev()
            .take(self.regression_detection_rounds)
            .filter(|d| d.drift_score < 0)
            .count()
            >= 2
    }

    pub fn cumulative_drift_score(&self) -> i32 {
        self.drift_history.iter().map(|d| d.drift_score).sum()
    }

    pub fn is_exhausted(&self) -> bool {
        self.total_rounds >= self.max_rounds && !self.is_converged
    }

    pub fn has_unresolved(&self) -> bool {
        !self.unresolved_signatures.is_empty()
    }

    pub fn first_unresolved_strategy(&self) -> Option<FixStrategy> {
        self.unresolved_signatures
            .iter()
            .next()
            .and_then(|sig| self.issue_registry.next_strategy_for_signature(sig))
    }

    pub fn all_strategies_exhausted(&self) -> bool {
        self.unresolved_signatures.iter().all(|sig| {
            self.issue_registry
                .next_strategy_for_signature(sig)
                .is_none()
        })
    }

    /// Store the first pass review context for adversarial second pass.
    pub fn set_first_pass_context(&mut self, context: String) {
        self.first_pass_review_context = Some(context);
    }

    /// Get the first pass review context for adversarial second pass.
    pub fn first_pass_context(&self) -> Option<&str> {
        self.first_pass_review_context.as_deref()
    }

    /// Clear the first pass context after convergence or reset.
    pub fn clear_first_pass_context(&mut self) {
        self.first_pass_review_context = None;
    }

    /// Get current oscillation count.
    pub fn oscillation_count(&self) -> u32 {
        self.oscillation_count
    }

    /// Calculate extension eligibility based on observed progress.
    /// Returns (should_extend, net_resolved, net_introduced).
    pub fn calculate_extension_eligibility(&self, oscillation_threshold: u32) -> (bool, i32, i32) {
        // Never extend if oscillating
        if self.oscillation_count >= oscillation_threshold {
            return (false, 0, 0);
        }

        if self.drift_history.is_empty() {
            return (false, 0, 0);
        }

        // Calculate net progress over recent rounds
        let (net_resolved, net_introduced): (i32, i32) =
            self.drift_history.iter().fold((0, 0), |(res, intro), d| {
                (
                    res + d.resolved_issue_count as i32,
                    intro + d.new_issue_count as i32,
                )
            });

        let net_progress = net_resolved - net_introduced;

        // Require net positive progress
        if net_progress <= 0 {
            return (false, net_resolved, net_introduced);
        }

        // Check that at least some issues were resolved recently (not stagnant)
        let any_recent_resolution = self
            .drift_history
            .iter()
            .any(|d| d.resolved_issue_count > 0);
        if !any_recent_resolution {
            return (false, net_resolved, net_introduced);
        }

        (true, net_resolved, net_introduced)
    }
}
