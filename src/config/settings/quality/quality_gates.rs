use serde::{Deserialize, Serialize};

use crate::error::{PilotError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityConfig {
    pub min_evidence_quality: f32,
    pub min_evidence_confidence: f32,
    pub require_verifiable_evidence: bool,
    pub verifiable_weight: f32,
    pub confidence_weight: f32,
    /// Evidence coverage threshold for "Sufficient" tier (0.5 = 50% coverage required)
    pub evidence_coverage_threshold: f32,
    /// Lower bound for "SufficientButRisky" tier (0.3 = 30%).
    /// Coverage between this and evidence_coverage_threshold triggers LLM assessment.
    pub risky_coverage_threshold: f32,
    /// Maximum items to display in validation warnings/errors.
    pub max_display_items: usize,
    /// Require Green tier evidence quality to proceed with planning.
    /// When true, Yellow tier evidence is rejected as an error.
    /// When false, Yellow tier produces a warning but proceeds.
    pub require_green_tier: bool,
}

impl QualityConfig {
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        if !(0.0..=1.0).contains(&self.min_evidence_quality) {
            errors.push("min_evidence_quality must be between 0.0 and 1.0");
        }
        if self.min_evidence_quality < 0.5 {
            errors.push("min_evidence_quality must be >= 0.5 (NON-NEGOTIABLE)");
        }
        if !(0.0..=1.0).contains(&self.min_evidence_confidence) {
            errors.push("min_evidence_confidence must be between 0.0 and 1.0");
        }
        if self.min_evidence_confidence < 0.3 {
            errors.push("min_evidence_confidence must be >= 0.3 (NON-NEGOTIABLE)");
        }
        if !self.require_verifiable_evidence {
            errors.push("require_verifiable_evidence must be true (NON-NEGOTIABLE)");
        }
        if !(0.0..=1.0).contains(&self.evidence_coverage_threshold) {
            errors.push("evidence_coverage_threshold must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.risky_coverage_threshold) {
            errors.push("risky_coverage_threshold must be between 0.0 and 1.0");
        }
        if self.risky_coverage_threshold >= self.evidence_coverage_threshold {
            errors.push("risky_coverage_threshold must be less than evidence_coverage_threshold");
        }
        {
            let weight_sum = self.verifiable_weight + self.confidence_weight;
            if (weight_sum - 1.0).abs() > 0.01 {
                errors.push("quality verifiable_weight + confidence_weight must sum to 1.0");
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "QualityConfig validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            min_evidence_quality: 0.6,
            min_evidence_confidence: 0.5,
            require_verifiable_evidence: true,
            verifiable_weight: 0.4,
            confidence_weight: 0.6,
            evidence_coverage_threshold: 0.5,
            risky_coverage_threshold: 0.3,
            max_display_items: 5,
            require_green_tier: true,
        }
    }
}
