use std::path::{Path, PathBuf};
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::agent::LlmExecutor;
use crate::config::{ComplexityConfig, TaskScoringConfig};
use crate::error::Result;

use super::evidence::Evidence;
use super::task_scorer::{TaskScore, TaskScorer};

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

    pub fn format_for_prompt(&self) -> String {
        let tier_str = match self.actual_tier {
            ComplexityTier::Trivial => "TRIVIAL",
            ComplexityTier::Simple => "SIMPLE",
            ComplexityTier::Complex => "COMPLEX",
        };
        format!(
            "- \"{}\" → {} ({} files)",
            truncate(&self.description, 60),
            tier_str,
            self.files_affected
        )
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    crate::utils::truncate_at_boundary(s, max_len)
}

/// Stores and retrieves complexity examples for LLM calibration.
pub struct ComplexityExampleStore {
    examples_file: PathBuf,
    max_examples_per_tier: usize,
}

impl ComplexityExampleStore {
    /// Default max examples per tier for in-context learning.
    const DEFAULT_MAX_EXAMPLES_PER_TIER: usize = 3;

    pub fn new(pilot_dir: &Path) -> Self {
        Self {
            examples_file: pilot_dir.join("complexity_examples.jsonl"),
            max_examples_per_tier: Self::DEFAULT_MAX_EXAMPLES_PER_TIER,
        }
    }

    /// Set custom max examples per tier.
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
                section.push_str(&ex.format_for_prompt());
                section.push('\n');
            }
        }

        if !simple.is_empty() {
            section.push_str("\n**SIMPLE** (from this project):\n");
            for ex in simple {
                section.push_str(&ex.format_for_prompt());
                section.push('\n');
            }
        }

        if !complex.is_empty() {
            section.push_str("\n**COMPLEX** (from this project):\n");
            for ex in complex {
                section.push_str(&ex.format_for_prompt());
                section.push('\n');
            }
        }

        section
    }
}

/// Complexity tier for routing tasks to appropriate execution paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Complexity gate result with 3-tier routing.
#[derive(Debug, Clone)]
pub struct ComplexityGate {
    /// Complexity tier determining execution path.
    pub tier: ComplexityTier,
    /// Whether the task is simple enough for direct execution (trivial or simple).
    pub is_simple: bool,
    /// Brief reasoning for the decision.
    pub reasoning: String,
    /// Evidence-based signals used in the decision.
    pub evidence_signals: Option<EvidenceSignals>,
}

/// Evidence-based signals extracted for complexity assessment.
#[derive(Debug, Clone, Default)]
pub struct EvidenceSignals {
    /// Number of relevant files identified.
    pub relevant_file_count: usize,
    /// Number of detected dependencies.
    pub dependency_count: usize,
    /// Average confidence of evidence.
    pub avg_confidence: f32,
    /// Whether codebase has existing patterns for the task.
    pub has_similar_patterns: bool,
    /// Complete file scope information for LLM assessment.
    /// Shows all paths for small scopes, directory distribution for large scopes.
    pub file_scope: FileScope,
    /// Detected workspace type (monorepo tooling).
    /// None = single package, Some = workspace/monorepo detected.
    pub workspace_type: Option<WorkspaceType>,

    // === Structural complexity signals (language-agnostic) ===
    /// Number of distinct top-level directories touched.
    /// High value (>3) suggests cross-module changes requiring coordination.
    pub distinct_top_dirs: usize,
    /// Confidence variance (max - min). High variance suggests uncertainty.
    /// Values > 0.5 indicate mixed evidence quality.
    pub confidence_spread: f32,
    /// Ratio of low-confidence files (< 0.5).
    /// High ratio (> 0.3) suggests evidence quality issues.
    pub low_confidence_ratio: f32,
}

/// Confidence statistics for evidence quality assessment.
#[derive(Debug, Clone, Default)]
pub struct ConfidenceStats {
    pub min: f32,
    pub max: f32,
    pub median: f32,
    pub high_confidence_count: usize, // >= 0.7
    pub low_confidence_count: usize,  // < 0.5
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
/// Provides full picture with confidence distribution.
#[derive(Debug, Clone, Default)]
pub struct FileScope {
    pub total_count: usize,
    /// All file paths with confidence when count is manageable (≤20).
    pub all_paths: Vec<(String, f32)>,
    /// Directory distribution when total_count > 20.
    pub directory_distribution: Vec<(String, usize)>,
    /// Confidence statistics across all files.
    pub confidence_stats: ConfidenceStats,
}

impl FileScope {
    const SHOW_ALL_THRESHOLD: usize = 20;

