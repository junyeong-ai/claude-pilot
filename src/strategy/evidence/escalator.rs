//! Evidence escalator that coordinates multi-level gathering.

use std::path::Path;

use tracing::{debug, info, warn};

use crate::config::QualityConfig;
use crate::error::Result;
use crate::planning::Evidence;

use super::{EvidenceResult, EvidenceStrategy, GatheringContext, GatheringOutcome};

/// Orchestrates evidence gathering across multiple strategy levels.
pub struct EvidenceEscalator {
    strategies: Vec<Box<dyn EvidenceStrategy>>,
    max_levels: u8,
    quality_config: QualityConfig,
}

impl EvidenceEscalator {
    pub fn new(quality_config: QualityConfig) -> Self {
        Self {
            strategies: Vec::new(),
            max_levels: 4,
            quality_config,
        }
    }

    pub fn with_max_levels(mut self, max: u8) -> Self {
        self.max_levels = max;
        self
    }

    pub fn add_strategy(mut self, strategy: Box<dyn EvidenceStrategy>) -> Self {
        self.strategies.push(strategy);
        self.strategies.sort_by_key(|s| s.level());
        self
    }

    pub fn register_strategy(&mut self, strategy: Box<dyn EvidenceStrategy>) {
        self.strategies.push(strategy);
        self.strategies.sort_by_key(|s| s.level());
    }

    /// Gather evidence with automatic escalation on insufficient quality.
    pub async fn gather(&self, description: &str, working_dir: &Path) -> Result<EvidenceResult> {
        let mut context = GatheringContext::new(description);

        for level in 0..self.max_levels {
            let strategy = match self.select_strategy(level, &context) {
                Some(s) => s,
                None => {
                    debug!(level, "No strategy available for level");
                    continue;
                }
            };

            info!(
                level,
                name = strategy.name(),
                "Attempting evidence gathering"
            );
            context.mark_level_attempted(level);

            let outcome = strategy.gather(&context, working_dir).await?;
            self.process_outcome(&mut context, outcome);

            // Check if we have sufficient evidence
            if let Some(evidence) = &context.accumulated_evidence {
                let quality_summary = evidence.validate_quality(&self.quality_config);
                let quality = quality_summary.quality_score;
                let required = self.quality_config.min_evidence_quality;

                if quality >= required {
                    if context.identified_gaps.is_empty() {
                        info!(quality, "Evidence gathering complete");
                        return Ok(EvidenceResult::Complete(evidence.clone()));
                    } else {
                        info!(
                            quality,
                            gaps = context.identified_gaps.len(),
                            "Evidence sufficient with gaps"
                        );
                        return Ok(EvidenceResult::Sufficient {
                            evidence: evidence.clone(),
                            gaps: context.identified_gaps.clone(),
                        });
                    }
                }

                debug!(
                    quality,
                    required, "Evidence quality insufficient, escalating"
                );
            }
        }

        // All levels exhausted - return what we have with human escalation context
        warn!("All evidence gathering levels exhausted");
        let partial_evidence = context
            .accumulated_evidence
            .clone()
            .unwrap_or_else(Evidence::empty);

        Ok(EvidenceResult::NeedsHuman {
            partial: partial_evidence,
            context: Box::new(context),
        })
    }

    fn select_strategy(
        &self,
        level: u8,
        context: &GatheringContext,
    ) -> Option<&dyn EvidenceStrategy> {
        // If we have gaps, prefer strategies that can address them
        if !context.identified_gaps.is_empty() {
            for strategy in &self.strategies {
                if strategy.level() == level
                    && context.gaps().iter().any(|g| strategy.can_address_gap(g))
                {
                    return Some(strategy.as_ref());
                }
            }
        }

        // Fall back to first strategy at this level
        self.strategies
            .iter()
            .find(|s| s.level() == level)
            .map(|s| s.as_ref())
    }

    fn process_outcome(&self, context: &mut GatheringContext, outcome: GatheringOutcome) {
        context.merge_evidence(outcome.evidence);
        context.add_gaps(outcome.remaining_gaps);
    }
}
