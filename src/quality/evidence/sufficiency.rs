use std::collections::HashMap;
use std::path::Path;

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::planning::Evidence;
use crate::quality::EvidenceSufficiencyConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub id: String,
    pub description: String,
    pub category: RequirementCategory,
    pub priority: RequirementPriority,
    pub satisfied_by: Vec<EvidenceMatcher>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementCategory {
    FileStructure,
    Dependencies,
    Patterns,
    TestCoverage,
    Documentation,
    Conventions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementPriority {
    Critical,
    High,
    Medium,
    Low,
}

impl RequirementPriority {
    pub fn weight(&self) -> f32 {
        match self {
            Self::Critical => 2.0,
            Self::High => 1.5,
            Self::Medium => 1.0,
            Self::Low => 0.5,
        }
    }
}

/// Matchers for verifying evidence requirements.
///
/// ## Language-Agnostic Matchers (Recommended)
/// - `FileExists`: Checks file paths (deterministic, universal)
/// - `PatternFound`: Checks code patterns (deterministic)
/// - `DependencyPresent`: Checks dependency names (deterministic)
/// - `SourceVerifiable`: Checks source attribution (deterministic)
/// - `ConfidenceAbove`: Checks confidence threshold (numeric, universal)
///
/// ## Limited Matchers (Use with Caution)
/// - `KeywordInAnalysis`: Searches LLM-generated text fields.
///   WARNING: This searches in `relevance`, `summary`, and `affected_areas`
///   which are LLM-generated and may be in ANY language. Only use for
///   language-neutral terms (file paths, technical terms, identifiers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EvidenceMatcher {
    FileExists {
        pattern: String,
    },
    PatternFound {
        pattern: String,
    },
    DependencyPresent {
        name: String,
    },
    SourceVerifiable,
    ConfidenceAbove {
        threshold: f32,
    },
    /// WARNING: Searches LLM-generated text (may be in any language).
    /// Prefer structured matchers for reliable cross-language matching.
    KeywordInAnalysis {
        keyword: String,
    },
}

impl EvidenceMatcher {
    pub fn matches(&self, evidence: &Evidence) -> bool {
        match self {
            Self::FileExists { pattern } => evidence
                .codebase_analysis
                .relevant_files
                .iter()
                .any(|f| f.path.contains(pattern) || glob_match(pattern, &f.path)),
            Self::PatternFound { pattern } => evidence
                .codebase_analysis
                .existing_patterns
                .iter()
                .any(|p: &String| p.contains(pattern)),
            Self::DependencyPresent { name } => evidence
                .dependency_analysis
                .current_dependencies
                .iter()
                .any(|d| d.name.contains(name)),
            Self::SourceVerifiable => evidence
                .codebase_analysis
                .relevant_files
                .iter()
                .any(|f| f.source.is_verifiable()),
            Self::ConfidenceAbove { threshold } => {
                let avg_confidence = if evidence.codebase_analysis.relevant_files.is_empty() {
                    0.0
                } else {
                    evidence
                        .codebase_analysis
                        .relevant_files
                        .iter()
                        .map(|f| f.confidence)
                        .sum::<f32>()
                        / evidence.codebase_analysis.relevant_files.len() as f32
                };
                avg_confidence >= *threshold
            }
            Self::KeywordInAnalysis { keyword } => {
                let lower_keyword = keyword.to_lowercase();
                // First check language-agnostic fields (file paths, pattern names)
                let in_paths = evidence
                    .codebase_analysis
                    .relevant_files
                    .iter()
                    .any(|f| f.path.to_lowercase().contains(&lower_keyword));
                let in_patterns = evidence
                    .codebase_analysis
                    .existing_patterns
                    .iter()
                    .any(|p| p.to_lowercase().contains(&lower_keyword));

                if in_paths || in_patterns {
                    return true;
                }

                // Fallback: check LLM-generated text fields (may be in any language)
                // Only reliable for language-neutral terms (identifiers, technical terms)
                evidence.codebase_analysis.relevant_files.iter().any(|f| {
                    f.relevance.to_lowercase().contains(&lower_keyword)
                        || f.summary.to_lowercase().contains(&lower_keyword)
                }) || evidence
                    .codebase_analysis
                    .affected_areas
                    .iter()
                    .any(|a: &String| a.to_lowercase().contains(&lower_keyword))
            }
        }
    }
}

