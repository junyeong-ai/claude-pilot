use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::info;

use crate::error::Result;

use super::tiers::ComplexityTier;

/// Recorded complexity example for calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityExample {
    pub description: String,
    pub estimated_tier: ComplexityTier,
    pub actual_tier: ComplexityTier,
    pub files_affected: usize,
    pub reasoning: String,
}

impl ComplexityExample {
    pub fn was_correct(&self) -> bool {
        self.estimated_tier == self.actual_tier
    }

    pub fn format_for_llm(&self) -> String {
        let tier_str = match self.actual_tier {
            ComplexityTier::Trivial => "TRIVIAL",
            ComplexityTier::Simple => "SIMPLE",
            ComplexityTier::Complex => "COMPLEX",
        };
        format!(
            "- \"{}\" → {} ({} files)",
            crate::utils::truncate_at_boundary(&self.description, 60),
            tier_str,
            self.files_affected
        )
    }
}

/// Stores and retrieves complexity examples for LLM calibration.
pub struct ComplexityExampleStore {
    examples_file: PathBuf,
    max_examples_per_tier: usize,
}

impl ComplexityExampleStore {
    const DEFAULT_MAX_EXAMPLES_PER_TIER: usize = 3;

    pub fn new(pilot_dir: &Path) -> Self {
        Self {
            examples_file: pilot_dir.join("complexity_examples.jsonl"),
            max_examples_per_tier: Self::DEFAULT_MAX_EXAMPLES_PER_TIER,
        }
    }

    pub fn with_max_examples(mut self, max_per_tier: usize) -> Self {
        self.max_examples_per_tier = max_per_tier;
        self
    }

    pub async fn load(&self) -> Vec<ComplexityExample> {
        if !self.examples_file.exists() {
            return Vec::new();
        }

        match fs::read_to_string(&self.examples_file).await {
            Ok(content) => content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn append(&self, example: &ComplexityExample) -> Result<()> {
        let mut content = if self.examples_file.exists() {
            fs::read_to_string(&self.examples_file)
                .await
                .unwrap_or_default()
        } else {
            String::new()
        };

        content.push_str(&serde_json::to_string(example)?);
        content.push('\n');

        fs::write(&self.examples_file, content).await?;
        info!(tier = ?example.actual_tier, "Recorded complexity example");
        Ok(())
    }

    pub fn select_examples<'a>(
        &self,
        examples: &'a [ComplexityExample],
    ) -> Vec<&'a ComplexityExample> {
        let mut trivial: Vec<_> = examples
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Trivial && e.was_correct())
            .collect();
        let mut simple: Vec<_> = examples
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Simple && e.was_correct())
            .collect();
        let mut complex: Vec<_> = examples
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Complex && e.was_correct())
            .collect();

        trivial.truncate(self.max_examples_per_tier);
        simple.truncate(self.max_examples_per_tier);
        complex.truncate(self.max_examples_per_tier);

        let mut selected = Vec::new();
        selected.extend(trivial);
        selected.extend(simple);
        selected.extend(complex);
        selected
    }

    pub fn format_examples_section(&self, examples: &[ComplexityExample]) -> String {
        let selected = self.select_examples(examples);
        if selected.is_empty() {
            return String::new();
        }

        let mut section = String::from("\n## Project-Specific Examples\n\n");

        let trivial: Vec<_> = selected
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Trivial)
            .collect();
        let simple: Vec<_> = selected
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Simple)
            .collect();
        let complex: Vec<_> = selected
            .iter()
            .filter(|e| e.actual_tier == ComplexityTier::Complex)
            .collect();

        if !trivial.is_empty() {
            section.push_str("**TRIVIAL** (from this project):\n");
            for ex in trivial {
                section.push_str(&ex.format_for_llm());
                section.push('\n');
            }
        }

        if !simple.is_empty() {
            section.push_str("\n**SIMPLE** (from this project):\n");
            for ex in simple {
                section.push_str(&ex.format_for_llm());
                section.push('\n');
            }
        }

        if !complex.is_empty() {
            section.push_str("\n**COMPLEX** (from this project):\n");
            for ex in complex {
                section.push_str(&ex.format_for_llm());
                section.push('\n');
            }
        }

        section
    }
}
