use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{EvidenceConfig, QualityConfig};
use crate::utils::estimate_tokens;

pub use super::validation::QualityTier;

#[derive(Debug, Clone)]
pub struct QualityAssessment {
    pub tier: QualityTier,
    pub quality_score: f32,
    pub confidence: f32,
    pub verifiable_items: usize,
    pub total_items: usize,
    pub has_verifiable: bool,
}

impl QualityAssessment {
    pub fn format_for_llm(&self) -> String {
        let tier_desc = match self.tier {
            QualityTier::Green => "HIGH quality - proceed normally",
            QualityTier::Yellow => "MODERATE quality - proceed with caution",
            QualityTier::Red => "INSUFFICIENT - high risk",
        };
        format!(
            "{} (score: {:.0}%, confidence: {:.0}%, verifiable: {}/{})",
            tier_desc,
            self.quality_score * 100.0,
            self.confidence * 100.0,
            self.verifiable_items,
            self.total_items
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub codebase_analysis: CodebaseAnalysis,
    pub dependency_analysis: DependencyAnalysis,
    pub prior_knowledge: PriorKnowledge,
    pub gathered_at: DateTime<Utc>,
    /// Indicates whether evidence gathering was complete or truncated.
    /// Planning should fail or warn if evidence is incomplete for complex tasks.
    #[serde(default)]
    pub completeness: EvidenceCompleteness,
}

/// Tracks whether evidence gathering was complete or had to truncate results.
/// This enables validation to detect when plans are based on incomplete analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceCompleteness {
    /// True if all evidence was gathered without truncation.
    pub is_complete: bool,
    /// Number of files found before truncation (if truncated).
    pub files_found: usize,
    /// Maximum files allowed (from config).
    pub files_limit: usize,
    /// Warnings about potential incompleteness.
    pub warnings: Vec<String>,
}

impl EvidenceCompleteness {
    pub fn complete() -> Self {
        Self {
            is_complete: true,
            files_found: 0,
            files_limit: 0,
            warnings: Vec::new(),
        }
    }

    pub fn truncated(found: usize, limit: usize, warning: impl Into<String>) -> Self {
        Self {
            is_complete: false,
            files_found: found,
            files_limit: limit,
            warnings: vec![warning.into()],
        }
    }

    pub fn add_warning(&mut self, warning: impl Into<String>) {
        self.is_complete = false;
        self.warnings.push(warning.into());
    }

    pub fn format_for_llm(&self) -> Option<String> {
        if self.is_complete {
            return None;
        }

        let coverage_pct = if self.files_limit > 0 {
            (self.files_found as f32 / self.files_limit as f32 * 100.0).min(100.0)
        } else {
            100.0
        };

        Some(format!(
            "⚠️ INCOMPLETE EVIDENCE ({:.0}% coverage): Only {} of {} files analyzed. \
             Proceed with caution - verify assumptions and be conservative with changes. \
             Issues: {}",
            coverage_pct,
            self.files_found,
            self.files_limit,
            self.warnings.join("; ")
        ))
    }
}

/// Evidence sufficiency classification for planning decisions.
/// Provides clear, quantifiable completion criteria instead of boolean checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceSufficiency {
    /// All required evidence gathered with high confidence.
    Complete,
    /// Evidence meets minimum threshold but has known gaps.
    Sufficient {
        /// Coverage ratio (0.0-1.0) of required evidence categories.
        coverage: f32,
        /// List of missing or incomplete evidence areas.
        missing: Vec<String>,
    },
    /// Evidence is in a risky zone - LLM should decide based on task complexity.
    /// Simple tasks may proceed; complex tasks should gather more evidence.
    SufficientButRisky {
        /// Current coverage ratio (0.0-1.0).
        coverage: f32,
        /// List of missing or incomplete evidence areas.
        missing: Vec<String>,
        /// Context about risk factors for LLM decision.
        risk_context: String,
    },
    /// Evidence does not meet minimum requirements; planning should halt.
    Insufficient {
        /// Specific reason why evidence is insufficient.
        reason: String,
        /// Current coverage ratio (0.0-1.0).
        coverage: f32,
    },
}

impl EvidenceSufficiency {
    /// Check if evidence is good enough to proceed with planning.
    /// Note: SufficientButRisky returns true - LLM decides based on task complexity.
    pub fn allows_planning(&self) -> bool {
        !matches!(self, Self::Insufficient { .. })
    }

    /// Check if this tier requires LLM assessment to proceed.
    pub fn requires_llm_assessment(&self) -> bool {
        matches!(self, Self::SufficientButRisky { .. })
    }

