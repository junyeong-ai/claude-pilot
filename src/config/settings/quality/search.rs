use serde::{Deserialize, Serialize};

/// Configuration for search and retrieval features.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable search-based features (fix history, analysis index).
    pub enabled: bool,
    /// Directory name for search indexes (relative to pilot_dir).
    pub index_dir: String,
    /// Timeout in seconds for search operations.
    pub timeout_secs: u64,
    /// Include previous fix attempts in search context.
    pub include_previous_attempts: bool,
    /// Maximum tokens for related code in search results.
    pub max_related_code_tokens: usize,
    /// Maximum number of fix records to consider from history.
    pub max_fix_records: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            index_dir: "search".into(),
            timeout_secs: 10,
            include_previous_attempts: true,
            max_related_code_tokens: 2000,
            max_fix_records: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusConfig {
    pub enabled: bool,
    pub max_context_switches: u32,
    pub derive_from_plan: bool,
    pub derive_from_evidence: bool,
    pub evidence_confidence_threshold: f32,
    pub warn_on_unexpected_files: bool,
    pub strict_module_boundaries: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_context_switches: 3,
            derive_from_plan: true,
            derive_from_evidence: true,
            evidence_confidence_threshold: 0.7,
            warn_on_unexpected_files: true,
            strict_module_boundaries: false,
        }
    }
}