    /// Create file scope from paths with confidence values.
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
            use std::collections::HashMap;

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

    /// Legacy: Create from paths only (confidence unknown).
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
    /// Rust workspace (Cargo.toml with [workspace])
    CargoWorkspace,
    /// Node.js workspaces (package.json with workspaces)
    NpmWorkspace,
    /// Yarn workspaces (similar to npm)
    YarnWorkspace,
    /// pnpm workspace (pnpm-workspace.yaml)
    PnpmWorkspace,
    /// Nx monorepo (nx.json)
    NxMonorepo,
    /// Turborepo (turbo.json)
    Turborepo,
    /// Lerna monorepo (lerna.json)
    Lerna,
    /// Gradle multi-project (settings.gradle with include)
    GradleMultiProject,
    /// Maven multi-module (pom.xml with modules)
    MavenMultiModule,
    /// Bazel/Buck workspace
    BazelBuck,
    /// Go workspace (go.work)
    GoWorkspace,
    /// Unknown workspace type (detected via directory structure)
    Unknown,
}

impl WorkspaceType {
    /// Returns whether this workspace type was detected via manifest file (verified)
    /// or via directory structure heuristics (uncertain).
    pub fn detection_confidence(&self) -> WorkspaceDetectionConfidence {
        match self {
            Self::Unknown => WorkspaceDetectionConfidence::Heuristic,
            _ => WorkspaceDetectionConfidence::ManifestVerified,
        }
    }

    /// Format for LLM context with appropriate uncertainty indication.
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
    /// Detected via manifest file (Cargo.toml [workspace], pnpm-workspace.yaml, etc.)
    ManifestVerified,
    /// Inferred from directory structure (apps/, packages/, libs/ patterns)
    Heuristic,
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

/// LLM-based complexity estimator with 3-tier routing.
/// Routes to: Trivial (skip planning) / Simple (direct execution) / Complex (full planning).
/// All complexity decisions are semantic - no heuristic gates.
pub struct ComplexityEstimator<E: LlmExecutor = Arc<crate::agent::TaskAgent>> {
    working_dir: PathBuf,
    executor: E,
    example_store: ComplexityExampleStore,
    cached_examples: Vec<ComplexityExample>,
    task_scorer: TaskScorer,
    assessment_timeout_secs: u64,
}

impl<E: LlmExecutor> ComplexityEstimator<E> {
    pub fn new(
        working_dir: &Path,
        config: &ComplexityConfig,
        executor: E,
        task_scoring_config: TaskScoringConfig,
    ) -> Self {
        let pilot_dir = working_dir.join(".claude").join("pilot");
        Self {
            working_dir: working_dir.to_path_buf(),
            executor,
            example_store: ComplexityExampleStore::new(&pilot_dir),
            cached_examples: Vec::new(),
            task_scorer: TaskScorer::new(task_scoring_config),
            assessment_timeout_secs: config.assessment_timeout_secs,
        }
    }

    /// Get the configured assessment timeout.
    pub fn assessment_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.assessment_timeout_secs)
    }

    pub async fn load_examples(&mut self) {
        self.cached_examples = self.example_store.load().await;
        debug!(
            count = self.cached_examples.len(),
            "Loaded complexity examples"
        );
    }

    pub async fn record_feedback(
        &self,
        description: &str,
        estimated: ComplexityTier,
        actual: ComplexityTier,
        files_affected: usize,
        reasoning: &str,
    ) {
        let example = ComplexityExample {
            description: description.to_string(),
            estimated_tier: estimated,
            actual_tier: actual,
            files_affected,
            reasoning: reasoning.to_string(),
        };

        if let Err(e) = self.example_store.append(&example).await {
            warn!(error = %e, "Failed to record complexity feedback");
        }
    }

    /// Determines task complexity tier.
    /// Falls back to "complex" if LLM call fails (safer default).
    pub async fn evaluate(&self, description: &str) -> ComplexityGate {
        self.evaluate_with_evidence(description, None).await
    }