    /// Get coverage ratio for logging/display.
    pub fn coverage(&self) -> f32 {
        match self {
            Self::Complete => 1.0,
            Self::Sufficient { coverage, .. } => *coverage,
            Self::SufficientButRisky { coverage, .. } => *coverage,
            Self::Insufficient { coverage, .. } => *coverage,
        }
    }

    /// Get human-readable description.
    pub fn description(&self) -> String {
        match self {
            Self::Complete => "Complete - all required evidence gathered".to_string(),
            Self::Sufficient { coverage, missing } => {
                let missing_str = if missing.is_empty() {
                    String::new()
                } else {
                    format!(" (missing: {})", missing.join(", "))
                };
                format!(
                    "Sufficient - {:.0}% coverage{}",
                    coverage * 100.0,
                    missing_str
                )
            }
            Self::SufficientButRisky {
                coverage,
                missing,
                risk_context,
            } => {
                let missing_str = if missing.is_empty() {
                    String::new()
                } else {
                    format!(" (gaps: {})", missing.join(", "))
                };
                format!(
                    "Risky - {:.0}% coverage{}: {}",
                    coverage * 100.0,
                    missing_str,
                    risk_context
                )
            }
            Self::Insufficient { reason, coverage } => {
                format!(
                    "Insufficient - {:.0}% coverage: {}",
                    coverage * 100.0,
                    reason
                )
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodebaseAnalysis {
    pub relevant_files: Vec<RelevantFile>,
    pub existing_patterns: Vec<String>,
    pub conventions: Vec<String>,
    pub affected_areas: Vec<String>,
    /// Detected test structure for test pattern inference
    #[serde(default)]
    pub test_structure: TestStructure,
}

/// Detected test structure for inferring test patterns during task decomposition.
/// Used to provide examples and enable fallback inference when LLM returns empty patterns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestStructure {
    /// Test file naming patterns (e.g., "test_*.py", "*_test.rs", "*.spec.ts")
    pub file_patterns: Vec<String>,
    /// Test directories (e.g., "tests/", "__tests__/", "spec/")
    pub test_directories: Vec<String>,
    /// Module-to-test file mapping examples for inference
    pub module_test_mapping: Vec<ModuleTestMapping>,
    /// Detected test framework (e.g., "pytest", "jest", "cargo test")
    pub framework: Option<String>,
}

/// Maps a source module to its corresponding test file(s) for pattern inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleTestMapping {
    pub source_file: String,
    pub test_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelevantFile {
    pub path: String,
    pub relevance: String,
    pub summary: String,
    /// Source of evidence (how this file was found)
    pub source: EvidenceSource,
    /// Confidence level in this evidence (0.0 - 1.0)
    pub confidence: f32,
}

/// Tracks the source of evidence for fact validation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum EvidenceSource {
    /// Found via pattern detection
    PatternDetection { pattern: String },
    /// Found from git history
    GitHistory { commit: Option<String> },
    /// Found from dependency analysis
    DependencyAnalysis,
    /// Found from filesystem structure
    FilesystemStructure,
    /// From prior knowledge (skills/rules)
    PriorKnowledge { source_path: String },
    /// User provided
    UserProvided,

    // === LSP-based sources (high confidence - semantic analysis) ===
    /// Found via LSP go-to-definition
    LspDefinition {
        location: String,
        target_file: String,
        target_line: u32,
    },
    /// Found via LSP find-references
    LspReferences {
        symbol: String,
        reference_count: usize,
    },
    /// Found via LSP call hierarchy (incoming/outgoing calls)
    LspCallHierarchy {
        symbol: String,
        direction: CallDirection,
        call_count: usize,
    },
    /// Found via LSP document/workspace symbol search
    LspSymbolSearch {
        query: String,
        symbol_kind: Option<String>,
    },
    /// Found via LSP hover (type info, docs)
    LspHover { location: String },
    /// Found via LSP diagnostics
    LspDiagnostics {
        file: String,
        error_count: usize,
        warning_count: usize,
    },
    /// Found via LSP context (combined callers, callees, types, tests)
    LspContext { location: String },

    // === AST-based sources (high confidence - structural analysis) ===
    /// Found via tree-sitter AST pattern search
    AstSearch {
        pattern: String,
        language: String,
        match_count: usize,
    },

    #[default]
    /// Unknown source
    Unknown,
}

/// Direction of call hierarchy analysis
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Incoming,
    Outgoing,
}

impl EvidenceSource {
    /// Get a string description of the source for validation
    pub fn description(&self) -> String {
        match self {
            Self::PatternDetection { pattern } => format!("pattern detection: {}", pattern),
            Self::GitHistory { commit } => match commit {
                Some(c) => format!("git history (commit: {})", &c[..8.min(c.len())]),
                None => "git history".to_string(),
            },
            Self::DependencyAnalysis => "dependency analysis".to_string(),
            Self::FilesystemStructure => "filesystem structure".to_string(),
            Self::PriorKnowledge { source_path } => format!("prior knowledge: {}", source_path),
            Self::UserProvided => "user provided".to_string(),

            // LSP-based sources
            Self::LspDefinition {
                target_file,
                target_line,
                ..
            } => {
                format!("LSP definition: {}:{}", target_file, target_line)
            }
            Self::LspReferences {
                symbol,
                reference_count,
            } => {
                format!("LSP references: {} ({} refs)", symbol, reference_count)
            }
            Self::LspCallHierarchy {
                symbol,
                direction,
                call_count,
            } => {
                let dir = match direction {
                    CallDirection::Incoming => "callers",
                    CallDirection::Outgoing => "callees",
                };
                format!("LSP {}: {} ({} calls)", dir, symbol, call_count)
            }
            Self::LspSymbolSearch { query, symbol_kind } => match symbol_kind {
                Some(kind) => format!("LSP symbol search: '{}' ({})", query, kind),
                None => format!("LSP symbol search: '{}'", query),
            },
            Self::LspHover { location } => format!("LSP hover: {}", location),
            Self::LspDiagnostics {
                file,
                error_count,
                warning_count,
            } => {
                format!(
                    "LSP diagnostics: {} ({} errors, {} warnings)",
                    file, error_count, warning_count
                )
            }
            Self::LspContext { location } => format!("LSP context: {}", location),

            // AST-based sources
            Self::AstSearch {
                pattern,
                language,
                match_count,
            } => {
                format!(
                    "AST search: '{}' in {} ({} matches)",
                    pattern, language, match_count
                )
            }

            Self::Unknown => "unknown".to_string(),
        }
    }

