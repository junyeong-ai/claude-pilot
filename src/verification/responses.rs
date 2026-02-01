//! Structured output response types for verification.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{IssueCategory, IssueSeverity};

/// Structured AI review response with checklist-based evaluation.
/// Ensures systematic verification across all quality dimensions.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AiReviewResponse {
    pub checklist: ReviewChecklist,
    #[serde(default)]
    pub issues: Vec<AiReviewIssue>,
}

impl AiReviewResponse {
    pub fn no_issues(&self) -> bool {
        self.issues.is_empty() && self.checklist.all_passed()
    }

    pub fn checked_areas(&self) -> Vec<String> {
        self.checklist.checked_areas()
    }
}

/// Dynamic checklist for AI code review.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ReviewChecklist {
    /// Dynamic list of review dimensions determined by LLM.
    /// Each dimension is a key-value pair: dimension name → checklist item.
    #[serde(default)]
    pub dimensions: std::collections::HashMap<String, ChecklistItem>,
}

impl ReviewChecklist {
    pub fn all_passed(&self) -> bool {
        self.dimensions.values().all(|item| item.passed)
    }

    pub fn checked_areas(&self) -> Vec<String> {
        self.dimensions
            .iter()
            .filter(|(_, item)| item.checked)
            .map(|(name, item)| format!("{}: {}", name, item.summary()))
            .collect()
    }

    pub fn failed_dimensions(&self) -> Vec<String> {
        self.dimensions
            .iter()
            .filter(|(_, item)| !item.passed)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Returns failed dimensions with their full checklist items (including notes).
    /// This preserves the LLM's detailed reasoning for why each dimension failed.
    pub fn failed_items(&self) -> Vec<(&String, &ChecklistItem)> {
        self.dimensions
            .iter()
            .filter(|(_, item)| !item.passed)
            .collect()
    }
}

/// Individual checklist item with verification status.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ChecklistItem {
    pub checked: bool,
    pub passed: bool,
    #[serde(default)]
    pub files_reviewed: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl ChecklistItem {
    pub fn summary(&self) -> String {
        if !self.checked {
            return "not checked".to_string();
        }
        let status = if self.passed { "PASS" } else { "FAIL" };
        let files = if self.files_reviewed.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.files_reviewed.join(", "))
        };
        format!("{}{}", status, files)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AiReviewIssue {
    pub severity: IssueSeverity,
    pub category: IssueCategory,
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub checklist_dimension: Option<String>,
    pub confidence: f32,
}
