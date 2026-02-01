use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::error::{PilotError, Result};

static ERROR_CODE_PATTERN: OnceLock<Regex> = OnceLock::new();

/// Generic error code pattern that matches common formats across languages.
fn error_code_pattern() -> &'static Regex {
    ERROR_CODE_PATTERN.get_or_init(|| {
        // Match any alphanumeric error code pattern: XX0000, XXX0000, etc.
        Regex::new(r"[A-Z]{1,5}\d{3,5}").unwrap()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Success,
    Failure,
    Partial { resolved: u32, remaining: u32 },
}

impl OutcomeStatus {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryContext {
    pub task_id: Option<String>,
    pub files: Vec<String>,
    pub error_message: String,
    pub attempt_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryOutcome {
    pub created_at: DateTime<Utc>,
    pub error_category: String,
    pub issue_signature: String,
    pub strategy: String,
    pub outcome: OutcomeStatus,
    pub context: RetryContext,
}

impl RetryOutcome {
    pub fn new(
        error_category: impl Into<String>,
        issue_signature: impl Into<String>,
        strategy: impl Into<String>,
        outcome: OutcomeStatus,
        context: RetryContext,
    ) -> Self {
        Self {
            created_at: Utc::now(),
            error_category: error_category.into(),
            issue_signature: issue_signature.into(),
            strategy: strategy.into(),
            outcome,
            context,
        }
    }

    pub fn success(
        error_category: impl Into<String>,
        issue_signature: impl Into<String>,
        strategy: impl Into<String>,
        context: RetryContext,
    ) -> Self {
        Self::new(
            error_category,
            issue_signature,
            strategy,
            OutcomeStatus::Success,
            context,
        )
    }

    pub fn failure(
        error_category: impl Into<String>,
        issue_signature: impl Into<String>,
        strategy: impl Into<String>,
        context: RetryContext,
    ) -> Self {
        Self::new(
            error_category,
            issue_signature,
            strategy,
            OutcomeStatus::Failure,
            context,
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct StrategyStats {
    pub successes: usize,
    pub failures: usize,
    pub partials: usize,
}

impl StrategyStats {
    pub fn total(&self) -> usize {
        self.successes + self.failures + self.partials
    }

    pub fn success_rate(&self) -> f32 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.successes as f32 / total as f32
    }

    pub fn record(&mut self, outcome: &OutcomeStatus) {
        match outcome {
            OutcomeStatus::Success => self.successes += 1,
            OutcomeStatus::Failure => self.failures += 1,
            OutcomeStatus::Partial { .. } => self.partials += 1,
        }
    }
}

pub struct RetryHistory {
    path: PathBuf,
    retention_days: i64,
}

impl RetryHistory {
    pub fn new(path: PathBuf, retention_days: i64) -> Self {
        Self {
            path,
            retention_days,
        }
    }

    pub fn from_config(pilot_dir: &Path, retention_days: i64) -> Self {
        let path = pilot_dir.join("learning").join("retry_history.jsonl");
        Self::new(path, retention_days)
    }

    pub async fn ensure_dir(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    pub async fn append(&self, outcome: &RetryOutcome) -> Result<()> {
        self.ensure_dir().await?;

        let line = serde_json::to_string(outcome)
            .map_err(|e| PilotError::Learning(format!("JSON serialize failed: {}", e)))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        file.write_all(format!("{}\n", line).as_bytes()).await?;
        file.flush().await?;

        debug!(path = %self.path.display(), "Appended retry outcome");
        Ok(())
    }

    pub async fn load(&self) -> Result<Vec<RetryOutcome>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.path).await?;
        let cutoff = Utc::now() - Duration::days(self.retention_days);

        let outcomes: Vec<RetryOutcome> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| match serde_json::from_str::<RetryOutcome>(line) {
                Ok(outcome) if outcome.created_at >= cutoff => Some(outcome),
                Ok(_) => None,
                Err(e) => {
                    warn!(line = %line, error = %e, "Skipping invalid history line");
                    None
                }
            })
            .collect();

        debug!(count = outcomes.len(), path = %self.path.display(), "Loaded retry history");
        Ok(outcomes)
    }

    pub async fn compact(&self) -> Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.path).await?;
        let all_lines: Vec<_> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let original_count = all_lines.len();

        let cutoff = Utc::now() - Duration::days(self.retention_days);
        let valid_outcomes: Vec<RetryOutcome> = all_lines
            .into_iter()
            .filter_map(|line| serde_json::from_str::<RetryOutcome>(line).ok())
            .filter(|o| o.created_at >= cutoff)
            .collect();

        if valid_outcomes.len() == original_count {
            return Ok(0);
        }

        self.ensure_dir().await?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .await?;

        for outcome in &valid_outcomes {
            let line = serde_json::to_string(outcome)
                .map_err(|e| PilotError::Learning(format!("JSON serialize failed: {}", e)))?;
            file.write_all(format!("{}\n", line).as_bytes()).await?;
        }

        file.flush().await?;
        let removed = original_count - valid_outcomes.len();
        debug!(removed, "Compacted retry history");
        Ok(removed)
    }
}

pub fn find_similar<'a>(
    history: &'a [RetryOutcome],
    category: &str,
    signature: &str,
) -> Vec<&'a RetryOutcome> {
    history
        .iter()
        .filter(|o| o.error_category == category || o.issue_signature == signature)
        .collect()
}