/// Match a path against a glob pattern with substring fallback.
fn glob_match(pattern: &str, path: &str) -> bool {
    let path_normalized = path.replace('\\', "/");
    let pattern_normalized = pattern.replace('\\', "/");

    // No wildcards - simple substring match
    if !pattern_normalized.contains('*') && !pattern_normalized.contains('?') {
        return path_normalized.contains(&pattern_normalized);
    }

    // Use glob crate for proper pattern matching
    match Pattern::new(&pattern_normalized) {
        Ok(glob) => glob.matches(&path_normalized),
        Err(_) => path_normalized.contains(&pattern_normalized.replace('*', "")),
    }
}

/// Graduated sufficiency level for nuanced decision making.
/// Provides actionable guidance instead of binary pass/fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SufficiencyLevel {
    /// All requirements met, proceed with confidence
    Sufficient,
    /// Most requirements met, can proceed with caution
    PartiallyMet,
    /// Many requirements missing, may need more evidence
    Insufficient,
    /// Critical requirements missing, should not proceed
    Critical,
}

impl SufficiencyLevel {
    /// Determine level from coverage metrics.
    ///
    /// **Note**: These thresholds are guidance, not hard gates.
    /// For borderline cases, use `format_for_llm_decision()` to let LLM decide.
    pub fn from_coverage(
        weighted_coverage: f32,
        has_critical_missing: bool,
        low_confidence_ratio: f32,
    ) -> Self {
        if has_critical_missing {
            return Self::Critical;
        }

        // High coverage + high confidence = sufficient
        if weighted_coverage >= 0.9 && low_confidence_ratio < 0.2 {
            return Self::Sufficient;
        }

        // Good coverage = partially met
        if weighted_coverage >= 0.7 {
            return Self::PartiallyMet;
        }

        // Low coverage = insufficient
        if weighted_coverage >= 0.4 {
            return Self::Insufficient;
        }

        // Very low coverage = critical
        Self::Critical
    }

    /// Get recommendation for this level.
    pub fn recommendation(&self) -> &'static str {
        match self {
            Self::Sufficient => "Evidence is sufficient to proceed with implementation.",
            Self::PartiallyMet => "Proceed with caution - monitor for gaps during implementation.",
            Self::Insufficient => "Consider gathering additional evidence before proceeding.",
            Self::Critical => "Critical evidence missing - investigate before continuing.",
        }
    }

    /// Can implementation proceed at this level?
    pub fn can_proceed(&self) -> bool {
        matches!(self, Self::Sufficient | Self::PartiallyMet)
    }

    /// Check if this level is borderline and should defer to LLM judgment.
    ///
    /// Borderline cases: PartiallyMet or Insufficient with coverage 0.5-0.8.
    /// In these cases, context matters more than thresholds.
    pub fn is_borderline(&self) -> bool {
        matches!(self, Self::PartiallyMet | Self::Insufficient)
    }
}

/// Confidence distribution across evidence items.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfidenceDistribution {
    pub high_confidence_count: usize,
    pub medium_confidence_count: usize,
    pub low_confidence_count: usize,
    pub min_confidence: f32,
    pub max_confidence: f32,
    pub median_confidence: f32,
}

impl ConfidenceDistribution {
    pub fn from_values(values: &[f32]) -> Self {
        if values.is_empty() {
            return Self::default();
        }

        let high_confidence_count = values.iter().filter(|&&v| v >= 0.8).count();
        let medium_confidence_count = values.iter().filter(|&&v| (0.5..0.8).contains(&v)).count();
        let low_confidence_count = values.iter().filter(|&&v| v < 0.5).count();

        let mut sorted: Vec<f32> = values.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let median_confidence = if sorted.len().is_multiple_of(2) {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        } else {
            sorted[sorted.len() / 2]
        };

        Self {
            high_confidence_count,
            medium_confidence_count,
            low_confidence_count,
            min_confidence: sorted[0],
            max_confidence: sorted[sorted.len() - 1],
            median_confidence,
        }
    }

    pub fn low_confidence_ratio(&self) -> f32 {
        let total =
            self.high_confidence_count + self.medium_confidence_count + self.low_confidence_count;
        if total == 0 {
            return 0.0;
        }
        self.low_confidence_count as f32 / total as f32
    }
}

/// Gathering efficiency: tracks yield trend across iterations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatheringEfficiency {
    pub recent_yields: Vec<usize>,
    pub is_declining: bool,
}

