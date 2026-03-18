use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Complexity tier for routing tasks to appropriate execution paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityTier {
    /// Skip planning entirely. Single-line fixes, typos, trivial changes.
    Trivial,
    /// Direct execution with minimal planning. Focused changes with clear scope.
    Simple,
    /// Full planning with spec/plan/tasks. Multi-file, architectural decisions.
    Complex,
}

impl ComplexityTier {
    pub fn is_trivial(&self) -> bool {
        matches!(self, Self::Trivial)
    }

    pub fn is_simple(&self) -> bool {
        matches!(self, Self::Simple)
    }

    pub fn needs_planning(&self) -> bool {
        matches!(self, Self::Complex)
    }
}

impl PartialOrd for ComplexityTier {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ComplexityTier {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_val = match self {
            ComplexityTier::Trivial => 0,
            ComplexityTier::Simple => 1,
            ComplexityTier::Complex => 2,
        };
        let other_val = match other {
            ComplexityTier::Trivial => 0,
            ComplexityTier::Simple => 1,
            ComplexityTier::Complex => 2,
        };
        self_val.cmp(&other_val)
    }
}

/// Complexity gate result with 3-tier routing.
#[derive(Debug, Clone)]
pub struct ComplexityGate {
    pub tier: ComplexityTier,
    pub is_simple: bool,
    pub reasoning: String,
    pub evidence_signals: Option<EvidenceSignals>,
}

impl ComplexityGate {
    pub fn trivial(reasoning: impl Into<String>) -> Self {
        Self {
            tier: ComplexityTier::Trivial,
            is_simple: true,
            reasoning: reasoning.into(),
            evidence_signals: None,
        }
    }

    pub fn simple(reasoning: impl Into<String>) -> Self {
        Self {
            tier: ComplexityTier::Simple,
            is_simple: true,
            reasoning: reasoning.into(),
            evidence_signals: None,
        }
    }

    pub fn complex(reasoning: impl Into<String>) -> Self {
        Self {
            tier: ComplexityTier::Complex,
            is_simple: false,
            reasoning: reasoning.into(),
            evidence_signals: None,
        }
    }

    pub fn with_evidence(mut self, signals: EvidenceSignals) -> Self {
        self.evidence_signals = Some(signals);
        self
    }
}

/// Structured response for complexity gate assessment.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComplexityResult {
    pub tier: ComplexityTier,
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Evidence-based signals extracted for complexity assessment.
#[derive(Debug, Clone, Default)]
pub struct EvidenceSignals {
    pub relevant_file_count: usize,
    pub dependency_count: usize,
    pub avg_confidence: f32,
    pub has_similar_patterns: bool,
    pub file_scope: FileScope,
    pub workspace_type: Option<WorkspaceType>,
    pub distinct_top_dirs: usize,
    pub confidence_spread: f32,
    pub low_confidence_ratio: f32,
}

/// Confidence statistics for evidence quality assessment.
#[derive(Debug, Clone, Default)]
pub struct ConfidenceStats {
    pub min: f32,
    pub max: f32,
    pub median: f32,
    pub high_confidence_count: usize,
    pub low_confidence_count: usize,
}

impl ConfidenceStats {
    pub fn from_values(values: &[f32]) -> Self {
        if values.is_empty() {
            return Self::default();
        }

        let mut sorted: Vec<f32> = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let median = if sorted.len().is_multiple_of(2) {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };

        let high_confidence_count = values.iter().filter(|&&v| v >= 0.7).count();
        let low_confidence_count = values.iter().filter(|&&v| v < 0.5).count();

        Self {
            min,
            max,
            median,
            high_confidence_count,
            low_confidence_count,
        }
    }

    pub fn format_for_llm(&self) -> String {
        if self.min == 0.0 && self.max == 0.0 {
            return "no confidence data".to_string();
        }
        format!(
            "confidence {:.0}%-{:.0}%, median {:.0}%",
            self.min * 100.0,
            self.max * 100.0,
            self.median * 100.0
        )
    }
}