pub fn strategy_effectiveness(
    history: &[RetryOutcome],
    category: &str,
) -> HashMap<String, StrategyStats> {
    let mut stats: HashMap<String, StrategyStats> = HashMap::new();

    for outcome in history.iter().filter(|o| o.error_category == category) {
        stats
            .entry(outcome.strategy.clone())
            .or_default()
            .record(&outcome.outcome);
    }

    stats
}

pub fn best_strategy(
    history: &[RetryOutcome],
    category: &str,
    min_samples: usize,
) -> Option<(String, f32)> {
    strategy_effectiveness(history, category)
        .into_iter()
        .filter(|(_, s)| s.total() >= min_samples)
        .max_by(|(_, a), (_, b)| {
            a.success_rate()
                .partial_cmp(&b.success_rate())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(strategy, s)| (strategy, s.success_rate()))
}

/// Extract issue signature: `{category}_{error_code}` or `{category}` if no code.
pub fn extract_issue_signature(error_category: &str, error_message: &str) -> String {
    match extract_error_code(error_message) {
        code if code.is_empty() => error_category.to_string(),
        code => format!("{}_{}", error_category, code),
    }
}

/// Extract error code (E0308, TS2322, CS8602, etc.) from message.
fn extract_error_code(message: &str) -> String {
    error_code_pattern()
        .find(message)
        .map(|m| m.as_str().to_uppercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_context() -> RetryContext {
        RetryContext {
            task_id: Some("T001".into()),
            files: vec!["src/lib.rs".into()],
            error_message: "expected `String`, found `&str`".into(),
            attempt_number: 2,
        }
    }

    #[test]
    fn test_outcome_result_is_success() {
        assert!(OutcomeStatus::Success.is_success());
        assert!(!OutcomeStatus::Failure.is_success());
        assert!(
            !OutcomeStatus::Partial {
                resolved: 1,
                remaining: 2
            }
            .is_success()
        );
    }

    #[test]
    fn test_strategy_stats() {
        let mut stats = StrategyStats::default();
        stats.record(&OutcomeStatus::Success);
        stats.record(&OutcomeStatus::Success);
        stats.record(&OutcomeStatus::Failure);

        assert_eq!(stats.total(), 3);
        assert!((stats.success_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_extract_issue_signature() {
        // With error code - combines category and code
        assert_eq!(
            extract_issue_signature("type_check", "error[E0308]: mismatched types"),
            "type_check_E0308"
        );
        assert_eq!(
            extract_issue_signature("type_check", "TS2322: Type 'A' is not assignable"),
            "type_check_TS2322"
        );

        // Without error code - uses category only (language-agnostic fallback)
        assert_eq!(
            extract_issue_signature("build", "找不到模块 foo"), // Chinese error
            "build"
        );
        assert_eq!(
            extract_issue_signature("test_failure", "assertion failed"),
            "test_failure"
        );

        // This design ensures equal treatment for all languages
        // Non-English toolchains get the same signature quality as English ones
    }

    #[test]
    fn test_extract_error_code() {
        assert_eq!(extract_error_code("error[E0308]: type mismatch"), "E0308");
        assert_eq!(extract_error_code("TS2304: Cannot find name"), "TS2304");
        assert_eq!(extract_error_code("no error code"), "");
    }

    #[tokio::test]
    async fn test_append_and_load() {
        let dir = TempDir::new().unwrap();
        let history = RetryHistory::new(dir.path().join("test.jsonl"), 90);

        let outcome = RetryOutcome::success(
            "build_failure",
            "E0308_type_mismatch",
            "direct_fix",
            test_context(),
        );

        history.append(&outcome).await.unwrap();
        let loaded = history.load().await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].error_category, "build_failure");
        assert_eq!(loaded[0].strategy, "direct_fix");
    }

    #[tokio::test]
    async fn test_strategy_effectiveness() {
        let dir = TempDir::new().unwrap();
        let store = RetryHistory::new(dir.path().join("test.jsonl"), 90);

        let ctx = test_context();

        store
            .append(&RetryOutcome::success(
                "build",
                "sig1",
                "strategy_a",
                ctx.clone(),
            ))
            .await
            .unwrap();
        store
            .append(&RetryOutcome::success(
                "build",
                "sig2",
                "strategy_a",
                ctx.clone(),
            ))
            .await
            .unwrap();
        store
            .append(&RetryOutcome::failure(
                "build",
                "sig3",
                "strategy_b",
                ctx.clone(),
            ))
            .await
            .unwrap();

        let outcomes = store.load().await.unwrap();
        let stats = strategy_effectiveness(&outcomes, "build");

        assert_eq!(stats.get("strategy_a").map(|s| s.successes), Some(2));
        assert_eq!(stats.get("strategy_b").map(|s| s.failures), Some(1));
    }

    #[tokio::test]
    async fn test_best_strategy() {
        let dir = TempDir::new().unwrap();
        let store = RetryHistory::new(dir.path().join("test.jsonl"), 90);

        let ctx = test_context();

        store
            .append(&RetryOutcome::success("build", "sig", "good", ctx.clone()))
            .await
            .unwrap();
        store
            .append(&RetryOutcome::success("build", "sig", "good", ctx.clone()))
            .await
            .unwrap();
        store
            .append(&RetryOutcome::failure("build", "sig", "bad", ctx.clone()))
            .await
            .unwrap();
        store
            .append(&RetryOutcome::failure("build", "sig", "bad", ctx.clone()))
            .await
            .unwrap();

        let outcomes = store.load().await.unwrap();
        let best = best_strategy(&outcomes, "build", 2);

        assert_eq!(best.map(|(s, _)| s), Some("good".into()));
    }
}
