use serde::{Deserialize, Serialize};

fn validate_ratio(value: f32, name: &str) -> Result<(), String> {
    if (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{} must be between 0.0 and 1.0, got {}",
            name, value
        ))
    }
}

// EVIDENCE GATHERING CONFIG
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceGatheringConfig {
    pub max_iterations: u32,
    pub total_timeout_secs: u64,
    pub convergence_threshold: f32,
    pub max_retries: u32,
    pub retry_base_delay_ms: u64,
    pub max_content_matches: usize,
    /// Minimum matches per file for ripgrep --max-count.
    /// Higher values ensure we don't miss matches in files with many occurrences.
    pub min_per_file_matches: usize,
    pub confidence: ConfidenceScores,
    /// Minimum score threshold for cache hits (0.0-1.0).
    /// Only cached results with score above this threshold are considered relevant.
    pub cache_relevance_threshold: f32,
    /// Minimum coverage score (0.0-1.0) below which refinement is triggered
    /// when search limits are hit. Lower values allow more tolerance for
    /// incomplete results.
    pub min_coverage_threshold: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfidenceScores {
    pub direct_path: f32,
    pub glob_pattern: f32,
    pub content_match: f32,
}

impl Default for ConfidenceScores {
    fn default() -> Self {
        Self {
            direct_path: 0.95,
            glob_pattern: 0.90,
            content_match: 0.70,
        }
    }
}

impl Default for EvidenceGatheringConfig {
    fn default() -> Self {
        Self {
            max_iterations: 3,
            total_timeout_secs: 300,
            convergence_threshold: 0.8,
            max_retries: 3,
            retry_base_delay_ms: 500,
            max_content_matches: 5000,
            min_per_file_matches: 100,
            confidence: ConfidenceScores::default(),
            cache_relevance_threshold: 0.7,
            min_coverage_threshold: 0.6,
        }
    }
}

impl EvidenceGatheringConfig {
    /// Returns the effective coverage threshold adjusted for task complexity.
    /// TRIVIAL tasks can proceed with lower coverage (simpler scope).
    /// COMPLEX tasks require full threshold (more risk from incomplete evidence).
    ///
    /// Multipliers:
    /// - TRIVIAL: 0.7 (30% lower bar)
    /// - SIMPLE: 0.85 (15% lower bar)
    /// - COMPLEX: 1.0 (full threshold)
    pub fn effective_threshold(&self, complexity: ComplexityLevel) -> f32 {
        let multiplier = match complexity {
            ComplexityLevel::Trivial => 0.7,
            ComplexityLevel::Simple => 0.85,
            ComplexityLevel::Complex => 1.0,
        };
        self.min_coverage_threshold * multiplier
    }
}

/// Simplified complexity level for threshold adjustment.
/// Maps to ComplexityTier but decoupled from planning module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexityLevel {
    Trivial,
    Simple,
    Complex,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceSufficiencyConfig {
    pub min_coverage_ratio: f32,
    pub halt_on_insufficient: bool,
    pub max_collection_time_secs: u64,
    pub gathering: EvidenceGatheringConfig,
    pub sources: EvidenceSourcesConfig,
    pub verification: EvidenceVerificationConfig,
}

impl Default for EvidenceSufficiencyConfig {
    fn default() -> Self {
        Self {
            min_coverage_ratio: 0.8,
            halt_on_insufficient: true,
            max_collection_time_secs: 300,
            gathering: EvidenceGatheringConfig::default(),
            sources: EvidenceSourcesConfig::default(),
            verification: EvidenceVerificationConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceSourcesConfig {
    pub file_structure: bool,
    pub dependencies: bool,
    pub patterns: bool,
    pub tests: bool,
    pub documentation: bool,
}

impl Default for EvidenceSourcesConfig {
    fn default() -> Self {
        Self {
            file_structure: true,
            dependencies: true,
            patterns: true,
            tests: true,
            documentation: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceVerificationConfig {
    pub verify_paths: bool,
    pub verify_names: bool,
    pub require_citations: bool,
}

impl Default for EvidenceVerificationConfig {
    fn default() -> Self {
        Self {
            verify_paths: true,
            verify_names: true,
            require_citations: true,
        }
    }
}

// COHERENCE CONFIG
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoherenceConfig {
    pub enabled: bool,
    pub run_after_all_tasks: bool,
    pub incremental_enabled: bool,
    pub incremental_interval: u32,
    pub fail_fast: bool,
    pub thresholds: CoherenceThresholds,
}

impl Default for CoherenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            run_after_all_tasks: true,
            incremental_enabled: true,
            incremental_interval: 2,
            fail_fast: true,
            thresholds: CoherenceThresholds::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoherenceThresholds {
    pub contract_consistency: f32,
    pub integration_soundness: f32,
    pub mission_completion: f32,
    /// Fallback word overlap threshold when LLM is unavailable (0.0-1.0).
    pub word_overlap_fallback: f32,
    /// Weight for critical severity issues in score calculation (0.0-1.0).
    pub critical_severity_weight: f32,
    /// Weight for major severity issues in score calculation (0.0-1.0).
    pub major_severity_weight: f32,
    /// Threshold for integration soundness file modification warning.
    /// Files modified by more than this many tasks trigger a warning.
    pub multi_task_modification_threshold: usize,
}

impl Default for CoherenceThresholds {
    fn default() -> Self {
        Self {
            contract_consistency: 0.8,
            integration_soundness: 0.7,
            mission_completion: 0.85,
            word_overlap_fallback: 0.7,
            critical_severity_weight: 0.3,
            major_severity_weight: 0.15,
            multi_task_modification_threshold: 2,
        }
    }
}

impl CoherenceThresholds {
    pub fn validate(&self) -> Result<(), String> {
        validate_ratio(self.contract_consistency, "contract_consistency")?;
        validate_ratio(self.integration_soundness, "integration_soundness")?;
        validate_ratio(self.mission_completion, "mission_completion")?;
        Ok(())
    }
}