impl GatheringEfficiency {
    pub fn from_yields(yields: &[usize]) -> Self {
        let recent: Vec<usize> = yields
            .iter()
            .rev()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        let is_declining = recent.len() >= 2 && recent.windows(2).all(|w| w[1] <= w[0]);

        Self {
            recent_yields: recent,
            is_declining,
        }
    }
}

/// Rich context for LLM sufficiency decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SufficiencyContext {
    pub level: SufficiencyLevel,
    pub coverage_ratio: f32,
    pub confidence_distribution: ConfidenceDistribution,
    pub gathering_efficiency: Option<GatheringEfficiency>,
}

impl SufficiencyContext {
    pub fn new(
        coverage_ratio: f32,
        confidence_values: &[f32],
        yield_history: Option<&[usize]>,
    ) -> Self {
        let confidence_distribution = ConfidenceDistribution::from_values(confidence_values);
        let gathering_efficiency = yield_history.map(GatheringEfficiency::from_yields);

        let level = SufficiencyLevel::from_coverage(
            coverage_ratio,
            false,
            confidence_distribution.low_confidence_ratio(),
        );

        Self {
            level,
            coverage_ratio,
            confidence_distribution,
            gathering_efficiency,
        }
    }

    pub fn format_for_llm(&self) -> String {
        let d = &self.confidence_distribution;
        let file_count =
            d.high_confidence_count + d.medium_confidence_count + d.low_confidence_count;

        let mut lines = vec![format!(
            "Coverage: {:.0}% | Files: {} ({}↑ {}↔ {}↓)",
            self.coverage_ratio * 100.0,
            file_count,
            d.high_confidence_count,
            d.medium_confidence_count,
            d.low_confidence_count
        )];

        if file_count > 0 {
            lines.push(format!(
                "Confidence: {:.0}%-{:.0}% (median {:.0}%)",
                d.min_confidence * 100.0,
                d.max_confidence * 100.0,
                d.median_confidence * 100.0
            ));
        }

        if let Some(ref eff) = self.gathering_efficiency {
            let trend = if eff.is_declining {
                "↓declining"
            } else {
                "→stable"
            };
            lines.push(format!("Yield: {:?} {}", eff.recent_yields, trend));
        }

        lines.push(format!("Level: {:?}", self.level));
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageResult {
    pub total_requirements: usize,
    pub satisfied_requirements: usize,
    pub coverage_ratio: f32,
    pub weighted_coverage: f32,
    /// Legacy boolean field - prefer `level` for nuanced decisions
    pub sufficient: bool,
    /// Graduated sufficiency level
    pub level: SufficiencyLevel,
    pub missing: Vec<MissingEvidence>,
    pub category_coverage: HashMap<RequirementCategory, CategoryCoverage>,
    /// Actionable recommendations based on coverage analysis
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryCoverage {
    pub total: usize,
    pub satisfied: usize,
    pub ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingEvidence {
    pub requirement_id: String,
    pub description: String,
    pub category: RequirementCategory,
    pub priority: RequirementPriority,
    pub suggestion: Option<String>,
}

impl MissingEvidence {
    fn from_requirement(req: &EvidenceRequirement) -> Self {
        let suggestion = match req.category {
            RequirementCategory::FileStructure => {
                Some("Run file discovery to find relevant source files".to_string())
            }
            RequirementCategory::Dependencies => {
                Some("Analyze package manifest (Cargo.toml, package.json, etc.)".to_string())
            }
            RequirementCategory::Patterns => {
                Some("Search for similar implementations in the codebase".to_string())
            }
            RequirementCategory::TestCoverage => {
                Some("Look for existing tests related to this feature".to_string())
            }
            RequirementCategory::Documentation => {
                Some("Search for README, docs, or inline documentation".to_string())
            }
            RequirementCategory::Conventions => {
                Some("Analyze coding style and conventions used in the project".to_string())
            }
        };

        Self {
            requirement_id: req.id.clone(),
            description: req.description.clone(),
            category: req.category,
            priority: req.priority,
            suggestion,
        }
    }
}

#[derive(Debug)]
pub struct InsufficientEvidenceError {
    pub coverage: f32,
    pub required: f32,
    pub missing: Vec<MissingEvidence>,
}

impl std::fmt::Display for InsufficientEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Insufficient evidence coverage: {:.1}% (required: {:.1}%). Missing {} items.",
            self.coverage * 100.0,
            self.required * 100.0,
            self.missing.len()
        )
    }
}

impl std::error::Error for InsufficientEvidenceError {}

pub struct EvidenceSufficiencyChecker {
    config: EvidenceSufficiencyConfig,
}

impl EvidenceSufficiencyChecker {
    pub fn new(config: EvidenceSufficiencyConfig) -> Self {
        Self { config }
    }

