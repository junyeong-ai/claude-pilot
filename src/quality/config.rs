use serde::{Deserialize, Serialize};

use crate::config::validate_unit_f32;
use crate::error::{PilotError, Result};

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
    /// Maximum refinement attempts when search limits are hit with low coverage.
    pub max_refinement_attempts: u32,
    /// Minimum improvement ratio to detect coverage plateau.
    pub plateau_threshold: f32,
    /// Number of recent iterations to check for plateau detection.
    pub plateau_window: usize,
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
            max_refinement_attempts: 2,
            plateau_threshold: 0.02,
            plateau_window: 3,
        }
    }
}

impl EvidenceGatheringConfig {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        for (name, value) in [
            ("convergence_threshold", self.convergence_threshold),
            ("cache_relevance_threshold", self.cache_relevance_threshold),
            ("min_coverage_threshold", self.min_coverage_threshold),
            ("plateau_threshold", self.plateau_threshold),
            ("confidence.direct_path", self.confidence.direct_path),
            ("confidence.glob_pattern", self.confidence.glob_pattern),
            ("confidence.content_match", self.confidence.content_match),
        ] {
            validate_unit_f32(value, name, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "EvidenceGatheringConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }

    /// Returns the effective coverage threshold adjusted for task complexity.
    /// Multipliers: TRIVIAL 0.7, SIMPLE 0.85, COMPLEX 1.0.
    pub fn effective_threshold(&self, complexity: crate::planning::ComplexityTier) -> f32 {
        use crate::planning::ComplexityTier;
        let multiplier = match complexity {
            ComplexityTier::Trivial => 0.7,
            ComplexityTier::Simple => 0.85,
            ComplexityTier::Complex => 1.0,
        };
        self.min_coverage_threshold * multiplier
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceSufficiencyConfig {
    pub min_coverage_ratio: f32,
    pub halt_on_insufficient: bool,
    pub max_collection_time_secs: u64,
    pub gathering: EvidenceGatheringConfig,
}

impl EvidenceSufficiencyConfig {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        validate_unit_f32(self.min_coverage_ratio, "min_coverage_ratio", &mut errors);

        if let Err(e) = self.gathering.validate() {
            errors.push(e.to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "EvidenceSufficiencyConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

impl Default for EvidenceSufficiencyConfig {
    fn default() -> Self {
        Self {
            min_coverage_ratio: 0.8,
            halt_on_insufficient: true,
            max_collection_time_secs: 300,
            gathering: EvidenceGatheringConfig::default(),
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
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        for (name, value) in [
            ("contract_consistency", self.contract_consistency),
            ("integration_soundness", self.integration_soundness),
            ("mission_completion", self.mission_completion),
            ("word_overlap_fallback", self.word_overlap_fallback),
            ("critical_severity_weight", self.critical_severity_weight),
            ("major_severity_weight", self.major_severity_weight),
        ] {
            validate_unit_f32(value, name, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "CoherenceThresholds validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CoherenceThresholds ──

    #[test]
    fn coherence_thresholds_default_validates() {
        CoherenceThresholds::default().validate().unwrap();
    }

    #[test]
    fn coherence_threshold_above_1_rejected() {
        let mut t = CoherenceThresholds::default();
        t.contract_consistency = 1.5;
        assert!(t.validate().is_err());
    }

    #[test]
    fn coherence_threshold_negative_rejected() {
        let mut t = CoherenceThresholds::default();
        t.mission_completion = -0.1;
        assert!(t.validate().is_err());
    }

    #[test]
    fn coherence_threshold_boundary_valid() {
        let mut t = CoherenceThresholds::default();
        t.contract_consistency = 0.0;
        t.integration_soundness = 1.0;
        assert!(t.validate().is_ok());
    }

    #[test]
    fn coherence_multiple_errors_aggregated() {
        let mut t = CoherenceThresholds::default();
        t.contract_consistency = 1.5;
        t.mission_completion = -0.1;
        let err = t.validate().unwrap_err().to_string();
        assert!(err.contains("contract_consistency"), "err: {err}");
        assert!(err.contains("mission_completion"), "err: {err}");
    }

    #[test]
    fn coherence_new_fields_validated() {
        let mut t = CoherenceThresholds::default();
        t.word_overlap_fallback = -0.1;
        assert!(t.validate().is_err());

        let mut t = CoherenceThresholds::default();
        t.critical_severity_weight = 1.5;
        assert!(t.validate().is_err());

        let mut t = CoherenceThresholds::default();
        t.major_severity_weight = 2.0;
        assert!(t.validate().is_err());
    }

    // ── EvidenceGatheringConfig ──

    #[test]
    fn gathering_default_validates() {
        EvidenceGatheringConfig::default().validate().unwrap();
    }

    #[test]
    fn gathering_convergence_threshold_out_of_range() {
        let mut g = EvidenceGatheringConfig::default();
        g.convergence_threshold = 1.5;
        assert!(g.validate().is_err());
    }

    #[test]
    fn gathering_cache_relevance_negative_rejected() {
        let mut g = EvidenceGatheringConfig::default();
        g.cache_relevance_threshold = -0.1;
        assert!(g.validate().is_err());
    }

    #[test]
    fn gathering_confidence_scores_out_of_range() {
        let mut g = EvidenceGatheringConfig::default();
        g.confidence.direct_path = 1.5;
        assert!(g.validate().is_err());
    }

    #[test]
    fn gathering_boundary_valid() {
        let mut g = EvidenceGatheringConfig::default();
        g.convergence_threshold = 0.0;
        g.cache_relevance_threshold = 1.0;
        g.plateau_threshold = 0.0;
        assert!(g.validate().is_ok());
    }

    #[test]
    fn gathering_multiple_errors_aggregated() {
        let mut g = EvidenceGatheringConfig::default();
        g.convergence_threshold = 1.5;
        g.cache_relevance_threshold = -0.1;
        let err = g.validate().unwrap_err().to_string();
        assert!(err.contains("convergence_threshold"), "err: {err}");
        assert!(err.contains("cache_relevance_threshold"), "err: {err}");
    }

    // ── EvidenceSufficiencyConfig ──

    #[test]
    fn sufficiency_default_validates() {
        EvidenceSufficiencyConfig::default().validate().unwrap();
    }

    #[test]
    fn sufficiency_min_coverage_ratio_out_of_range() {
        let mut s = EvidenceSufficiencyConfig::default();
        s.min_coverage_ratio = 1.5;
        assert!(s.validate().is_err());
    }

    #[test]
    fn sufficiency_delegates_to_gathering() {
        let mut s = EvidenceSufficiencyConfig::default();
        s.gathering.convergence_threshold = -0.1;
        assert!(s.validate().is_err());
    }

    // ── EvidenceGatheringConfig::effective_threshold ──

    #[test]
    fn effective_threshold_scaling() {
        let cfg = EvidenceGatheringConfig::default();
        use crate::planning::ComplexityTier;
        let trivial = cfg.effective_threshold(ComplexityTier::Trivial);
        let simple = cfg.effective_threshold(ComplexityTier::Simple);
        let complex = cfg.effective_threshold(ComplexityTier::Complex);
        assert!(trivial < simple, "trivial={trivial} should be < simple={simple}");
        assert!(simple < complex, "simple={simple} should be < complex={complex}");
    }

    #[test]
    fn effective_threshold_trivial_multiplier() {
        let cfg = EvidenceGatheringConfig::default();
        use crate::planning::ComplexityTier;
        let trivial = cfg.effective_threshold(ComplexityTier::Trivial);
        let expected = cfg.min_coverage_threshold * 0.7;
        assert!((trivial - expected).abs() < f32::EPSILON);
    }

    #[test]
    fn effective_threshold_complex_is_full() {
        let cfg = EvidenceGatheringConfig::default();
        use crate::planning::ComplexityTier;
        let complex = cfg.effective_threshold(ComplexityTier::Complex);
        assert!((complex - cfg.min_coverage_threshold).abs() < f32::EPSILON);
    }
}
