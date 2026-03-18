use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::agent::LlmExecutor;
use crate::config::{ComplexityAssessmentConfig, TaskScoringConfig};
use crate::error::Result;

use super::examples::{ComplexityExample, ComplexityExampleStore};
use super::tiers::{
    ComplexityGate, ComplexityResult, ComplexityTier, EvidenceSignals, FileScope, WorkspaceType,
};
use crate::planning::evidence::Evidence;
use crate::planning::task_scorer::{TaskComplexityScore, TaskScorer};

/// LLM-based complexity estimator with 3-tier routing.
/// Routes to: Trivial (skip planning) / Simple (direct execution) / Complex (full planning).
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
        config: &ComplexityAssessmentConfig,
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

    pub async fn evaluate(&self, description: &str) -> ComplexityGate {
        self.evaluate_with_evidence(description, None).await
    }

    pub async fn evaluate_with_evidence(
        &self,
        description: &str,
        evidence: Option<&Evidence>,
    ) -> ComplexityGate {
        let signals = evidence.map(Self::extract_signals);

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

    fn try_fast_route(&self, signals: &Option<EvidenceSignals>) -> Option<ComplexityGate> {
        let signals = signals.as_ref()?;

        let files = signals.relevant_file_count;
        let deps = signals.dependency_count;
        let confidence = signals.avg_confidence;
        let has_patterns = signals.has_similar_patterns;
        let is_workspace = signals.workspace_type.is_some();

        let confidence_spread = signals.confidence_spread;
        let low_confidence_ratio = signals.low_confidence_ratio;
        let distinct_dirs = signals.distinct_top_dirs;

        let has_quality_concerns = confidence_spread > 0.5 || low_confidence_ratio > 0.3;

        // TRIVIAL: Single file, no dependencies, very high confidence, no quality concerns
        if files == 1 && deps == 0 && confidence >= 0.95 && !has_quality_concerns {
            return Some(
                ComplexityGate::trivial(format!(
                    "Fast-path: 1 file, 0 deps, {:.0}% confidence",
                    confidence * 100.0
                ))
                .with_evidence(signals.clone()),
            );
        }

        // SIMPLE: Very focused scope with strong signals
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

        // COMPLEX: Obviously large scope
        if files >= 50 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: {} files (large scope requires planning)",
                    files
                ))
                .with_evidence(signals.clone()),
            );
        }

        // COMPLEX: Cross-module changes
        if distinct_dirs >= 4 && files > 3 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: {} files across {} directories (cross-module coordination needed)",
                    files, distinct_dirs
                ))
                .with_evidence(signals.clone()),
            );
        }

        // Monorepo/workspace with many files
        if is_workspace && files > 5 {
            return Some(
                ComplexityGate::complex(format!(
                    "Fast-path: workspace detected with {} files (cross-package planning needed)",
                    files
                ))
                .with_evidence(signals.clone()),
            );
        }

        None
    }

    fn extract_signals(evidence: &Evidence) -> EvidenceSignals {
        let relevant_files = &evidence.codebase_analysis.relevant_files;
        let dependencies = &evidence.dependency_analysis.current_dependencies;
        let patterns = &evidence.prior_knowledge.similar_patterns;

        let confidences: Vec<f32> = relevant_files.iter().map(|f| f.confidence).collect();

        let avg_confidence = if confidences.is_empty() {
            0.0
        } else {
            confidences.iter().sum::<f32>() / confidences.len() as f32
        };

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

        let distinct_top_dirs = relevant_files
            .iter()
            .filter_map(|f| f.path.split('/').next())
            .collect::<HashSet<_>>()
            .len();

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

    fn detect_workspace_from_files(
        files: &[crate::planning::evidence::RelevantFile],
    ) -> Option<WorkspaceType> {
        for file in files {
            let path = &file.path;

            let depth = path.matches('/').count();
            if depth > 1 {
                continue;
            }

            let filename = path.rsplit('/').next().unwrap_or(path).to_lowercase();

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
        task_score: &TaskComplexityScore,
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
        ComplexityGate {
            tier: response.tier,
            is_simple: response.tier != ComplexityTier::Complex,
            reasoning: response
                .reasoning
                .unwrap_or_else(|| "No reasoning provided".into()),
            evidence_signals: None,
        }
    }
}