    /// Determines complexity using both description and evidence.
    ///
    /// Uses a two-phase approach:
    /// 1. **Fast heuristics**: For clear-cut cases (trivial/obviously complex)
    /// 2. **LLM judgment**: For ambiguous cases requiring semantic understanding
    ///
    /// This avoids unnecessary LLM calls for trivial tasks while ensuring
    /// complex tasks get proper semantic analysis.
    pub async fn evaluate_with_evidence(
        &self,
        description: &str,
        evidence: Option<&Evidence>,
    ) -> ComplexityGate {
        let signals = evidence.map(Self::extract_signals);

        // Calculate multi-dimensional task score for LLM context
        let task_score = self
            .task_scorer
            .score_for_description(description, evidence);
        debug!(
            file_score = ?task_score.file_score,
            dependency_score = ?task_score.dependency_score,
            pattern_score = ?task_score.pattern_score,
            confidence_score = ?task_score.confidence_score,
            history_score = ?task_score.history_score,
            available_dimensions = task_score.available_dimensions,
            total = task_score.total,
            "TaskScorer signals"
        );

        // Phase 1: Try fast heuristics for clear-cut cases
        // These are "golden heuristics" that work universally across languages/frameworks
        if let Some(gate) = self.try_fast_route(&signals) {
            let file_count = signals.as_ref().map(|s| s.relevant_file_count).unwrap_or(0);
            info!(
                tier = ?gate.tier,
                files = file_count,
                "Complexity (fast-path): {} files → {:?}",
                file_count,
                gate.tier
            );
            return gate;
        }

        // Phase 2: LLM for ambiguous cases
        match self
            .evaluate_with_llm_and_evidence(description, &signals, &task_score)
            .await
        {
            Ok(mut gate) => {
                gate.evidence_signals = signals.clone();
                let file_count = signals.as_ref().map(|s| s.relevant_file_count).unwrap_or(0);
                info!(
                    tier = ?gate.tier,
                    files = file_count,
                    task_score = task_score.total,
                    "Complexity (LLM): {} files, score {:.2} → {:?}",
                    file_count,
                    task_score.total,
                    gate.tier
                );
                debug!(reasoning = %gate.reasoning, "LLM reasoning");
                gate
            }
            Err(e) => {
                warn!(error = %e, "Complexity evaluation failed, defaulting to planning");
                ComplexityGate::complex("Evaluation failed, using planning for safety")
            }
        }
    }

