use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;
use tracing::{debug, warn};

use super::record::FixRecord;
use crate::error::{PilotError, Result};
use crate::verification::{FixStrategy, IssueCategory};

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    category: Option<IssueCategory>,
    strategy: Option<FixStrategy>,
    success: Option<bool>,
    mission_id: Option<String>,
    limit: Option<usize>,
    timeout_secs: u64,
}

impl SearchQuery {
    pub fn new() -> Self {
        Self {
            timeout_secs: 30,
            ..Default::default()
        }
    }

    pub fn with_category(mut self, cat: IssueCategory) -> Self {
        self.category = Some(cat);
        self
    }

    pub fn with_strategy(mut self, strat: FixStrategy) -> Self {
        self.strategy = Some(strat);
        self
    }

    pub fn with_success(mut self, success: bool) -> Self {
        self.success = Some(success);
        self
    }

    pub fn with_mission(mut self, id: impl Into<String>) -> Self {
        self.mission_id = Some(id.into());
        self
    }

    pub fn with_limit(mut self, max: usize) -> Self {
        self.limit = Some(max);
        self
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    fn matches(&self, record: &FixRecord) -> bool {
        if self
            .category
            .as_ref()
            .is_some_and(|cat| record.category != *cat)
        {
            return false;
        }
        if self
            .strategy
            .as_ref()
            .is_some_and(|strat| record.strategy != *strat)
        {
            return false;
        }
        if self
            .success
            .is_some_and(|success| record.success != success)
        {
            return false;
        }
        if self
            .mission_id
            .as_ref()
            .is_some_and(|mid| record.mission_id.as_ref() != Some(mid))
        {
            return false;
        }
        true
    }

    fn build_rg_pattern(&self) -> Option<String> {
        if let Some(cat) = &self.category {
            let cat_str = serde_json::to_string(cat).unwrap_or_default();
            return Some(format!("\"category\":{}", cat_str));
        }
        None
    }
}

#[derive(Debug)]
pub struct SearchResult {
    pub records: Vec<FixRecord>,
    pub total_scanned: usize,
    pub search_time_ms: u64,
}

impl SearchResult {
    pub fn empty() -> Self {
        Self {
            records: Vec::new(),
            total_scanned: 0,
            search_time_ms: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }
}

pub struct FixSearcher {
    index_dir: std::path::PathBuf,
    rg_path: String,
}

impl FixSearcher {
    pub fn new(index_dir: impl AsRef<Path>) -> Self {
        Self {
            index_dir: index_dir.as_ref().to_path_buf(),
            rg_path: "rg".to_string(),
        }
    }

    pub async fn search(&self, query: &SearchQuery) -> Result<SearchResult> {
        let start = std::time::Instant::now();

        let fixes_path = self.index_dir.join("fixes.jsonl");
        if !fixes_path.exists() {
            return Ok(SearchResult::empty());
        }

        let pattern = query.build_rg_pattern();
        let raw_matches = if let Some(ref pat) = pattern {
            self.execute_rg(pat, &fixes_path, query.timeout_secs)
                .await?
        } else {
            self.read_all(&fixes_path).await?
        };

        let mut records = Vec::new();
        let mut total_scanned = 0;

        for line in raw_matches.lines() {
            if line.trim().is_empty() {
                continue;
            }
            total_scanned += 1;

            if let Ok(record) = FixRecord::from_jsonl(line)
                && query.matches(&record)
            {
                records.push(record);

                if query.limit.is_some_and(|limit| records.len() >= limit) {
                    break;
                }
            }
        }

        let search_time_ms = start.elapsed().as_millis() as u64;

        debug!(
            total_scanned,
            matched = records.len(),
            search_time_ms,
            "Fix search completed"
        );

        Ok(SearchResult {
            records,
            total_scanned,
            search_time_ms,
        })
    }

    async fn execute_rg(&self, pattern: &str, path: &Path, timeout_secs: u64) -> Result<String> {
        let mut cmd = Command::new(&self.rg_path);
        cmd.arg("--no-filename")
            .arg("--no-line-number")
            .arg("-e")
            .arg(pattern)
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = timeout(Duration::from_secs(timeout_secs), cmd.output())
            .await
            .map_err(|_| PilotError::Timeout("ripgrep search timed out".into()))?
            .map_err(PilotError::Io)?;

        if !output.status.success() && output.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() && !stderr.contains("No such file") {
                warn!(stderr = %stderr, "ripgrep warning");
            }
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    async fn read_all(&self, path: &Path) -> Result<String> {
        if path.exists() {
            Ok(tokio::fs::read_to_string(path).await?)
        } else {
            Ok(String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_query_builder() {
        let query = SearchQuery::new()
            .with_category(IssueCategory::BuildError)
            .with_success(true)
            .with_limit(10);

        assert_eq!(query.category, Some(IssueCategory::BuildError));
        assert_eq!(query.success, Some(true));
        assert_eq!(query.limit, Some(10));
    }

    #[test]
    fn test_query_matches() {
        let record = FixRecord::new(
            "test",
            IssueCategory::BuildError,
            FixStrategy::DirectFix,
            true,
            "Test",
        );

        let query1 = SearchQuery::new().with_category(IssueCategory::BuildError);
        assert!(query1.matches(&record));

        let query2 = SearchQuery::new().with_success(true);
        assert!(query2.matches(&record));

        let query3 = SearchQuery::new().with_category(IssueCategory::TestFailure);
        assert!(!query3.matches(&record));

        let query4 = SearchQuery::new().with_success(false);
        assert!(!query4.matches(&record));
    }
}