    /// Check if this source is verifiable
    pub fn is_verifiable(&self) -> bool {
        !matches!(self, Self::Unknown | Self::UserProvided)
    }

    /// Check if this source is LSP-based (semantically verified)
    pub fn is_lsp_based(&self) -> bool {
        matches!(
            self,
            Self::LspDefinition { .. }
                | Self::LspReferences { .. }
                | Self::LspCallHierarchy { .. }
                | Self::LspSymbolSearch { .. }
                | Self::LspHover { .. }
                | Self::LspDiagnostics { .. }
                | Self::LspContext { .. }
        )
    }

    /// Check if this source is AST-based (structurally verified)
    pub fn is_ast_based(&self) -> bool {
        matches!(self, Self::AstSearch { .. })
    }

    /// Check if this source is semantically verified (LSP or AST)
    pub fn is_semantic(&self) -> bool {
        self.is_lsp_based() || self.is_ast_based()
    }

    /// Get confidence category for this source type.
    ///
    /// Returns categorical level rather than arbitrary numeric value.
    /// LLM should use this category with project context for calibration.
    pub fn confidence_category(&self) -> SourceConfidenceCategory {
        match self {
            // Semantic: Compiler/language-server verified
            Self::LspDefinition { .. }
            | Self::LspReferences { .. }
            | Self::LspCallHierarchy { .. }
            | Self::LspDiagnostics { .. }
            | Self::LspContext { .. }
            | Self::AstSearch { .. } => SourceConfidenceCategory::Semantic,

            // ToolVerified: Verified by external tools
            Self::LspSymbolSearch { .. }
            | Self::LspHover { .. }
            | Self::GitHistory { .. }
            | Self::DependencyAnalysis => SourceConfidenceCategory::ToolVerified,

            // Heuristic: Pattern/structure-based detection
            Self::FilesystemStructure
            | Self::PatternDetection { .. }
            | Self::PriorKnowledge { .. } => SourceConfidenceCategory::Heuristic,

            // Unverified: No automated verification
            Self::UserProvided | Self::Unknown => SourceConfidenceCategory::Unverified,
        }
    }

    /// Format confidence information for LLM context.
    ///
    /// Provides category and factors so LLM can calibrate based on project context.
    /// Uses categorical labels instead of arbitrary numeric values.
    pub fn format_confidence_for_llm(&self) -> String {
        let category = self.confidence_category();
        format!(
            "[{} - {} confidence] {}",
            category.label(),
            category.level_hint(),
            self.description()
        )
    }
}

/// Confidence categories for evidence sources.
///
/// Categorical rather than numeric - LLM calibrates with project context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceConfidenceCategory {
    /// Semantically verified by compiler/language-server (highest reliability)
    Semantic,
    /// Verified by external tools (git, package manager)
    ToolVerified,
    /// Pattern/heuristic-based detection (project-dependent reliability)
    Heuristic,
    /// No automated verification (requires manual validation)
    Unverified,
}