    pub fn check_coverage(
        &self,
        requirements: &[EvidenceRequirement],
        evidence: &Evidence,
    ) -> Result<CoverageResult, InsufficientEvidenceError> {
        let result = self.calculate_coverage(requirements, evidence);

        if !result.sufficient && self.config.halt_on_insufficient {
            return Err(InsufficientEvidenceError {
                coverage: result.coverage_ratio,
                required: self.config.min_coverage_ratio,
                missing: result.missing.clone(),
            });
        }

        Ok(result)
    }

    pub fn calculate_coverage(
        &self,
        requirements: &[EvidenceRequirement],
        evidence: &Evidence,
    ) -> CoverageResult {
        if requirements.is_empty() {
            return CoverageResult {
                total_requirements: 0,
                satisfied_requirements: 0,
                coverage_ratio: 1.0,
                weighted_coverage: 1.0,
                sufficient: true,
                level: SufficiencyLevel::Sufficient,
                missing: Vec::new(),
                category_coverage: HashMap::new(),
                recommendations: vec![],
            };
        }

        let mut satisfied = Vec::new();
        let mut missing = Vec::new();
        let mut total_weight = 0.0;
        let mut satisfied_weight = 0.0;
        let mut category_stats: HashMap<RequirementCategory, (usize, usize)> = HashMap::new();

        for req in requirements {
            let weight = req.priority.weight();
            total_weight += weight;

            let is_satisfied = req
                .satisfied_by
                .iter()
                .any(|matcher| matcher.matches(evidence));

            let entry = category_stats.entry(req.category).or_insert((0, 0));
            entry.0 += 1;

            if is_satisfied {
                satisfied.push(req.id.clone());
                satisfied_weight += weight;
                entry.1 += 1;
            } else {
                missing.push(MissingEvidence::from_requirement(req));
            }
        }

        let coverage_ratio = satisfied.len() as f32 / requirements.len() as f32;
        let weighted_coverage = if total_weight > 0.0 {
            satisfied_weight / total_weight
        } else {
            1.0
        };

        let category_coverage = category_stats
            .into_iter()
            .map(|(cat, (total, sat))| {
                (
                    cat,
                    CategoryCoverage {
                        total,
                        satisfied: sat,
                        ratio: if total > 0 {
                            sat as f32 / total as f32
                        } else {
                            1.0
                        },
                    },
                )
            })
            .collect();

        let has_critical_missing = missing
            .iter()
            .any(|m| m.priority == RequirementPriority::Critical);

        let sufficient =
            weighted_coverage >= self.config.min_coverage_ratio && !has_critical_missing;

        // Calculate low confidence ratio from evidence
        let low_confidence_ratio = 0.0; // Would need evidence param to calculate properly

        let level = SufficiencyLevel::from_coverage(
            weighted_coverage,
            has_critical_missing,
            low_confidence_ratio,
        );

        // Generate actionable recommendations based on missing evidence
        let recommendations = self.generate_recommendations(&missing, &category_coverage, level);

        CoverageResult {
            total_requirements: requirements.len(),
            satisfied_requirements: satisfied.len(),
            coverage_ratio,
            weighted_coverage,
            sufficient,
            level,
            missing,
            category_coverage,
            recommendations,
        }
    }

