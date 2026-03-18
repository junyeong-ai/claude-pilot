use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::Severity;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeLocation {
    pub file: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub snippet: Option<String>,
}

impl CodeLocation {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            line: None,
            column: None,
            snippet: None,
        }
    }

    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    pub fn with_snippet(mut self, snippet: impl Into<String>) -> Self {
        self.snippet = Some(snippet.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoherenceCheckType {
    ContractConsistency,
    IntegrationSoundness,
    MissionCompletion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoherenceResult {
    pub check_type: CoherenceCheckType,
    pub score: f32,
    pub passed: bool,
    pub issues: Vec<CoherenceIssue>,
    pub affected_tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoherenceIssue {
    pub check_type: CoherenceCheckType,
    pub description: String,
    pub task_a: Option<String>,
    pub task_b: Option<String>,
    pub severity: Severity,
    pub suggested_resolution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedCoherence {
    pub contract_consistency: CoherenceResult,
    pub integration_soundness: CoherenceResult,
    pub mission_completion: CoherenceResult,
    pub overall_passed: bool,
    pub tasks_needing_rework: Vec<String>,
}

impl AggregatedCoherence {
    pub fn new(
        contract_consistency: CoherenceResult,
        integration_soundness: CoherenceResult,
        mission_completion: CoherenceResult,
    ) -> Self {
        let overall_passed = contract_consistency.passed
            && integration_soundness.passed
            && mission_completion.passed;

        let mut tasks_needing_rework = Vec::new();
        tasks_needing_rework.extend(contract_consistency.affected_tasks.iter().cloned());
        tasks_needing_rework.extend(integration_soundness.affected_tasks.iter().cloned());
        tasks_needing_rework.sort();
        tasks_needing_rework.dedup();

        Self {
            contract_consistency,
            integration_soundness,
            mission_completion,
            overall_passed,
            tasks_needing_rework,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityFileChange {
    pub path: PathBuf,
    pub change_type: QualityFileChangeType,
    pub content: Option<String>,
    pub diff: Option<String>,
}

impl QualityFileChange {
    pub fn new(path: impl Into<PathBuf>, change_type: QualityFileChangeType) -> Self {
        Self {
            path: path.into(),
            change_type,
            content: None,
            diff: None,
        }
    }

    pub fn modified(path: impl Into<PathBuf>) -> Self {
        Self::new(path, QualityFileChangeType::Modified)
    }

    pub fn created(path: impl Into<PathBuf>) -> Self {
        Self::new(path, QualityFileChangeType::Added)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityFileChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}