    /// Fast-path routing for clear-cut cases.
    ///
    /// These heuristics are intentionally conservative - they only fire when
    /// the signals are unambiguous. Ambiguous cases go to LLM.
    ///
    /// ## Golden Heuristics
    /// Fast-path routing for unambiguous complexity cases.
    ///
    /// **Conservative approach**: Only bypass LLM when signals are crystal clear.
    /// If ANY structural complexity indicator suggests uncertainty → let LLM decide.
    ///
    /// Universal signals used (work across all languages/frameworks):
    /// - File count: 1 vs 50+ (extreme cases only)
    /// - Confidence spread: High variance suggests mixed evidence quality
    /// - Cross-module indicator: Multiple top-level directories = coordination needed
    /// - Low confidence ratio: High ratio = uncertain evidence
    ///
    /// All borderline cases → LLM decides (preserves LLM's judgment capability)
    fn try_fast_route(&self, signals: &Option<EvidenceSignals>) -> Option<ComplexityGate> {
        let signals = signals.as_ref()?;

        let files = signals.relevant_file_count;
        let deps = signals.dependency_count;
        let confidence = signals.avg_confidence;
        let has_patterns = signals.has_similar_patterns;
        let is_workspace = signals.workspace_type.is_some();

        // Structural complexity indicators (language-agnostic)
        let confidence_spread = signals.confidence_spread;
        let low_confidence_ratio = signals.low_confidence_ratio;
        let distinct_dirs = signals.distinct_top_dirs;

        // === Early bail-out: If ANY structural warning, let LLM decide ===
        // High confidence spread (>0.5) suggests mixed evidence quality
        // High low-confidence ratio (>0.3) suggests uncertain evidence
        // These thresholds are conservative: when uncertain, defer to LLM
        let has_quality_concerns = confidence_spread > 0.5 || low_confidence_ratio > 0.3;

        // TRIVIAL: Single file, no dependencies, very high confidence, no quality concerns
        // This is unambiguously trivial regardless of language/framework.
        // NOTE: has_patterns NOT required - single file is structurally trivial even for novel tasks.
        // LLM judgment is preserved for ambiguous cases (2+ files without patterns).
        if files == 1 && deps == 0 && confidence >= 0.95 && !has_quality_concerns {
            return Some(
                ComplexityGate::trivial(format!(
                    "Fast-path: 1 file, 0 deps, {:.0}% confidence",
                    confidence * 100.0
                ))
                .with_evidence(signals.clone()),
            );
        }

        // SIMPLE: Very focused scope with strong signals AND no structural complexity
        // Additional guard: files must be in same top-level directory (distinct_dirs <= 1)
        // This ensures "3 files" isn't actually "3 modules across the codebase"
        if files <= 3
            && deps <= 2
            && confidence >= 0.85
            && has_patterns
            && !is_workspace
            && !has_quality_concerns
            && distinct_dirs <= 1
        {
            return Some(
                ComplexityGate::simple(format!(
                    "Fast-path: {} files in same module, {} deps, {:.0}% confidence",
                    files,
                    deps,
                    confidence * 100.0
                ))
                .with_evidence(signals.clone()),
            );
        }

        // COMPLEX: Obviously large scope (50+ files always needs planning)
        if files >= 50 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: {} files (large scope requires planning)",
                    files
                ))
                .with_evidence(signals.clone()),
            );
        }

        // COMPLEX: Cross-module changes (4+ distinct directories, even with few files)
        // 8 files across 5 directories is more complex than 15 files in one directory
        if distinct_dirs >= 4 && files > 3 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: {} files across {} directories (cross-module coordination needed)",
                    files, distinct_dirs
                ))
                .with_evidence(signals.clone()),
            );
        }

        // Monorepo/workspace always needs planning due to cross-package concerns
        if is_workspace && files > 5 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: workspace detected with {} files (cross-package planning needed)",
                    files
                ))
                .with_evidence(signals.clone()),
            );
        }

        // Ambiguous: 4-49 files, mixed confidence, quality concerns → LLM decides
        // This preserves LLM's ability to apply nuanced judgment
        None
    }

    fn extract_signals(evidence: &Evidence) -> EvidenceSignals {
        let relevant_files = &evidence.codebase_analysis.relevant_files;
        let dependencies = &evidence.dependency_analysis.current_dependencies;
        let patterns = &evidence.prior_knowledge.similar_patterns;

        // Collect confidences for statistical analysis
        let confidences: Vec<f32> = relevant_files.iter().map(|f| f.confidence).collect();

        let avg_confidence = if confidences.is_empty() {
            0.0
        } else {
            confidences.iter().sum::<f32>() / confidences.len() as f32
        };

        // Calculate confidence spread (max - min) - language agnostic metric
        let (confidence_spread, low_confidence_ratio) = if confidences.is_empty() {
            (0.0, 0.0)
        } else {
            let min = confidences.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = confidences
                .iter()
                .cloned()
                .fold(f32::NEG_INFINITY, f32::max);
            let low_count = confidences.iter().filter(|&&c| c < 0.5).count();
            (max - min, low_count as f32 / confidences.len() as f32)
        };

        // Count distinct top-level directories - structural complexity metric
        let distinct_top_dirs = {
            use std::collections::HashSet;
            relevant_files
                .iter()
                .filter_map(|f| f.path.split('/').next())
                .collect::<HashSet<_>>()
                .len()
        };

        // Include confidence per file for complete LLM context.
        let paths_with_confidence: Vec<(String, f32)> = relevant_files
            .iter()
            .map(|f| (f.path.clone(), f.confidence))
            .collect();
        let file_scope = FileScope::from_paths_with_confidence(&paths_with_confidence);

        let workspace_type = Self::detect_workspace_from_files(relevant_files);

        EvidenceSignals {
            relevant_file_count: relevant_files.len(),
            dependency_count: dependencies.len(),
            avg_confidence,
            has_similar_patterns: !patterns.is_empty(),
            file_scope,
            workspace_type,
            distinct_top_dirs,
            confidence_spread,
            low_confidence_ratio,
        }
    }

    /// Detect workspace type from file patterns.
    /// Uses exact file name matching at root or near-root level only.
    fn detect_workspace_from_files(
        files: &[super::evidence::RelevantFile],
    ) -> Option<WorkspaceType> {
        for file in files {
            let path = &file.path;

            // Only consider files at root level (no parent or one parent directory)
            let depth = path.matches('/').count();
            if depth > 1 {
                continue;
            }

            let filename = path.rsplit('/').next().unwrap_or(path).to_lowercase();

            // Exact file name matches only
            match filename.as_str() {
                "pnpm-workspace.yaml" => return Some(WorkspaceType::PnpmWorkspace),
                "nx.json" => return Some(WorkspaceType::NxMonorepo),
                "turbo.json" => return Some(WorkspaceType::Turborepo),
                "lerna.json" => return Some(WorkspaceType::Lerna),
                "go.work" => return Some(WorkspaceType::GoWorkspace),
                "workspace" | "workspace.bazel" => return Some(WorkspaceType::BazelBuck),
                _ => {}
            }
        }

        None
    }

    async fn evaluate_with_llm_and_evidence(
        &self,
        description: &str,
        signals: &Option<EvidenceSignals>,
        task_score: &TaskScore,
    ) -> Result<ComplexityGate> {
        let task_score_context = format!("\n{}", task_score.format_for_llm());

        let evidence_context = signals
            .as_ref()
            .map(|s| {
                let workspace_info = s.workspace_type.map_or(String::new(), |wt| {
                    format!("- Workspace: {}\n", wt.format_for_llm())
                });

                let file_scope_info = format!("{}\n", s.file_scope.format_for_llm());

                format!(
                    r"
## Evidence Signals (from codebase analysis)
- Relevant files: {} files identified
- Dependencies: {} detected
- Average confidence: {:.0}%
- Has similar patterns: {}
{workspace_info}{file_scope_info}",
                    s.relevant_file_count,
                    s.dependency_count,
                    s.avg_confidence * 100.0,
                    if s.has_similar_patterns { "yes" } else { "no" }
                )
            })
            .unwrap_or_default();

        let examples_section = self
            .example_store
            .format_examples_section(&self.cached_examples);

        let prompt = format!(
            r"Classify this task's complexity for execution routing.

## Task
{description}
{evidence_context}{task_score_context}{examples_section}
## TRIVIAL (skip planning entirely)
- Single-line fix (typo, import, minor syntax)
- Obvious bug fix with clear solution
- Simple rename or delete operation
- Adding a single obvious field/parameter
- Truly minimal scope: 1 file, no decisions, no dependencies

## SIMPLE (direct execution, minimal planning)
- Single focused change with clear semantic scope
- No architectural decisions
- Existing patterns to follow
- Unambiguous requirements
NOTE: File count alone does NOT determine complexity.
A 30-file utility rename can be SIMPLE. A 1-file architecture change can be COMPLEX.
Focus on semantic scope, not file count.

## COMPLEX (full planning with spec/plan/tasks)
- Multiple components or features
- Architectural decisions required
- Requires research or exploration
- Ambiguous requirements
- Cross-package changes in monorepo
- Changes affecting external API contracts"
        );

        let response: ComplexityResult = self
            .executor
            .execute_structured(&prompt, &self.working_dir)
            .await?;

        Ok(self.build_gate(response))
    }

    fn build_gate(&self, response: ComplexityResult) -> ComplexityGate {
        let tier = match response.tier {
            ComplexityTierValue::Trivial => ComplexityTier::Trivial,
            ComplexityTierValue::Simple => ComplexityTier::Simple,
            ComplexityTierValue::Complex => ComplexityTier::Complex,
        };

        ComplexityGate {
            tier,
            is_simple: tier != ComplexityTier::Complex,
            reasoning: response
                .reasoning
                .unwrap_or_else(|| "No reasoning provided".into()),
            evidence_signals: None,
        }
    }
}