impl SourceConfidenceCategory {
    /// Human-readable label for the category
    pub fn label(&self) -> &'static str {
        match self {
            Self::Semantic => "SEMANTIC",
            Self::ToolVerified => "TOOL-VERIFIED",
            Self::Heuristic => "HEURISTIC",
            Self::Unverified => "UNVERIFIED",
        }
    }

    /// Qualitative level hint for LLM
    pub fn level_hint(&self) -> &'static str {
        match self {
            Self::Semantic => "high",
            Self::ToolVerified => "moderate-high",
            Self::Heuristic => "moderate",
            Self::Unverified => "low",
        }
    }

    /// Format for LLM with calibration guidance
    pub fn format_for_llm(&self) -> String {
        match self {
            Self::Semantic => {
                "Compiler/language-server verified. Highly reliable unless LSP has known issues."
                    .into()
            }
            Self::ToolVerified => {
                "Verified by external tools. Reliable for well-maintained projects.".into()
            }
            Self::Heuristic => {
                "Pattern-based detection. Reliability varies by project conventions.".into()
            }
            Self::Unverified => {
                "No automated verification. Cross-check with other sources recommended.".into()
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DependencyAnalysis {
    pub current_dependencies: Vec<Dependency>,
    pub suggested_additions: Vec<SuggestedDependency>,
    pub version_constraints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedDependency {
    pub name: String,
    pub reason: String,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PriorKnowledge {
    pub relevant_skills: Vec<FoundSkill>,
    pub relevant_rules: Vec<FoundRule>,
    pub similar_patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundSkill {
    pub name: String,
    pub path: String,
    pub relevance: String,
    /// Source of how this skill was found
    pub source: EvidenceSource,
    /// Confidence in this skill's relevance (0.0 - 1.0)
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundRule {
    pub name: String,
    pub path: String,
    pub relevance: String,
    /// Source of how this rule was found
    pub source: EvidenceSource,
    /// Confidence in this rule's relevance (0.0 - 1.0)
    pub confidence: f32,
}

/// Summary of evidence quality for validation
#[derive(Debug, Clone)]
pub struct EvidenceQualitySummary {
    pub total_evidence_items: usize,
    pub verifiable_items: usize,
    pub average_confidence: f32,
    pub source_breakdown: Vec<(String, usize)>,
    pub low_confidence_items: Vec<String>,
    pub quality_score: f32,
}

/// Trait for evidence items that can be quality-validated.
trait EvidenceItem {
    fn source(&self) -> &EvidenceSource;
    fn confidence(&self) -> f32;
    fn display_name(&self) -> String;
}

impl EvidenceItem for RelevantFile {
    fn source(&self) -> &EvidenceSource {
        &self.source
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn display_name(&self) -> String {
        format!("File: {}", self.path)
    }
}

impl EvidenceItem for FoundSkill {
    fn source(&self) -> &EvidenceSource {
        &self.source
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn display_name(&self) -> String {
        format!("Skill: {}", self.name)
    }
}

impl EvidenceItem for FoundRule {
    fn source(&self) -> &EvidenceSource {
        &self.source
    }
    fn confidence(&self) -> f32 {
        self.confidence
    }
    fn display_name(&self) -> String {
        format!("Rule: {}", self.name)
    }
}

impl Evidence {
    /// Create an empty evidence instance.
    pub fn empty() -> Self {
        Self {
            codebase_analysis: CodebaseAnalysis::default(),
            dependency_analysis: DependencyAnalysis::default(),
            prior_knowledge: PriorKnowledge::default(),
            gathered_at: Utc::now(),
            completeness: EvidenceCompleteness::complete(),
        }
    }

    /// Merge another evidence into this one.
    pub fn merge(&mut self, other: Evidence) {
        // Merge relevant files (deduplicate by path)
        for file in other.codebase_analysis.relevant_files {
            if !self
                .codebase_analysis
                .relevant_files
                .iter()
                .any(|f| f.path == file.path)
            {
                self.codebase_analysis.relevant_files.push(file);
            }
        }

        // Merge patterns (deduplicate)
        for pattern in other.codebase_analysis.existing_patterns {
            if !self.codebase_analysis.existing_patterns.contains(&pattern) {
                self.codebase_analysis.existing_patterns.push(pattern);
            }
        }

        // Merge conventions
        for convention in other.codebase_analysis.conventions {
            if !self.codebase_analysis.conventions.contains(&convention) {
                self.codebase_analysis.conventions.push(convention);
            }
        }

        // Merge affected areas
        for area in other.codebase_analysis.affected_areas {
            if !self.codebase_analysis.affected_areas.contains(&area) {
                self.codebase_analysis.affected_areas.push(area);
            }
        }

        // Merge dependencies
        for dep in other.dependency_analysis.current_dependencies {
            if !self
                .dependency_analysis
                .current_dependencies
                .iter()
                .any(|d| d.name == dep.name)
            {
                self.dependency_analysis.current_dependencies.push(dep);
            }
        }

        // Merge prior knowledge
        for skill in other.prior_knowledge.relevant_skills {
            if !self
                .prior_knowledge
                .relevant_skills
                .iter()
                .any(|s| s.path == skill.path)
            {
                self.prior_knowledge.relevant_skills.push(skill);
            }
        }

        for rule in other.prior_knowledge.relevant_rules {
            if !self
                .prior_knowledge
                .relevant_rules
                .iter()
                .any(|r| r.path == rule.path)
            {
                self.prior_knowledge.relevant_rules.push(rule);
            }
        }

        // Merge completeness (worst case)
        if !other.completeness.is_complete {
            self.completeness.is_complete = false;
        }
        self.completeness
            .warnings
            .extend(other.completeness.warnings);
    }

    pub fn validate_quality(&self, config: &QualityConfig) -> EvidenceQualitySummary {
        let mut total_items = 0;
        let mut verifiable = 0;
        let mut confidence_sum = 0.0;
        let mut source_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut low_confidence = Vec::new();

        fn process_item<T: EvidenceItem>(
            item: &T,
            threshold: f32,
            total: &mut usize,
            verifiable: &mut usize,
            confidence_sum: &mut f32,
            source_counts: &mut std::collections::HashMap<String, usize>,
            low_confidence: &mut Vec<String>,
        ) {
            *total += 1;
            if item.source().is_verifiable() {
                *verifiable += 1;
            }
            *confidence_sum += item.confidence();
            if item.confidence() < threshold {
                low_confidence.push(format!(
                    "{} (confidence: {:.2})",
                    item.display_name(),
                    item.confidence()
                ));
            }
            *source_counts
                .entry(item.source().description())
                .or_insert(0) += 1;
        }

        for item in &self.codebase_analysis.relevant_files {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }
        for item in &self.prior_knowledge.relevant_skills {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }
        for item in &self.prior_knowledge.relevant_rules {
            process_item(
                item,
                config.min_evidence_confidence,
                &mut total_items,
                &mut verifiable,
                &mut confidence_sum,
                &mut source_counts,
                &mut low_confidence,
            );
        }

        let average_confidence = if total_items > 0 {
            confidence_sum / total_items as f32
        } else {
            0.0
        };
        let verifiable_ratio = if total_items > 0 {
            verifiable as f32 / total_items as f32
        } else {
            0.0
        };

        // Calculate semantic source quality bonus.
        // LSP and AST sources provide higher-quality evidence than pattern matching.
        let semantic_source_count = self
            .codebase_analysis
            .relevant_files
            .iter()
            .filter(|f| f.source.is_semantic())
            .count();
        let semantic_ratio = if total_items > 0 {
            semantic_source_count as f32 / total_items as f32
        } else {
            0.0
        };

        // Quality score formula:
        // - Base: weighted combination of verifiable ratio and confidence
        // - Bonus: up to 10% for having semantic (LSP/AST) sources
        let base_score = (verifiable_ratio * config.verifiable_weight)
            + (average_confidence * config.confidence_weight);
        let semantic_bonus = semantic_ratio * 0.1;
        let quality_score = (base_score + semantic_bonus).min(1.0);

        let mut source_breakdown: Vec<(String, usize)> = source_counts.into_iter().collect();
        source_breakdown.sort_by(|a, b| b.1.cmp(&a.1));

        EvidenceQualitySummary {
            total_evidence_items: total_items,
            verifiable_items: verifiable,
            average_confidence,
            source_breakdown,
            low_confidence_items: low_confidence,
            quality_score,
        }
    }

    pub fn get_quality_tier(&self, config: &QualityConfig) -> QualityAssessment {
        let summary = self.validate_quality(config);

        // Yellow threshold = 70% of configured minimum
        let yellow_quality = config.min_evidence_quality * 0.7;
        let yellow_confidence = config.min_evidence_confidence * 0.7;

        let tier = if summary.quality_score >= config.min_evidence_quality
            && summary.average_confidence >= config.min_evidence_confidence
        {
            QualityTier::Green
        } else if summary.quality_score >= yellow_quality
            && summary.average_confidence >= yellow_confidence
        {
            QualityTier::Yellow
        } else {
            QualityTier::Red
        };

        let has_verifiable = summary.verifiable_items > 0;

        QualityAssessment {
            tier,
            quality_score: summary.quality_score,
            confidence: summary.average_confidence,
            verifiable_items: summary.verifiable_items,
            total_items: summary.total_evidence_items,
            has_verifiable,
        }
    }

    /// Validate evidence sufficiency with granular classification.
    ///
    /// Returns `EvidenceSufficiency` instead of boolean for clear, quantifiable
    /// completion criteria as required by NON-NEGOTIABLE fact-based development.
    pub fn validate_sufficiency(&self, config: &QualityConfig) -> EvidenceSufficiency {
        let summary = self.validate_quality(config);
        let mut missing = Vec::new();

        // Calculate coverage based on multiple factors
        let mut coverage_factors = 0;
        let mut satisfied_factors = 0;

        // Factor 1: Quality score meets threshold
        coverage_factors += 1;
        if summary.quality_score >= config.min_evidence_quality {
            satisfied_factors += 1;
        } else {
            missing.push(format!(
                "quality score {:.0}% < {:.0}%",
                summary.quality_score * 100.0,
                config.min_evidence_quality * 100.0
            ));
        }

        // Factor 2: Average confidence meets threshold
        coverage_factors += 1;
        if summary.average_confidence >= config.min_evidence_confidence {
            satisfied_factors += 1;
        } else {
            missing.push(format!(
                "avg confidence {:.0}% < {:.0}%",
                summary.average_confidence * 100.0,
                config.min_evidence_confidence * 100.0
            ));
        }

        // Factor 3: Has verifiable evidence (if required)
        if config.require_verifiable_evidence {
            coverage_factors += 1;
            if summary.verifiable_items > 0 {
                satisfied_factors += 1;
            } else {
                missing.push("no verifiable evidence".to_string());
            }
        }

        // Factor 4: Has relevant files
        coverage_factors += 1;
        if !self.codebase_analysis.relevant_files.is_empty() {
            satisfied_factors += 1;
        } else {
            missing.push("no relevant files found".to_string());
        }

        // Factor 5: Evidence completeness (no truncation warnings)
        coverage_factors += 1;
        if self.completeness.is_complete {
            satisfied_factors += 1;
        } else if !self.completeness.warnings.is_empty() {
            missing.push(format!(
                "truncated: {}",
                self.completeness.warnings.first().unwrap_or(&String::new())
            ));
        }

        let coverage = satisfied_factors as f32 / coverage_factors as f32;

        // Classification logic with three-tier system:
        // - Complete: 100% coverage, no gaps
        // - Sufficient: >= evidence_coverage_threshold (default 50%)
        // - SufficientButRisky: >= risky_coverage_threshold but < evidence_coverage_threshold
        // - Insufficient: < risky_coverage_threshold (default 30%)
        if missing.is_empty() && coverage >= 1.0 {
            EvidenceSufficiency::Complete
        } else if coverage >= config.evidence_coverage_threshold {
            EvidenceSufficiency::Sufficient { coverage, missing }
        } else if coverage >= config.risky_coverage_threshold {
            let risk_context = if missing.is_empty() {
                format!(
                    "Coverage {:.0}% is between {:.0}%-{:.0}% risky zone",
                    coverage * 100.0,
                    config.risky_coverage_threshold * 100.0,
                    config.evidence_coverage_threshold * 100.0
                )
            } else {
                format!(
                    "Coverage {:.0}% with gaps: {}",
                    coverage * 100.0,
                    missing.join(", ")
                )
            };
            EvidenceSufficiency::SufficientButRisky {
                coverage,
                missing,
                risk_context,
            }
        } else {
            let reason = if missing.is_empty() {
                format!(
                    "coverage {:.0}% below minimum {:.0}%",
                    coverage * 100.0,
                    config.risky_coverage_threshold * 100.0
                )
            } else {
                missing.join("; ")
            };
            EvidenceSufficiency::Insufficient { reason, coverage }
        }
    }

    /// Get list of all evidence sources for traceability
    pub fn all_sources(&self) -> Vec<String> {
        let mut sources = Vec::new();

        for file in &self.codebase_analysis.relevant_files {
            sources.push(format!(
                "File '{}' via {}",
                file.path,
                file.source.description()
            ));
        }

        for skill in &self.prior_knowledge.relevant_skills {
            sources.push(format!(
                "Skill '{}' via {}",
                skill.name,
                skill.source.description()
            ));
        }

        for rule in &self.prior_knowledge.relevant_rules {
            sources.push(format!(
                "Rule '{}' via {}",
                rule.name,
                rule.source.description()
            ));
        }

        sources
    }

    pub fn to_markdown(&self, quality_config: &QualityConfig) -> String {
        let mut output = String::new();
        output.push_str("# Evidence Report\n\n");
        output.push_str(&format!(
            "**Gathered At**: {}\n\n",
            self.gathered_at.format("%Y-%m-%d %H:%M:%S UTC")
        ));

        // Add evidence quality summary
        let quality = self.validate_quality(quality_config);
        output.push_str("## Evidence Quality\n\n");
        output.push_str(&format!(
            "- **Quality Score**: {:.1}%\n",
            quality.quality_score * 100.0
        ));
        output.push_str(&format!(
            "- **Total Items**: {}\n",
            quality.total_evidence_items
        ));
        output.push_str(&format!(
            "- **Verifiable**: {} ({:.0}%)\n",
            quality.verifiable_items,
            if quality.total_evidence_items > 0 {
                (quality.verifiable_items as f32 / quality.total_evidence_items as f32) * 100.0
            } else {
                0.0
            }
        ));
        output.push_str(&format!(
            "- **Avg Confidence**: {:.0}%\n\n",
            quality.average_confidence * 100.0
        ));

        if !quality.source_breakdown.is_empty() {
            output.push_str("### Evidence Sources\n\n");
            for (source, count) in &quality.source_breakdown {
                output.push_str(&format!("- {}: {} items\n", source, count));
            }
            output.push('\n');
        }

        if !quality.low_confidence_items.is_empty() {
            output.push_str("### Low Confidence Items\n\n");
            for item in &quality.low_confidence_items {
                output.push_str(&format!("- ⚠️ {}\n", item));
            }
            output.push('\n');
        }

        output.push_str("## Codebase Analysis\n\n");
        output.push_str("### Relevant Files\n\n");
        for file in &self.codebase_analysis.relevant_files {
            output.push_str(&format!(
                "- `{}` - {} [{}]\n",
                file.path,
                file.relevance,
                file.source.format_confidence_for_llm()
            ));
        }

        output.push_str("\n### Existing Patterns\n\n");
        for pattern in &self.codebase_analysis.existing_patterns {
            output.push_str(&format!("- {}\n", pattern));
        }

        output.push_str("\n### Conventions\n\n");
        for convention in &self.codebase_analysis.conventions {
            output.push_str(&format!("- {}\n", convention));
        }

        output.push_str("\n### Affected Areas\n\n");
        for area in &self.codebase_analysis.affected_areas {
            output.push_str(&format!("- {}\n", area));
        }

        output.push_str("\n## Dependency Analysis\n\n");
        output.push_str("### Current Dependencies\n\n");
        for dep in &self.dependency_analysis.current_dependencies {
            output.push_str(&format!("- {} ({})\n", dep.name, dep.version));
        }

        if !self.dependency_analysis.suggested_additions.is_empty() {
            output.push_str("\n### Suggested Additions\n\n");
            for dep in &self.dependency_analysis.suggested_additions {
                output.push_str(&format!("- {} - {}\n", dep.name, dep.reason));
            }
        }

        output.push_str("\n## Prior Knowledge\n\n");
        if !self.prior_knowledge.relevant_skills.is_empty() {
            output.push_str("### Relevant Skills\n\n");
            for skill in &self.prior_knowledge.relevant_skills {
                output.push_str(&format!(
                    "- {} - {} (confidence: {:.0}%)\n",
                    skill.name,
                    skill.relevance,
                    skill.confidence * 100.0
                ));
            }
        }

        if !self.prior_knowledge.relevant_rules.is_empty() {
            output.push_str("\n### Relevant Rules\n\n");
            for rule in &self.prior_knowledge.relevant_rules {
                output.push_str(&format!(
                    "- {} - {} (confidence: {:.0}%)\n",
                    rule.name,
                    rule.relevance,
                    rule.confidence * 100.0
                ));
            }
        }

        if !self.prior_knowledge.similar_patterns.is_empty() {
            output.push_str("\n### Similar Patterns\n\n");
            for pattern in &self.prior_knowledge.similar_patterns {
                output.push_str(&format!("- {}\n", pattern));
            }
        }

        output
    }

    /// Generate markdown with a maximum token budget.
    /// Prioritizes high-confidence evidence and truncates when necessary.
    pub fn to_markdown_with_budget(
        &self,
        max_tokens: usize,
        evidence_config: &EvidenceConfig,
        quality_config: &QualityConfig,
    ) -> String {
        let mut output = String::new();
        output.push_str("# Evidence Summary\n\n");

        let quality = self.validate_quality(quality_config);
        output.push_str(&format!(
            "Quality: {:.0}% | Items: {} | Confidence: {:.0}%\n\n",
            quality.quality_score * 100.0,
            quality.total_evidence_items,
            quality.average_confidence * 100.0
        ));

        let header_tokens = estimate_tokens(&output);
        let remaining_budget = max_tokens.saturating_sub(header_tokens);

        if remaining_budget < evidence_config.min_budget_threshold {
            output.push_str("_(Evidence truncated due to token limit)_\n");
            return output;
        }

        let file_budget = remaining_budget * evidence_config.file_budget_percent / 100;
        let pattern_budget = remaining_budget * evidence_config.pattern_budget_percent / 100;
        let dep_budget = remaining_budget * evidence_config.dependency_budget_percent / 100;
        let knowledge_budget = remaining_budget * evidence_config.knowledge_budget_percent / 100;

        // Sort files by confidence (higher confidence first)
        let mut sorted_files: Vec<_> = self.codebase_analysis.relevant_files.iter().collect();
        sorted_files.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        output.push_str("## Key Files\n\n");
        let mut file_tokens = 0;
        for file in sorted_files {
            let category = file.source.confidence_category();
            let line = format!("- `{}` [{}]\n", file.path, category.label());
            let line_tokens = estimate_tokens(&line);
            if file_tokens + line_tokens > file_budget {
                output.push_str("- _(more files omitted)_\n");
                break;
            }
            output.push_str(&line);
            file_tokens += line_tokens;
        }

        output.push_str("\n## Patterns\n\n");
        let mut pattern_tokens = 0;
        for pattern in &self.codebase_analysis.existing_patterns {
            let line = format!("- {}\n", pattern);
            let line_tokens = estimate_tokens(&line);
            if pattern_tokens + line_tokens > pattern_budget / 2 {
                break;
            }
            output.push_str(&line);
            pattern_tokens += line_tokens;
        }
        for conv in &self.codebase_analysis.conventions {
            let line = format!("- {}\n", conv);
            let line_tokens = estimate_tokens(&line);
            if pattern_tokens + line_tokens > pattern_budget {
                break;
            }
            output.push_str(&line);
            pattern_tokens += line_tokens;
        }

        if !self.dependency_analysis.current_dependencies.is_empty() {
            output.push_str("\n## Dependencies\n\n");
            let mut dep_tokens = 0;
            for dep in self
                .dependency_analysis
                .current_dependencies
                .iter()
                .take(evidence_config.max_dependencies_display)
            {
                let line = format!("- {}\n", dep.name);
                let line_tokens = estimate_tokens(&line);
                if dep_tokens + line_tokens > dep_budget {
                    break;
                }
                output.push_str(&line);
                dep_tokens += line_tokens;
            }
        }

        let has_knowledge = !self.prior_knowledge.relevant_skills.is_empty()
            || !self.prior_knowledge.relevant_rules.is_empty();
        if has_knowledge {
            output.push_str("\n## Prior Knowledge\n\n");
            let mut know_tokens = 0;

            let mut skills: Vec<_> = self.prior_knowledge.relevant_skills.iter().collect();
            skills.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for skill in skills {
                if skill.confidence < evidence_config.min_confidence_display {
                    continue;
                }
                let line = format!(
                    "- Skill: {} ({:.0}%)\n",
                    skill.name,
                    skill.confidence * 100.0
                );
                let line_tokens = estimate_tokens(&line);
                if know_tokens + line_tokens > knowledge_budget / 2 {
                    break;
                }
                output.push_str(&line);
                know_tokens += line_tokens;
            }

            let mut rules: Vec<_> = self.prior_knowledge.relevant_rules.iter().collect();
            rules.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for rule in rules {
                if rule.confidence < evidence_config.min_confidence_display {
                    continue;
                }
                let line = format!("- Rule: {} ({:.0}%)\n", rule.name, rule.confidence * 100.0);
                let line_tokens = estimate_tokens(&line);
                if know_tokens + line_tokens > knowledge_budget {
                    break;
                }
                output.push_str(&line);
                know_tokens += line_tokens;
            }
        }

        let final_tokens = estimate_tokens(&output);
        if final_tokens > max_tokens {
            let target_len = max_tokens * evidence_config.token_char_ratio;
            if output.len() > target_len {
                output.truncate(
                    target_len.saturating_sub(evidence_config.markdown_truncation_buffer),
                );
                output.push_str("\n\n_(truncated)_");
            }
        }

        output
    }

    /// Check if evidence would exceed token budget.
    pub fn exceeds_budget(&self, max_tokens: usize, quality_config: &QualityConfig) -> bool {
        estimate_tokens(&self.to_markdown(quality_config)) > max_tokens
    }

    /// Get estimated token count for full markdown.
    pub fn estimated_tokens(&self, quality_config: &QualityConfig) -> usize {
        estimate_tokens(&self.to_markdown(quality_config))
    }
}
