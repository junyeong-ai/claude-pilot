use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, warn};

use crate::error::{PilotError, Result};
use crate::utils::estimate_tokens;
use crate::verification::{FixStrategy, IssueCategory};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixRecord {
    pub id: String,
    #[serde(rename = "ts")]
    pub created_at: DateTime<Utc>,
    pub category: IssueCategory,
    pub strategy: FixStrategy,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_modified: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub learnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_path: Option<String>,
    #[serde(default)]
    pub detail_tokens: usize,
}

impl FixRecord {
    pub fn new(
        id: impl Into<String>,
        category: IssueCategory,
        strategy: FixStrategy,
        success: bool,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            created_at: Utc::now(),
            category,
            strategy,
            success,
            files_modified: Vec::new(),
            mission_id: None,
            summary: summary.into(),
            learnings: Vec::new(),
            detail_path: None,
            detail_tokens: 0,
        }
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_modified = files;
        self
    }

    pub fn with_mission(mut self, mission_id: impl Into<String>) -> Self {
        self.mission_id = Some(mission_id.into());
        self
    }

    pub fn with_learnings(mut self, learnings: Vec<String>) -> Self {
        self.learnings = learnings;
        self
    }

    pub fn with_detail(mut self, path: impl Into<String>, tokens: usize) -> Self {
        self.detail_path = Some(path.into());
        self.detail_tokens = tokens;
        self
    }

    pub fn index_tokens(&self) -> usize {
        50 + estimate_tokens(&self.summary)
            + self
                .learnings
                .iter()
                .map(|l| estimate_tokens(l))
                .sum::<usize>()
    }

    pub fn to_jsonl(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| PilotError::Serialization(format!("JSONL serialize: {}", e)))
    }

    pub fn from_jsonl(line: &str) -> Result<Self> {
        serde_json::from_str(line)
            .map_err(|e| PilotError::Serialization(format!("JSONL parse: {}", e)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixDetail {
    pub record_id: String,
    pub error_message: String,
    pub action_taken: String,
    #[serde(default)]
    pub outcome_analysis: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_involved: Vec<String>,
}

impl FixDetail {
    pub fn new(
        record_id: impl Into<String>,
        error_message: impl Into<String>,
        action_taken: impl Into<String>,
    ) -> Self {
        Self {
            record_id: record_id.into(),
            error_message: error_message.into(),
            action_taken: action_taken.into(),
            outcome_analysis: None,
            files_involved: Vec::new(),
        }
    }

    pub fn with_analysis(mut self, analysis: impl Into<String>) -> Self {
        self.outcome_analysis = Some(analysis.into());
        self
    }

    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.files_involved.push(file.into());
        self
    }

    pub fn estimate_tokens(&self) -> usize {
        let mut total = estimate_tokens(&self.error_message);
        total += estimate_tokens(&self.action_taken);
        if let Some(ref a) = self.outcome_analysis {
            total += estimate_tokens(a);
        }
        total
    }

    pub async fn save(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml_bw::to_string(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(path, yaml).await?;
        Ok(())
    }

    pub async fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).await?;
        let detail: Self = serde_yaml_bw::from_str(&content)?;
        Ok(detail)
    }
}

pub struct FixLoader {
    index_dir: PathBuf,
}

impl FixLoader {
    pub fn new(index_dir: impl AsRef<Path>) -> Self {
        Self {
            index_dir: index_dir.as_ref().to_path_buf(),
        }
    }

    pub async fn load_detail(&self, record: &FixRecord, budget: usize) -> Option<FixDetail> {
        if record.detail_tokens > budget {
            debug!(
                record_id = %record.id,
                detail_tokens = record.detail_tokens,
                budget,
                "Detail exceeds budget"
            );
            return None;
        }

        let path = match &record.detail_path {
            Some(p) => self.index_dir.join(p),
            None => return None,
        };

        match FixDetail::load(&path).await {
            Ok(detail) => Some(detail),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to load detail");
                None
            }
        }
    }

    pub async fn load_with_budget(
        &self,
        records: &[FixRecord],
        total_budget: usize,
        max_with_detail: usize,
    ) -> Vec<LoadedFix> {
        let mut results = Vec::new();
        let mut used_tokens = 0;

        for (i, record) in records.iter().enumerate() {
            let index_tokens = record.index_tokens();
            if used_tokens + index_tokens > total_budget {
                break;
            }
            used_tokens += index_tokens;

            let detail = if i < max_with_detail {
                let remaining = total_budget.saturating_sub(used_tokens);
                if let Some(d) = self.load_detail(record, remaining).await {
                    used_tokens += d.estimate_tokens();
                    Some(d)
                } else {
                    None
                }
            } else {
                None
            };

            results.push(LoadedFix {
                record: record.clone(),
                detail,
            });
        }

        results
    }
}

#[derive(Debug)]
pub struct LoadedFix {
    pub record: FixRecord,
    pub detail: Option<FixDetail>,
}

impl LoadedFix {
    pub fn to_prompt_context(&self) -> String {
        let mut output = format!("### {}\n", self.record.summary);

        for learning in &self.record.learnings {
            output.push_str(&format!("- {}\n", learning));
        }

        if let Some(ref detail) = self.detail {
            output.push_str(&format!("\n**Action**: {}\n", detail.action_taken));
            if let Some(ref analysis) = detail.outcome_analysis {
                output.push_str(&format!("**Result**: {}\n", analysis));
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fix_record_creation() {
        let record = FixRecord::new(
            "fix-001",
            IssueCategory::BuildError,
            FixStrategy::DirectFix,
            true,
            "Fixed type mismatch",
        )
        .with_mission("m-008")
        .with_learnings(vec!["Use .into() for conversion".to_string()])
        .with_detail("details/fix-001.yaml", 500);

        assert_eq!(record.id, "fix-001");
        assert_eq!(record.category, IssueCategory::BuildError);
        assert!(record.success);
        assert!(record.detail_path.is_some());
    }

    #[test]
    fn test_fix_record_jsonl_roundtrip() {
        let record = FixRecord::new(
            "fix-002",
            IssueCategory::TestFailure,
            FixStrategy::ContextualFix,
            false,
            "Test fix",
        )
        .with_mission("m-010");

        let jsonl = record.to_jsonl().unwrap();
        let parsed = FixRecord::from_jsonl(&jsonl).unwrap();

        assert_eq!(parsed.id, "fix-002");
        assert_eq!(parsed.category, IssueCategory::TestFailure);
        assert_eq!(parsed.mission_id, Some("m-010".to_string()));
    }

    #[test]
    fn test_fix_detail() {
        let detail = FixDetail::new("fix-001", "error[E0308]", "Added .to_string()")
            .with_analysis("Successfully fixed");

        assert!(detail.estimate_tokens() > 0);
    }
}