    /// Generate actionable recommendations based on coverage analysis.
    fn generate_recommendations(
        &self,
        missing: &[MissingEvidence],
        category_coverage: &HashMap<RequirementCategory, CategoryCoverage>,
        level: SufficiencyLevel,
    ) -> Vec<String> {
        let mut recommendations = Vec::new();

        // Add level-based recommendation
        recommendations.push(level.recommendation().to_string());

        // Prioritize critical missing items
        let critical_missing: Vec<_> = missing
            .iter()
            .filter(|m| m.priority == RequirementPriority::Critical)
            .collect();

        if !critical_missing.is_empty() {
            recommendations.push(format!(
                "Address {} critical missing items before proceeding: {}",
                critical_missing.len(),
                critical_missing
                    .iter()
                    .take(3)
                    .map(|m| m.requirement_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Identify weak categories
        for (cat, coverage) in category_coverage {
            if coverage.ratio < 0.5 && coverage.total > 0 {
                let suggestion = match cat {
                    RequirementCategory::FileStructure => {
                        "Try broader glob patterns or content search"
                    }
                    RequirementCategory::Dependencies => {
                        "Check package manifest files for dependencies"
                    }
                    RequirementCategory::Patterns => {
                        "Search for similar implementations in codebase"
                    }
                    RequirementCategory::TestCoverage => "Look for test directories and test files",
                    RequirementCategory::Documentation => "Search for README, docs, or comments",
                    RequirementCategory::Conventions => "Analyze existing code style and patterns",
                };
                recommendations.push(format!(
                    "{:?} coverage is low ({:.0}%): {}",
                    cat,
                    coverage.ratio * 100.0,
                    suggestion
                ));
            }
        }

        recommendations
    }

    /// Generate minimal universal requirements for evidence sufficiency.
    ///
    /// Semantic requirement generation is delegated to LLM in EvidenceGatheringLoop.
    /// This function only provides deterministic, universal requirements that apply
    /// to all missions regardless of domain.
    pub fn generate_requirements_from_mission(
        &self,
        _mission: &str,
        _working_dir: &Path,
    ) -> Vec<EvidenceRequirement> {
        vec![
            EvidenceRequirement {
                id: "file_structure".to_string(),
                description: "Relevant source files identified".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::High,
                satisfied_by: vec![EvidenceMatcher::SourceVerifiable],
            },
            EvidenceRequirement {
                id: "confidence_level".to_string(),
                description: "Evidence confidence above threshold".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::ConfidenceAbove {
                    threshold: self.config.gathering.convergence_threshold,
                }],
            },
        ]
    }

    pub fn format_coverage_report(&self, result: &CoverageResult) -> String {
        let mut report = String::new();

        report.push_str("## Evidence Coverage Report\n\n");
        report.push_str(&format!(
            "**Overall Coverage**: {:.1}% ({}/{})\n",
            result.coverage_ratio * 100.0,
            result.satisfied_requirements,
            result.total_requirements
        ));
        report.push_str(&format!(
            "**Weighted Coverage**: {:.1}%\n",
            result.weighted_coverage * 100.0
        ));
        report.push_str(&format!(
            "**Status**: {}\n\n",
            if result.sufficient {
                "✅ Sufficient"
            } else {
                "❌ Insufficient"
            }
        ));

        if !result.category_coverage.is_empty() {
            report.push_str("### Coverage by Category\n\n");
            for (category, coverage) in &result.category_coverage {
                report.push_str(&format!(
                    "- **{:?}**: {:.0}% ({}/{})\n",
                    category,
                    coverage.ratio * 100.0,
                    coverage.satisfied,
                    coverage.total
                ));
            }
            report.push('\n');
        }

        if !result.missing.is_empty() {
            report.push_str("### Missing Evidence\n\n");
            for missing in &result.missing {
                report.push_str(&format!(
                    "- **[{:?}]** {}: {}\n",
                    missing.priority, missing.requirement_id, missing.description
                ));
                if let Some(suggestion) = &missing.suggestion {
                    report.push_str(&format!("  - *Suggestion*: {}\n", suggestion));
                }
            }
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planning::{
        CodebaseAnalysis, DependencyAnalysis, EvidenceCompleteness, EvidenceSource, PriorKnowledge,
        RelevantFile, TestStructure,
    };
    use chrono::Utc;

    fn create_test_evidence() -> Evidence {
        Evidence {
            codebase_analysis: CodebaseAnalysis {
                relevant_files: vec![
                    RelevantFile {
                        path: "src/api/handler.rs".to_string(),
                        relevance: "API endpoint handlers".to_string(),
                        summary: "Contains HTTP request handlers".to_string(),
                        source: EvidenceSource::PatternDetection {
                            pattern: "api handler".to_string(),
                        },
                        confidence: 0.8,
                    },
                    RelevantFile {
                        path: "tests/api_tests.rs".to_string(),
                        relevance: "API tests".to_string(),
                        summary: "Integration tests for API".to_string(),
                        source: EvidenceSource::FilesystemStructure,
                        confidence: 0.9,
                    },
                ],
                existing_patterns: vec!["api routing".to_string(), "handler pattern".to_string()],
                conventions: vec!["use async/await".to_string()],
                affected_areas: vec!["src/api".to_string()],
                test_structure: TestStructure::default(),
            },
            dependency_analysis: DependencyAnalysis {
                current_dependencies: vec![],
                suggested_additions: vec![],
                version_constraints: vec![],
            },
            prior_knowledge: PriorKnowledge::default(),
            gathered_at: Utc::now(),
            completeness: EvidenceCompleteness::complete(),
        }
    }

    #[test]
    fn test_coverage_calculation() {
        let config = EvidenceSufficiencyConfig::default();
        let checker = EvidenceSufficiencyChecker::new(config);
        let evidence = create_test_evidence();

        let requirements = vec![
            EvidenceRequirement {
                id: "api_files".to_string(),
                description: "API source files".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::High,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "api".to_string(),
                }],
            },
            EvidenceRequirement {
                id: "test_files".to_string(),
                description: "Test files".to_string(),
                category: RequirementCategory::TestCoverage,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "test".to_string(),
                }],
            },
        ];

        let result = checker.calculate_coverage(&requirements, &evidence);
        assert_eq!(result.total_requirements, 2);
        assert_eq!(result.satisfied_requirements, 2);
        assert!(result.coverage_ratio >= 0.99);
        assert!(result.sufficient);
        assert!(result.missing.is_empty());
    }

    #[test]
    fn test_insufficient_coverage() {
        let config = EvidenceSufficiencyConfig {
            min_coverage_ratio: 0.8,
            halt_on_insufficient: true,
            ..Default::default()
        };
        let checker = EvidenceSufficiencyChecker::new(config);
        let evidence = create_test_evidence();

        let requirements = vec![
            EvidenceRequirement {
                id: "existing".to_string(),
                description: "Exists".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "api".to_string(),
                }],
            },
            EvidenceRequirement {
                id: "missing1".to_string(),
                description: "Missing 1".to_string(),
                category: RequirementCategory::Documentation,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "nonexistent".to_string(),
                }],
            },
            EvidenceRequirement {
                id: "missing2".to_string(),
                description: "Missing 2".to_string(),
                category: RequirementCategory::Documentation,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "alsonothere".to_string(),
                }],
            },
        ];

        let result = checker.check_coverage(&requirements, &evidence);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.missing.len(), 2);
    }

    #[test]
    fn test_critical_requirement_blocks_sufficiency() {
        let config = EvidenceSufficiencyConfig::default();
        let checker = EvidenceSufficiencyChecker::new(config);
        let evidence = create_test_evidence();

        let requirements = vec![
            EvidenceRequirement {
                id: "satisfied".to_string(),
                description: "Satisfied".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::Medium,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "api".to_string(),
                }],
            },
            EvidenceRequirement {
                id: "critical_missing".to_string(),
                description: "Critical missing".to_string(),
                category: RequirementCategory::FileStructure,
                priority: RequirementPriority::Critical,
                satisfied_by: vec![EvidenceMatcher::FileExists {
                    pattern: "required_critical".to_string(),
                }],
            },
        ];

        let result = checker.calculate_coverage(&requirements, &evidence);
        assert!(!result.sufficient);
    }

    #[test]
    fn test_empty_requirements() {
        let config = EvidenceSufficiencyConfig::default();
        let checker = EvidenceSufficiencyChecker::new(config);
        let evidence = create_test_evidence();

        let result = checker.calculate_coverage(&[], &evidence);
        assert!(result.sufficient);
        assert_eq!(result.coverage_ratio, 1.0);
    }

    #[test]
    fn test_evidence_matcher_keyword() {
        let evidence = create_test_evidence();
        let matcher = EvidenceMatcher::KeywordInAnalysis {
            keyword: "handler".to_string(),
        };
        assert!(matcher.matches(&evidence));

        let matcher = EvidenceMatcher::KeywordInAnalysis {
            keyword: "nonexistent".to_string(),
        };
        assert!(!matcher.matches(&evidence));
    }

    #[test]
    fn test_evidence_matcher_pattern() {
        let evidence = create_test_evidence();
        let matcher = EvidenceMatcher::PatternFound {
            pattern: "routing".to_string(),
        };
        assert!(matcher.matches(&evidence));
    }
}