/// Structured response for complexity gate assessment.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComplexityResult {
    /// Complexity tier: "trivial", "simple", or "complex"
    pub tier: ComplexityTierValue,
    /// Brief explanation for the classification decision
    #[serde(default)]
    pub reasoning: Option<String>,
}

/// Complexity tier values for structured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTierValue {
    Trivial,
    Simple,
    Complex,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complexity_tier() {
        assert!(ComplexityTier::Trivial.is_trivial());
        assert!(!ComplexityTier::Trivial.needs_planning());

        assert!(ComplexityTier::Simple.is_simple());
        assert!(!ComplexityTier::Simple.needs_planning());

        assert!(!ComplexityTier::Complex.is_simple());
        assert!(ComplexityTier::Complex.needs_planning());
    }

    #[test]
    fn test_gate_constructors() {
        let trivial = ComplexityGate::trivial("Fix typo");
        assert_eq!(trivial.tier, ComplexityTier::Trivial);
        assert!(trivial.is_simple);

        let simple = ComplexityGate::simple("Add field");
        assert_eq!(simple.tier, ComplexityTier::Simple);
        assert!(simple.is_simple);

        let complex = ComplexityGate::complex("New feature");
        assert_eq!(complex.tier, ComplexityTier::Complex);
        assert!(!complex.is_simple);
    }

    #[test]
    fn test_complexity_tier_ordering() {
        assert!(ComplexityTier::Trivial < ComplexityTier::Simple);
        assert!(ComplexityTier::Simple < ComplexityTier::Complex);
    }

    #[test]
    fn test_file_scope_small() {
        // Small scope: should show all paths
        let paths: Vec<String> = (0..5).map(|i| format!("src/file{}.rs", i)).collect();
        let scope = FileScope::from_paths(&paths);

        assert_eq!(scope.total_count, 5);
        assert_eq!(scope.all_paths.len(), 5);
        assert!(scope.directory_distribution.is_empty());

        let formatted = scope.format_for_llm();
        assert!(formatted.contains("5 total"), "Got: {}", formatted);
        assert!(formatted.contains("src/file0.rs"));
    }

    #[test]
    fn test_file_scope_large() {
        // Large scope: should show directory distribution
        let mut paths: Vec<String> = Vec::new();
        for i in 0..15 {
            paths.push(format!("src/module_a/file{}.rs", i));
        }
        for i in 0..10 {
            paths.push(format!("tests/file{}.rs", i));
        }

        let scope = FileScope::from_paths(&paths);

        assert_eq!(scope.total_count, 25);
        assert!(scope.all_paths.is_empty());
        assert!(!scope.directory_distribution.is_empty());

        let formatted = scope.format_for_llm();
        assert!(formatted.contains("25 total"), "Got: {}", formatted);
        assert!(formatted.contains("distribution"), "Got: {}", formatted);
        assert!(formatted.contains("src/"));
        assert!(formatted.contains("tests/"));
    }

    #[test]
    fn test_file_scope_threshold() {
        let paths: Vec<String> = (0..20).map(|i| format!("src/file{}.rs", i)).collect();
        let scope = FileScope::from_paths(&paths);

        assert_eq!(scope.total_count, 20);
        assert_eq!(scope.all_paths.len(), 20);
        assert!(scope.directory_distribution.is_empty());

        let paths: Vec<String> = (0..21).map(|i| format!("src/file{}.rs", i)).collect();
        let scope = FileScope::from_paths(&paths);

        assert_eq!(scope.total_count, 21);
        assert!(scope.all_paths.is_empty());
        assert!(!scope.directory_distribution.is_empty());
    }

    #[test]
    fn test_file_scope_with_confidence() {
        let paths_with_conf: Vec<(String, f32)> = vec![
            ("src/main.rs".into(), 0.9),
            ("src/lib.rs".into(), 0.8),
            ("src/config.rs".into(), 0.3),
        ];
        let scope = FileScope::from_paths_with_confidence(&paths_with_conf);

        assert_eq!(scope.total_count, 3);
        assert_eq!(scope.confidence_stats.min, 0.3);
        assert_eq!(scope.confidence_stats.max, 0.9);
        assert_eq!(scope.confidence_stats.high_confidence_count, 2); // 0.9, 0.8 >= 0.7
        assert_eq!(scope.confidence_stats.low_confidence_count, 1); // 0.3 < 0.5

        let formatted = scope.format_for_llm();
        assert!(
            formatted.contains("90%"),
            "Should show confidence, got: {}",
            formatted
        );
    }

    #[test]
    fn test_file_scope_empty() {
        let scope = FileScope::from_paths(&[]);
        assert_eq!(scope.total_count, 0);
        assert!(scope.format_for_llm().contains("No relevant files"));
    }

    #[test]
    fn test_confidence_stats() {
        let values = vec![0.1, 0.5, 0.9];
        let stats = ConfidenceStats::from_values(&values);

        assert_eq!(stats.min, 0.1);
        assert_eq!(stats.max, 0.9);
        assert_eq!(stats.median, 0.5);
        assert_eq!(stats.high_confidence_count, 1); // 0.9 >= 0.7
        assert_eq!(stats.low_confidence_count, 1); // 0.1 < 0.5
    }
}
