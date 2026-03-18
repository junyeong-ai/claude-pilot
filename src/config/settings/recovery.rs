use serde::{Deserialize, Serialize};

use crate::error::{PilotError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryConfig {
    pub enabled: bool,
    pub max_retries_per_failure: u32,
    pub failure_history_ttl_secs: u64,
    pub retry_analyzer: RetryAnalyzerConfig,
    pub execution_error: ExecutionErrorConfig,
    pub compaction_levels: CompactionLevelConfig,
    pub convergent_verification: ConvergentVerificationConfig,
    pub checkpoint: CheckpointConfig,
    pub reasoning: ReasoningConfig,
}

impl RecoveryConfig {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        if let Err(e) = self.convergent_verification.validate() {
            errors.push(e.to_string());
        }
        if self.checkpoint.interval_tasks == 0 {
            errors.push("checkpoint.interval_tasks must be greater than 0".into());
        }
        if self.checkpoint.max_checkpoints == 0 {
            errors.push("checkpoint.max_checkpoints must be greater than 0".into());
        }
        if self.reasoning.enabled && self.reasoning.retention_days <= 0 {
            errors.push("reasoning.retention_days must be positive when enabled".into());
        }
        if self.reasoning.max_hypotheses_in_context == 0 {
            errors.push("reasoning.max_hypotheses_in_context must be greater than 0".into());
        }
        if self.reasoning.max_decisions_in_context == 0 {
            errors.push("reasoning.max_decisions_in_context must be greater than 0".into());
        }
        if !(0.0..=1.0).contains(&self.reasoning.recent_decisions_ratio) {
            errors.push("reasoning.recent_decisions_ratio must be between 0.0 and 1.0".into());
        }

        // Compaction level strategy validation
        self.compaction_levels.light.validate("light", &mut errors);
        self.compaction_levels
            .moderate
            .validate("moderate", &mut errors);
        self.compaction_levels
            .aggressive
            .validate("aggressive", &mut errors);
        self.compaction_levels
            .emergency
            .validate("emergency", &mut errors);

        // Retry policy jitter_ratio validation
        for (name, policy) in [
            ("timeout", &self.execution_error.timeout),
            ("chunk_timeout", &self.execution_error.chunk_timeout),
            ("rate_limit", &self.execution_error.rate_limit),
            ("network_error", &self.execution_error.network_error),
            ("stream_error", &self.execution_error.stream_error),
        ] {
            if !(0.0..=1.0).contains(&policy.jitter_ratio) {
                errors.push(format!(
                    "execution_error.{}.jitter_ratio must be between 0.0 and 1.0",
                    name
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "RecoveryConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries_per_failure: 3,
            failure_history_ttl_secs: 3600,
            retry_analyzer: RetryAnalyzerConfig::default(),
            execution_error: ExecutionErrorConfig::default(),
            compaction_levels: CompactionLevelConfig::default(),
            convergent_verification: ConvergentVerificationConfig::default(),
            checkpoint: CheckpointConfig::default(),
            reasoning: ReasoningConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryAnalyzerConfig {
    pub max_error_history: usize,
    pub consecutive_error_threshold: usize,
    /// Maximum retries for transient errors (network, rate-limit, etc.)
    /// before escalating as likely permanent.
    pub transient_error_retry_limit: usize,
}

impl Default for RetryAnalyzerConfig {
    fn default() -> Self {
        Self {
            max_error_history: 5,
            consecutive_error_threshold: 3,
            transient_error_retry_limit: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ErrorRetryPolicy {
    pub retries: u32,
    pub base_delay_secs: u64,
    pub backoff_multiplier: f64,
    pub max_delay_secs: u64,
    pub jitter_ratio: f64,
}

impl ErrorRetryPolicy {
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration {
        let base = self.base_delay_secs as f64;
        let exponential = base * self.backoff_multiplier.powi(attempt as i32);
        let capped = exponential.min(self.max_delay_secs as f64);

        // Deterministic jitter using golden ratio fractional parts (no rand dependency).
        // Maps attempt to a pseudo-random value in [-1.0, 1.0) for symmetric jitter.
        let jitter_factor = (attempt as f64 * std::f64::consts::TAU.recip()).fract() * 2.0 - 1.0;
        let jitter = capped * self.jitter_ratio * jitter_factor;
        let final_secs = (capped + jitter).max(0.0);

        std::time::Duration::from_secs_f64(final_secs)
    }
}

impl Default for ErrorRetryPolicy {
    fn default() -> Self {
        Self {
            retries: 1,
            base_delay_secs: 5,
            backoff_multiplier: 2.0,
            max_delay_secs: 300,
            jitter_ratio: 0.25,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionErrorConfig {
    pub timeout: ErrorRetryPolicy,
    pub chunk_timeout: ErrorRetryPolicy,
    pub rate_limit: ErrorRetryPolicy,
    pub network_error: ErrorRetryPolicy,
    pub stream_error: ErrorRetryPolicy,
}

impl Default for ExecutionErrorConfig {
    fn default() -> Self {
        let default_backoff = ErrorRetryPolicy::default();
        Self {
            timeout: ErrorRetryPolicy { retries: 3, base_delay_secs: 5, ..default_backoff.clone() },
            chunk_timeout: ErrorRetryPolicy { retries: 2, base_delay_secs: 10, ..default_backoff.clone() },
            rate_limit: ErrorRetryPolicy { retries: 1, base_delay_secs: 120, max_delay_secs: 600, ..default_backoff.clone() },
            network_error: ErrorRetryPolicy { retries: 3, base_delay_secs: 5, ..default_backoff.clone() },
            stream_error: ErrorRetryPolicy { retries: 2, base_delay_secs: 5, ..default_backoff },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionStrategy {
    pub threshold: f32,
    pub aggressive: f32,
    pub preserve_tasks: usize,
    pub preserve_learnings: usize,
}

impl CompactionStrategy {
    fn validate(&self, level: &str, errors: &mut Vec<String>) {
        super::validate_unit_f32(
            self.threshold,
            &format!("compaction_levels.{}.threshold", level),
            errors,
        );
        super::validate_unit_f32(
            self.aggressive,
            &format!("compaction_levels.{}.aggressive", level),
            errors,
        );
    }
}

impl Default for CompactionStrategy {
    fn default() -> Self {
        Self {
            threshold: 0.8,
            aggressive: 0.5,
            preserve_tasks: 5,
            preserve_learnings: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionLevelConfig {
    pub light: CompactionStrategy,
    pub moderate: CompactionStrategy,
    pub aggressive: CompactionStrategy,
    pub emergency: CompactionStrategy,
    pub emergency_min_phase_age: usize,
}

impl Default for CompactionLevelConfig {
    fn default() -> Self {
        Self {
            light: CompactionStrategy {
                threshold: 0.9,
                aggressive: 0.7,
                preserve_tasks: 5,
                preserve_learnings: 10,
            },
            moderate: CompactionStrategy {
                threshold: 0.8,
                aggressive: 0.5,
                preserve_tasks: 5,
                preserve_learnings: 10,
            },
            aggressive: CompactionStrategy {
                threshold: 0.6,
                aggressive: 0.4,
                preserve_tasks: 3,
                preserve_learnings: 5,
            },
            emergency: CompactionStrategy {
                threshold: 0.4,
                aggressive: 0.2,
                preserve_tasks: 1,
                preserve_learnings: 3,
            },
            emergency_min_phase_age: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvergentVerificationConfig {
    pub required_clean_rounds: u32,
    pub max_rounds: u32,
    pub max_fix_attempts_per_issue: u32,
    pub include_ai_review: bool,
    pub auto_fix_enabled: bool,
    /// Timeout for individual fix attempts in seconds (default: 300 = 5 minutes)
    pub fix_timeout_secs: u64,
    /// Maximum total time for convergent verification in seconds (default: 1800 = 30 minutes)
    pub total_timeout_secs: u64,
    /// Number of recent rounds to check for regression detection (default: 3)
    pub regression_detection_rounds: usize,
    /// Maximum rounds to keep in verification history.
    pub max_history_rounds: usize,
    /// Maximum issues to process per batch in fix attempts.
    pub max_issues_per_batch: usize,
    /// Maximum length for fix output storage.
    pub max_fix_output_length: usize,
    /// Whether to use isolated sessions for fix attempts.
    pub use_isolated_fix_session: bool,
    /// Maximum entries in drift history window.
    pub max_drift_history_window: usize,
    /// Maximum entries in global issue registry.
    pub max_issue_registry_entries: usize,
    /// Minimum occurrences for an issue to be considered "persistent" (default: 3).
    /// Issues with total_occurrences >= this value and unresolved are persistent.
    pub persistent_issue_threshold: u32,
    /// Oscillation threshold - number of fix-break-fix cycles before declaring oscillation.
    /// Higher values allow more retry attempts before giving up.
    pub oscillation_threshold: u32,
    /// Grace period before oscillation detection kicks in.
    /// Allows fixes time to stabilize before flagging as oscillating.
    /// Default: 2 rounds (issues reappearing after round 2 count as oscillation).
    pub oscillation_grace_rounds: u32,
    /// Additional grace rounds for regression without progress.
    /// Default: 1 extra round beyond base grace period (so round > grace + 1).
    pub oscillation_regression_buffer: u32,
    /// Minimum confidence score (0.0-1.0) for PatternBank recommendations to be used.
    /// Lower values allow more pattern suggestions; higher values require stronger matches.
    pub pattern_confidence_threshold: f32,
    /// Maximum characters for diff display before truncation.
    /// Larger values provide more context but may overwhelm output.
    pub diff_truncation_length: usize,
    /// Confidence score assigned to persistent issues when generating Issue objects.
    /// Higher values make persistent issues more prominent in reports.
    pub persistent_issue_confidence: f32,
    /// Allow adaptive extension of max_rounds when meaningful progress is detected.
    /// If drift analysis shows net issues reduced, additional rounds are granted.
    pub allow_adaptive_extension: bool,
    /// Additional rounds granted per adaptive extension (default: 3).
    pub adaptive_extension_rounds: u32,
    /// Maximum number of adaptive extensions allowed (default: 2, so up to 6 extra rounds).
    pub max_adaptive_extensions: u32,
}

impl ConvergentVerificationConfig {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        if self.required_clean_rounds < 2 {
            errors.push("required_clean_rounds must be >= 2 (NON-NEGOTIABLE)");
        }
        if !self.include_ai_review {
            errors.push("include_ai_review must be true (NON-NEGOTIABLE)");
        }
        if self.max_rounds == 0 {
            errors.push("max_rounds must be greater than 0");
        }
        if self.max_rounds < self.required_clean_rounds {
            errors.push("max_rounds must be >= required_clean_rounds");
        }
        if self.fix_timeout_secs < 30 {
            errors.push("fix_timeout_secs must be at least 30 seconds");
        }
        if self.total_timeout_secs < self.fix_timeout_secs {
            errors.push("total_timeout_secs must be >= fix_timeout_secs");
        }
        if !(0.0..=1.0).contains(&self.persistent_issue_confidence) {
            errors.push("persistent_issue_confidence must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.pattern_confidence_threshold) {
            errors.push("pattern_confidence_threshold must be between 0.0 and 1.0");
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "ConvergentVerificationConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

impl Default for ConvergentVerificationConfig {
    fn default() -> Self {
        Self {
            required_clean_rounds: 2,
            max_rounds: 10,
            max_fix_attempts_per_issue: 5, // Strategies cycle if this exceeds available count
            include_ai_review: true,
            auto_fix_enabled: true,
            fix_timeout_secs: 300,
            total_timeout_secs: 1800,
            regression_detection_rounds: 3,
            max_history_rounds: 20,
            max_issues_per_batch: 5,
            max_fix_output_length: 2000,
            use_isolated_fix_session: false,
            max_drift_history_window: 5,
            max_issue_registry_entries: 100,
            persistent_issue_threshold: 3,
            oscillation_threshold: 3,
            // Grace period for oscillation detection: allows fixes to stabilize.
            // Issues reappearing after this many rounds count toward oscillation.
            oscillation_grace_rounds: 2,
            // Extra buffer for regression (new issues without progress): grace + 1.
            oscillation_regression_buffer: 1,
            pattern_confidence_threshold: 0.6,
            diff_truncation_length: 10000,
            persistent_issue_confidence: 0.90,
            // Adaptive extension: grant extra rounds when meaningful progress is detected.
            // Disabled by default to preserve backwards compatibility with existing behavior.
            allow_adaptive_extension: true,
            adaptive_extension_rounds: 3,
            max_adaptive_extensions: 2,
        }
    }
}

/// Configuration for recovery checkpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckpointConfig {
    /// Interval in minutes between automatic checkpoints.
    pub interval_minutes: u64,
    /// Interval in tasks between automatic checkpoints.
    pub interval_tasks: u32,
    /// Maximum number of checkpoints to keep per mission.
    pub max_checkpoints: usize,
    /// Whether to persist evidence snapshot in checkpoints for durable recovery.
    /// Essential for long-running missions where evidence re-gathering is expensive.
    pub persist_evidence: bool,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            interval_minutes: 10,
            interval_tasks: 3,
            max_checkpoints: 5,
            persist_evidence: true,
        }
    }
}

/// Configuration for reasoning history persistence (durable execution).
/// Enables recovery from long interruptions with full decision context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReasoningConfig {
    /// Enable reasoning history persistence.
    pub enabled: bool,
    /// Retention days for reasoning history.
    pub retention_days: i64,
    /// Maximum hypotheses to keep in checkpoint context.
    pub max_hypotheses_in_context: usize,
    /// Maximum decisions to keep in checkpoint context.
    pub max_decisions_in_context: usize,
    /// Ratio of max_decisions allocated to recent decisions during compaction (0.0-1.0).
    /// The rest is allocated to high-impact decisions.
    /// Default 0.5 means 50% recent, 50% high-impact.
    pub recent_decisions_ratio: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RecoveryConfig smoke ──

    #[test]
    fn default_validates() {
        RecoveryConfig::default().validate().unwrap();
    }

    // ── ConvergentVerificationConfig non-negotiable rules ──

    #[test]
    fn convergent_clean_rounds_below_2_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.required_clean_rounds = 1;
        assert!(c.validate().is_err());
    }

    #[test]
    fn convergent_clean_rounds_zero_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.required_clean_rounds = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn convergent_ai_review_disabled_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.include_ai_review = false;
        assert!(c.validate().is_err());
    }

    // ── Confidence range validation ──

    #[test]
    fn confidence_above_1_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.persistent_issue_confidence = 1.01;
        assert!(c.validate().is_err());
    }

    #[test]
    fn confidence_below_0_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.persistent_issue_confidence = -0.01;
        assert!(c.validate().is_err());
    }

    #[test]
    fn pattern_confidence_boundary_valid() {
        let mut c = ConvergentVerificationConfig::default();
        c.pattern_confidence_threshold = 0.0;
        assert!(c.validate().is_ok());
        c.pattern_confidence_threshold = 1.0;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn persistent_issue_confidence_boundary_valid() {
        let mut c = ConvergentVerificationConfig::default();
        c.persistent_issue_confidence = 0.0;
        assert!(c.validate().is_ok());
        c.persistent_issue_confidence = 1.0;
        assert!(c.validate().is_ok());
    }

    // ── Cross-field constraints ──

    #[test]
    fn max_rounds_less_than_clean_rounds_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.max_rounds = 1;
        c.required_clean_rounds = 2;
        assert!(c.validate().is_err());
    }

    #[test]
    fn max_rounds_equal_clean_rounds_accepted() {
        let mut c = ConvergentVerificationConfig::default();
        c.max_rounds = 2;
        c.required_clean_rounds = 2;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn total_timeout_less_than_fix_timeout_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.total_timeout_secs = 20;
        c.fix_timeout_secs = 30;
        assert!(c.validate().is_err());
    }

    #[test]
    fn total_timeout_equal_fix_timeout_accepted() {
        let mut c = ConvergentVerificationConfig::default();
        c.total_timeout_secs = 300;
        c.fix_timeout_secs = 300;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn fix_timeout_below_30_rejected() {
        let mut c = ConvergentVerificationConfig::default();
        c.fix_timeout_secs = 29;
        assert!(c.validate().is_err());
    }

    #[test]
    fn fix_timeout_exactly_30_accepted() {
        let mut c = ConvergentVerificationConfig::default();
        c.fix_timeout_secs = 30;
        assert!(c.validate().is_ok());
    }

    // ── RecoveryConfig level validation ──

    #[test]
    fn recent_decisions_ratio_above_1_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.recent_decisions_ratio = 1.5;
        assert!(r.validate().is_err());
    }

    #[test]
    fn recent_decisions_ratio_negative_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.recent_decisions_ratio = -0.1;
        assert!(r.validate().is_err());
    }

    #[test]
    fn recent_decisions_ratio_boundary_valid() {
        let mut r = RecoveryConfig::default();
        r.reasoning.recent_decisions_ratio = 0.0;
        assert!(r.validate().is_ok());
        r.reasoning.recent_decisions_ratio = 1.0;
        assert!(r.validate().is_ok());
    }

    #[test]
    fn jitter_ratio_out_of_range_rejected() {
        let mut r = RecoveryConfig::default();
        r.execution_error.timeout.jitter_ratio = -0.1;
        assert!(r.validate().is_err());
        r.execution_error.timeout.jitter_ratio = 1.5;
        assert!(r.validate().is_err());
    }

    #[test]
    fn jitter_ratio_boundary_valid() {
        let mut r = RecoveryConfig::default();
        r.execution_error.timeout.jitter_ratio = 0.0;
        assert!(r.validate().is_ok());
        r.execution_error.timeout.jitter_ratio = 1.0;
        assert!(r.validate().is_ok());
    }

    // ── ErrorRetryPolicy backoff tests ──

    #[test]
    fn delay_for_attempt_exponential() {
        let policy = ErrorRetryPolicy {
            retries: 5,
            base_delay_secs: 1,
            backoff_multiplier: 2.0,
            max_delay_secs: 60,
            jitter_ratio: 0.0,
        };
        let d0 = policy.delay_for_attempt(0);
        let d1 = policy.delay_for_attempt(1);
        let d2 = policy.delay_for_attempt(2);
        assert!(d1 > d0, "d1={:?} should be > d0={:?}", d1, d0);
        assert!(d2 > d1, "d2={:?} should be > d1={:?}", d2, d1);
    }

    #[test]
    fn delay_capped_at_max() {
        let policy = ErrorRetryPolicy {
            retries: 10,
            base_delay_secs: 100,
            backoff_multiplier: 10.0,
            max_delay_secs: 300,
            jitter_ratio: 0.0,
        };
        assert!(policy.delay_for_attempt(5).as_secs() <= 300);
        assert!(policy.delay_for_attempt(10).as_secs() <= 300);
    }

    #[test]
    fn delay_zero_jitter_is_deterministic() {
        let policy = ErrorRetryPolicy {
            retries: 3,
            base_delay_secs: 5,
            backoff_multiplier: 2.0,
            max_delay_secs: 60,
            jitter_ratio: 0.0,
        };
        // With zero jitter, same attempt always gives same delay
        assert_eq!(policy.delay_for_attempt(0), policy.delay_for_attempt(0));
        assert_eq!(policy.delay_for_attempt(2), policy.delay_for_attempt(2));
    }

    #[test]
    fn checkpoint_interval_tasks_zero_rejected() {
        let mut r = RecoveryConfig::default();
        r.checkpoint.interval_tasks = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn checkpoint_max_checkpoints_zero_rejected() {
        let mut r = RecoveryConfig::default();
        r.checkpoint.max_checkpoints = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn compaction_strategy_threshold_above_1_rejected() {
        let mut r = RecoveryConfig::default();
        r.compaction_levels.light.threshold = 1.5;
        assert!(r.validate().is_err());
    }

    #[test]
    fn compaction_strategy_aggressive_negative_rejected() {
        let mut r = RecoveryConfig::default();
        r.compaction_levels.emergency.aggressive = -0.1;
        assert!(r.validate().is_err());
    }

    #[test]
    fn reasoning_retention_days_zero_when_enabled_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.enabled = true;
        r.reasoning.retention_days = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn reasoning_retention_days_negative_when_enabled_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.enabled = true;
        r.reasoning.retention_days = -1;
        assert!(r.validate().is_err());
    }

    #[test]
    fn reasoning_retention_days_ignored_when_disabled() {
        let mut r = RecoveryConfig::default();
        r.reasoning.enabled = false;
        r.reasoning.retention_days = 0;
        assert!(r.validate().is_ok());
    }

    #[test]
    fn reasoning_max_decisions_zero_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.max_decisions_in_context = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn reasoning_max_hypotheses_zero_rejected() {
        let mut r = RecoveryConfig::default();
        r.reasoning.max_hypotheses_in_context = 0;
        assert!(r.validate().is_err());
    }

    #[test]
    fn multiple_errors_aggregated() {
        let mut r = RecoveryConfig::default();
        r.checkpoint.interval_tasks = 0;
        r.checkpoint.max_checkpoints = 0;
        r.reasoning.max_hypotheses_in_context = 0;
        let err = r.validate().unwrap_err().to_string();
        assert!(err.contains("interval_tasks"), "err: {err}");
        assert!(err.contains("max_checkpoints"), "err: {err}");
        assert!(err.contains("max_hypotheses_in_context"), "err: {err}");
    }
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
            max_hypotheses_in_context: 10,
            max_decisions_in_context: 20,
            recent_decisions_ratio: 0.5,
        }
    }
}