/// Complete file scope information for LLM complexity assessment.
#[derive(Debug, Clone, Default)]
pub struct FileScope {
    pub total_count: usize,
    pub all_paths: Vec<(String, f32)>,
    pub directory_distribution: Vec<(String, usize)>,
    pub confidence_stats: ConfidenceStats,
}

impl FileScope {
    const SHOW_ALL_THRESHOLD: usize = 20;

    pub fn from_paths_with_confidence(paths: &[(String, f32)]) -> Self {
        let total_count = paths.len();
        let confidences: Vec<f32> = paths.iter().map(|(_, c)| *c).collect();
        let confidence_stats = ConfidenceStats::from_values(&confidences);

        if total_count <= Self::SHOW_ALL_THRESHOLD {
            Self {
                total_count,
                all_paths: paths.to_vec(),
                directory_distribution: Vec::new(),
                confidence_stats,
            }
        } else {
            let mut dir_counts: HashMap<String, usize> = HashMap::new();
            for (path, _) in paths {
                let dir = Self::extract_top_level_dir(path);
                *dir_counts.entry(dir).or_insert(0) += 1;
            }

            let mut distribution: Vec<_> = dir_counts.into_iter().collect();
            distribution.sort_by(|a, b| b.1.cmp(&a.1));

            Self {
                total_count,
                all_paths: Vec::new(),
                directory_distribution: distribution,
                confidence_stats,
            }
        }
    }

    pub fn from_paths(paths: &[String]) -> Self {
        let with_confidence: Vec<(String, f32)> = paths.iter().map(|p| (p.clone(), 0.0)).collect();
        Self::from_paths_with_confidence(&with_confidence)
    }

    fn extract_top_level_dir(path: &str) -> String {
        path.split('/')
            .next()
            .map(|s| format!("{}/", s))
            .unwrap_or_else(|| path.to_string())
    }

    pub fn format_for_llm(&self) -> String {
        if self.total_count == 0 {
            return "No relevant files identified".to_string();
        }

        let conf_summary = self.confidence_stats.format_for_llm();

        if !self.all_paths.is_empty() {
            let paths: Vec<String> = self
                .all_paths
                .iter()
                .map(|(p, c)| {
                    if *c > 0.0 {
                        format!("{} ({:.0}%)", p, c * 100.0)
                    } else {
                        p.clone()
                    }
                })
                .collect();
            format!(
                "- Affected files ({} total, {}):\n  {}",
                self.total_count,
                conf_summary,
                paths.join("\n  ")
            )
        } else if !self.directory_distribution.is_empty() {
            let dist: Vec<String> = self
                .directory_distribution
                .iter()
                .map(|(dir, count)| format!("  {} ({} files)", dir, count))
                .collect();
            format!(
                "- Affected files ({} total, {}, distribution):\n{}",
                self.total_count,
                conf_summary,
                dist.join("\n")
            )
        } else {
            format!(
                "- Affected files: {} total ({})",
                self.total_count, conf_summary
            )
        }
    }
}

/// Workspace/monorepo types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceType {
    CargoWorkspace,
    NpmWorkspace,
    YarnWorkspace,
    PnpmWorkspace,
    NxMonorepo,
    Turborepo,
    Lerna,
    GradleMultiProject,
    MavenMultiModule,
    BazelBuck,
    GoWorkspace,
    Unknown,
}

impl WorkspaceType {
    pub fn detection_confidence(&self) -> WorkspaceDetectionConfidence {
        match self {
            Self::Unknown => WorkspaceDetectionConfidence::Heuristic,
            _ => WorkspaceDetectionConfidence::ManifestVerified,
        }
    }

    pub fn format_for_llm(&self) -> String {
        match self {
            Self::Unknown => {
                "Unknown monorepo structure (detected via directory patterns - package count may be inaccurate)".into()
            }
            wt => format!("{:?} workspace (verified via manifest file)", wt),
        }
    }
}

/// Confidence level for workspace/package detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDetectionConfidence {
    ManifestVerified,
    Heuristic,
}
