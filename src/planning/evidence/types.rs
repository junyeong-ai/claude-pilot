use serde::{Deserialize, Serialize};

pub use super::super::validation::QualityTier;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceCompleteness {
    pub is_complete: bool,
    pub files_found: usize,
    pub files_limit: usize,
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
            "INCOMPLETE EVIDENCE ({:.0}% coverage): Only {} of {} files analyzed. \
             Proceed with caution - verify assumptions and be conservative with changes. \
             Issues: {}",
            coverage_pct,
            self.files_found,
            self.files_limit,
            self.warnings.join("; ")
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvidenceSufficiency {
    Complete,
    Sufficient {
        coverage: f32,
        missing: Vec<String>,
    },
    SufficientButRisky {
        coverage: f32,
        missing: Vec<String>,
        risk_context: String,
    },
    Insufficient {
        reason: String,
        coverage: f32,
    },
}

impl EvidenceSufficiency {
    pub fn allows_planning(&self) -> bool {
        !matches!(self, Self::Insufficient { .. })
    }

    pub fn requires_llm_assessment(&self) -> bool {
        matches!(self, Self::SufficientButRisky { .. })
    }

    pub fn coverage(&self) -> f32 {
        match self {
            Self::Complete => 1.0,
            Self::Sufficient { coverage, .. } => *coverage,
            Self::SufficientButRisky { coverage, .. } => *coverage,
            Self::Insufficient { coverage, .. } => *coverage,
        }
    }

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
    #[serde(default)]
    pub test_structure: TestStructure,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestStructure {
    pub file_patterns: Vec<String>,
    pub test_directories: Vec<String>,
    pub module_test_mapping: Vec<ModuleTestMapping>,
    pub framework: Option<String>,
}

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
    pub source: EvidenceSource,
    pub confidence: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum EvidenceSource {
    PatternDetection { pattern: String },
    GitHistory { commit: Option<String> },
    DependencyAnalysis,
    FilesystemStructure,
    PriorKnowledge { source_path: String },
    UserProvided,

    LspDefinition {
        location: String,
        target_file: String,
        target_line: u32,
    },
    LspReferences {
        symbol: String,
        reference_count: usize,
    },
    LspCallHierarchy {
        symbol: String,
        direction: CallDirection,
        call_count: usize,
    },
    LspSymbolSearch {
        query: String,
        symbol_kind: Option<String>,
    },
    LspHover { location: String },
    LspDiagnostics {
        file: String,
        error_count: usize,
        warning_count: usize,
    },
    LspContext { location: String },

    AstSearch {
        pattern: String,
        language: String,
        match_count: usize,
    },

    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDirection {
    Incoming,
    Outgoing,
}

impl EvidenceSource {
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

    pub fn is_verifiable(&self) -> bool {
        !matches!(self, Self::Unknown | Self::UserProvided)
    }

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

    pub fn is_ast_based(&self) -> bool {
        matches!(self, Self::AstSearch { .. })
    }

    pub fn is_semantic(&self) -> bool {
        self.is_lsp_based() || self.is_ast_based()
    }

    pub fn confidence_category(&self) -> SourceConfidenceCategory {
        match self {
            Self::LspDefinition { .. }
            | Self::LspReferences { .. }
            | Self::LspCallHierarchy { .. }
            | Self::LspDiagnostics { .. }
            | Self::LspContext { .. }
            | Self::AstSearch { .. } => SourceConfidenceCategory::Semantic,

            Self::LspSymbolSearch { .. }
            | Self::LspHover { .. }
            | Self::GitHistory { .. }
            | Self::DependencyAnalysis => SourceConfidenceCategory::ToolVerified,

            Self::FilesystemStructure
            | Self::PatternDetection { .. }
            | Self::PriorKnowledge { .. } => SourceConfidenceCategory::Heuristic,

            Self::UserProvided | Self::Unknown => SourceConfidenceCategory::Unverified,
        }
    }

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceConfidenceCategory {
    Semantic,
    ToolVerified,
    Heuristic,
    Unverified,
}

impl SourceConfidenceCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Semantic => "SEMANTIC",
            Self::ToolVerified => "TOOL-VERIFIED",
            Self::Heuristic => "HEURISTIC",
            Self::Unverified => "UNVERIFIED",
        }
    }

    pub fn level_hint(&self) -> &'static str {
        match self {
            Self::Semantic => "high",
            Self::ToolVerified => "moderate-high",
            Self::Heuristic => "moderate",
            Self::Unverified => "low",
        }
    }

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
    pub source: EvidenceSource,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoundRule {
    pub name: String,
    pub path: String,
    pub relevance: String,
    pub source: EvidenceSource,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct EvidenceQualitySummary {
    pub total_evidence_items: usize,
    pub verifiable_items: usize,
    pub average_confidence: f32,
    pub source_breakdown: Vec<(String, usize)>,
    pub low_confidence_items: Vec<String>,
    pub quality_score: f32,
}

pub(super) trait EvidenceItem {
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
