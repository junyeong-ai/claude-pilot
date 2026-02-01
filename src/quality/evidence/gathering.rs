use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use claude_agent::{Agent as SdkAgent, Auth, PermissionMode, ToolAccess};
use glob::glob;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::config::{AgentConfig, ModelAgentType};
use crate::error::{ExecutionError, PilotError, Result};
use crate::quality::EvidenceGatheringConfig;
use crate::quality::evidence::SufficiencyContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethod {
    DirectPath,
    GlobPattern,
    ContentMatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatheringResult {
    pub files: Vec<VerifiedFile>,
    pub patterns: Vec<DiscoveredPattern>,
    pub iteration_count: u32,
    pub converged: bool,
    pub coverage_score: f32,
    pub hit_search_limit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedFile {
    pub path: PathBuf,
    pub relevance: String,
    pub confidence: f32,
    pub discovery_method: DiscoveryMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DiscoveredPattern {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct SearchStrategy {
    #[serde(default)]
    glob_patterns: Vec<GlobQuery>,
    #[serde(default)]
    content_patterns: Vec<ContentQuery>,
    #[serde(default)]
    direct_paths: Vec<DirectPath>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct GlobQuery {
    pattern: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ContentQuery {
    pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DirectPath {
    path: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Default)]
struct SearchResults {
    files: Vec<VerifiedFile>,
    content_matches: Vec<ContentMatch>,
    invalid_paths: Vec<String>,
    hit_search_limit: bool,
}

impl SearchResults {
    fn merge(&mut self, other: Self) {
        let existing_paths: HashSet<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        self.files.extend(
            other
                .files
                .into_iter()
                .filter(|f| !existing_paths.contains(&f.path)),
        );

        let existing_matches: HashSet<(PathBuf, u32)> = self
            .content_matches
            .iter()
            .map(|m| (m.path.clone(), m.line))
            .collect();
        self.content_matches.extend(
            other
                .content_matches
                .into_iter()
                .filter(|m| !existing_matches.contains(&(m.path.clone(), m.line))),
        );

        self.invalid_paths.extend(other.invalid_paths);
        self.hit_search_limit = self.hit_search_limit || other.hit_search_limit;
    }

    fn to_feedback(&self) -> String {
        let mut feedback = String::new();

        if !self.files.is_empty() {
            feedback.push_str("## Found Files\n");
            for f in &self.files {
                feedback.push_str(&format!("- `{}`: {}\n", f.path.display(), f.relevance));
            }
            feedback.push('\n');
        }

        if !self.content_matches.is_empty() {
            feedback.push_str("## Content Matches\n");
            for m in self.content_matches.iter().take(20) {
                feedback.push_str(&format!(
                    "- `{}:{}`: {}\n",
                    m.path.display(),
                    m.line,
                    m.snippet
                ));
            }
            if self.content_matches.len() > 20 {
                feedback.push_str(&format!(
                    "... and {} more\n",
                    self.content_matches.len() - 20
                ));
            }
            feedback.push('\n');
        }

        if !self.invalid_paths.is_empty() {
            feedback.push_str("## Invalid Paths\n");
            for path in &self.invalid_paths {
                feedback.push_str(&format!("- `{}`\n", path));
            }
            feedback.push('\n');
        }

        feedback
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentMatch {
    pub path: PathBuf,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct Evaluation {
    #[serde(default)]
    converged: bool,
    #[serde(default)]
    coverage_score: f32,
    #[serde(default)]
    patterns_found: Vec<DiscoveredPattern>,
    #[serde(default)]
    additional_queries: Option<SearchStrategy>,
}

/// LLM decision when coverage plateau is detected.
/// Two-phase approach: programmatic detection + LLM consultation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct PlateauDecision {
    /// Whether to continue gathering evidence despite plateau
    should_continue: bool,
    /// Brief explanation of the decision
    #[serde(default)]
    reason: String,
    /// If continuing, optional refined strategy to try
    #[serde(default)]
    refined_strategy: Option<SearchStrategy>,
}

pub struct EvidenceGatheringLoop {
    config: EvidenceGatheringConfig,
    agent_config: AgentConfig,
    working_dir: PathBuf,
}

impl EvidenceGatheringLoop {
    pub fn new(
        working_dir: &Path,
        config: EvidenceGatheringConfig,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            config,
            agent_config,
            working_dir: working_dir.to_path_buf(),
        }
    }

    pub async fn gather(&self, mission: &str, task: Option<&str>) -> Result<GatheringResult> {
        info!(
            mission = mission,
            max_iterations = self.config.max_iterations,
            timeout_secs = self.config.total_timeout_secs,
            "Starting evidence gathering"
        );

        let total_timeout = Duration::from_secs(self.config.total_timeout_secs);

        match timeout(total_timeout, self.gather_inner(mission, task)).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    timeout_secs = self.config.total_timeout_secs,
                    "Evidence gathering timed out"
                );
                Err(PilotError::EvidenceGathering(format!(
                    "Timed out after {} seconds",
                    self.config.total_timeout_secs
                )))
            }
        }
    }

    async fn gather_inner(&self, mission: &str, task: Option<&str>) -> Result<GatheringResult> {
        let mut accumulated = SearchResults::default();
        let mut refinement_attempts = 0;
        const MAX_REFINEMENT_ATTEMPTS: u32 = 2;
        const PLATEAU_THRESHOLD: f32 = 0.02; // 2% improvement threshold
        const PLATEAU_WINDOW: usize = 3; // Check last 3 iterations

        // Track coverage history for plateau detection
        let mut coverage_history: Vec<f32> = Vec::new();

        let initial_strategy = self.generate_strategy(mission, task, &accumulated).await?;
        debug!(
            glob_count = initial_strategy.glob_patterns.len(),
            content_count = initial_strategy.content_patterns.len(),
            direct_count = initial_strategy.direct_paths.len(),
            "Initial strategy"
        );

        let results = self.execute_searches(&initial_strategy)?;
        accumulated.merge(results);
        let mut iteration = 1;

        loop {
            let evaluation = self.evaluate_and_plan(mission, &accumulated).await?;
            coverage_history.push(evaluation.coverage_score);

            if evaluation.converged {
                info!(iteration, coverage = evaluation.coverage_score, "Converged");
                return Ok(self.build_result(
                    accumulated,
                    evaluation.patterns_found,
                    iteration,
                    true,
                    evaluation.coverage_score,
                ));
            }

            // Semantic stopping: detect coverage plateau, then consult LLM
            if let Some(reason) =
                self.detect_plateau(&coverage_history, PLATEAU_THRESHOLD, PLATEAU_WINDOW)
            {
                info!(
                    iteration,
                    coverage = evaluation.coverage_score,
                    reason = reason,
                    "Coverage plateau detected, consulting LLM"
                );

                // Two-phase: programmatic detection + LLM consultation
                let decision = self
                    .consult_plateau_decision(mission, reason, &accumulated, &coverage_history)
                    .await?;

                if !decision.should_continue {
                    info!(
                        iteration,
                        reason = %decision.reason,
                        "LLM decided to stop gathering"
                    );
                    return Ok(self.build_result(
                        accumulated,
                        evaluation.patterns_found,
                        iteration,
                        true, // Treat as successful convergence
                        evaluation.coverage_score,
                    ));
                }

                // LLM wants to continue - use refined strategy if provided
                info!(
                    iteration,
                    reason = %decision.reason,
                    has_strategy = decision.refined_strategy.is_some(),
                    "LLM decided to continue gathering"
                );

                if let Some(strategy) = decision.refined_strategy {
                    let results = self.execute_searches(&strategy)?;
                    accumulated.merge(results);
                    // Continue loop with new results
                }
                // If no refined strategy, let the normal loop continue with evaluation.additional_queries
            }

            // Quality-based retry: if search limits hit and coverage is low, try refined strategy
            let needs_refinement = accumulated.hit_search_limit
                && evaluation.coverage_score < self.config.min_coverage_threshold
                && refinement_attempts < MAX_REFINEMENT_ATTEMPTS;

            if needs_refinement {
                refinement_attempts += 1;
                info!(
                    refinement_attempts,
                    coverage = evaluation.coverage_score,
                    "Search limit hit with low coverage, generating refined strategy"
                );

                if let Some(refined) = self
                    .generate_refined_strategy(mission, task, &accumulated)
                    .await?
                {
                    let results = self.execute_searches(&refined)?;
                    accumulated.merge(results);
                    continue;
                }
            }

            if iteration >= self.config.max_iterations {
                if evaluation.coverage_score < self.config.min_coverage_threshold {
                    warn!(
                        iteration,
                        coverage = evaluation.coverage_score,
                        threshold = self.config.min_coverage_threshold,
                        "Max iterations with insufficient coverage"
                    );
                } else {
                    warn!(
                        iteration,
                        coverage = evaluation.coverage_score,
                        "Max iterations reached"
                    );
                }
                return Ok(self.build_result(
                    accumulated,
                    evaluation.patterns_found,
                    iteration,
                    false,
                    evaluation.coverage_score,
                ));
            }

            let Some(next_strategy) = evaluation.additional_queries else {
                debug!("No additional queries");
                return Ok(self.build_result(
                    accumulated,
                    evaluation.patterns_found,
                    iteration,
                    false,
                    evaluation.coverage_score,
                ));
            };

            iteration += 1;
            debug!(iteration, "Additional search");

            let results = self.execute_searches(&next_strategy)?;
            accumulated.merge(results);
        }
    }

    /// Detect coverage plateau: if last N iterations show < threshold improvement.
    /// Returns plateau reason if detected.
    fn detect_plateau(
        &self,
        history: &[f32],
        threshold: f32,
        window: usize,
    ) -> Option<&'static str> {
        if history.len() < window {
            return None;
        }

        let recent = &history[history.len() - window..];
        let first = recent[0];
        let last = recent[recent.len() - 1];
        let improvement = last - first;

        if improvement.abs() < threshold {
            if last >= self.config.min_coverage_threshold {
                return Some("coverage stable at acceptable level");
            } else {
                return Some("coverage stalled below threshold");
            }
        }

        // Check for oscillation (coverage going up and down)
        let increasing = recent.windows(2).filter(|w| w[1] > w[0]).count();
        let decreasing = recent.windows(2).filter(|w| w[1] < w[0]).count();
        if increasing > 0 && decreasing > 0 && improvement.abs() < threshold * 2.0 {
            return Some("coverage oscillating");
        }

        None
    }

    async fn consult_plateau_decision(
        &self,
        mission: &str,
        reason: &str,
        accumulated: &SearchResults,
        coverage_history: &[f32],
    ) -> Result<PlateauDecision> {
        let confidence_values: Vec<f32> = accumulated.files.iter().map(|f| f.confidence).collect();
        let current_coverage = coverage_history.last().copied().unwrap_or(0.0);

        let context = SufficiencyContext::new(current_coverage, &confidence_values, None);
        let prompt = build_plateau_consultation_prompt(mission, reason, &context);

        match self.call_llm_structured::<PlateauDecision>(&prompt).await {
            Ok(decision) => {
                debug!(
                    should_continue = decision.should_continue,
                    reason = %decision.reason,
                    has_strategy = decision.refined_strategy.is_some(),
                    "LLM plateau decision"
                );
                Ok(decision)
            }
            Err(e) => {
                warn!(error = %e, "Plateau consultation failed, defaulting to stop");
                Ok(PlateauDecision {
                    should_continue: false,
                    reason: format!("LLM consultation failed: {}", e),
                    refined_strategy: None,
                })
            }
        }
    }

    async fn generate_refined_strategy(
        &self,
        mission: &str,
        task: Option<&str>,
        previous: &SearchResults,
    ) -> Result<Option<SearchStrategy>> {
        let prompt = build_refinement_prompt(mission, task, previous);
        match self.call_llm_structured::<SearchStrategy>(&prompt).await {
            Ok(strategy)
                if !strategy.glob_patterns.is_empty()
                    || !strategy.content_patterns.is_empty()
                    || !strategy.direct_paths.is_empty() =>
            {
                Ok(Some(strategy))
            }
            Ok(_) => {
                debug!("Refined strategy empty, no refinement possible");
                Ok(None)
            }
            Err(e) => {
                warn!(error = %e, "Failed to generate refined strategy");
                Ok(None)
            }
        }
    }

    fn build_result(
        &self,
        accumulated: SearchResults,
        patterns: Vec<DiscoveredPattern>,
        iteration_count: u32,
        converged: bool,
        coverage_score: f32,
    ) -> GatheringResult {
        let files =
            self.merge_files_with_content_matches(accumulated.files, &accumulated.content_matches);

        GatheringResult {
            hit_search_limit: accumulated.hit_search_limit,
            files,
            patterns,
            iteration_count,
            converged,
            coverage_score,
        }
    }

    fn merge_files_with_content_matches(
        &self,
        mut files: Vec<VerifiedFile>,
        content_matches: &[ContentMatch],
    ) -> Vec<VerifiedFile> {
        let existing_paths: HashSet<PathBuf> = files.iter().map(|f| f.path.clone()).collect();

        let content_files: Vec<VerifiedFile> = content_matches
            .iter()
            .map(|m| m.path.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|p| !existing_paths.contains(p))
            .map(|path| VerifiedFile {
                path,
                relevance: "Contains matching code patterns".to_string(),
                confidence: self.config.confidence.content_match,
                discovery_method: DiscoveryMethod::ContentMatch,
            })
            .collect();

        files.extend(content_files);
        files
    }

    async fn generate_strategy(
        &self,
        mission: &str,
        task: Option<&str>,
        previous: &SearchResults,
    ) -> Result<SearchStrategy> {
        let prompt = build_strategy_prompt(mission, task, previous);
        self.call_llm_structured(&prompt).await
    }

    async fn evaluate_and_plan(
        &self,
        mission: &str,
        results: &SearchResults,
    ) -> Result<Evaluation> {
        let prompt = build_evaluation_prompt(mission, results);
        self.call_llm_structured(&prompt).await
    }

    async fn call_llm_structured<T>(&self, prompt: &str) -> Result<T>
    where
        T: schemars::JsonSchema + serde::de::DeserializeOwned + Send + 'static,
    {
        let mut last_error = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay_ms = self.config.retry_base_delay_ms * (1 << (attempt - 1));
                debug!(attempt, delay_ms, "Retrying LLM call");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            match self.call_llm_structured_inner::<T>(prompt).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if Self::is_retryable(&e) && attempt < self.config.max_retries {
                        warn!(attempt = attempt + 1, error = %e, "Transient error, retrying");
                        last_error = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| PilotError::AgentExecution("Max retries exceeded".into())))
    }

    fn is_retryable(error: &PilotError) -> bool {
        match error {
            PilotError::AgentExecution(msg) => ExecutionError::from_message(msg).is_transient(),
            PilotError::Timeout(_) => true,
            _ => false,
        }
    }

    async fn call_llm_structured_inner<T>(&self, prompt: &str) -> Result<T>
    where
        T: schemars::JsonSchema + serde::de::DeserializeOwned + Send + 'static,
    {
        let model_config = self
            .agent_config
            .model_config_for(ModelAgentType::Evidence)?;
        let agent_timeout = Duration::from_secs(self.agent_config.timeout_secs);

        let agent = SdkAgent::builder()
            .auth(Auth::ClaudeCli)
            .await
            .map_err(|e| PilotError::Config(format!("Auth failed: {}", e)))?
            .model(model_config.model_id())
            .working_dir(&self.working_dir)
            .tools(ToolAccess::none())
            .timeout(agent_timeout)
            .permission_mode(PermissionMode::AcceptEdits)
            .structured_output::<T>()
            .build()
            .await
            .map_err(|e| PilotError::Config(format!("Agent build failed: {}", e)))?;

        let result = agent
            .execute(prompt)
            .await
            .map_err(|e| PilotError::AgentExecution(format!("LLM error: {}", e)))?;

        result
            .extract()
            .map_err(|e| PilotError::Planning(format!("Extraction failed: {}", e)))
    }

    fn execute_searches(&self, strategy: &SearchStrategy) -> Result<SearchResults> {
        let mut results = SearchResults::default();

        for query in &strategy.glob_patterns {
            results
                .files
                .extend(self.search_glob(&query.pattern, &query.reason));
        }

        for query in &strategy.content_patterns {
            match self.search_content(&query.pattern) {
                Ok((matches, hit_limit)) => {
                    results.content_matches.extend(matches);
                    if hit_limit {
                        results.hit_search_limit = true;
                    }
                }
                Err(e) => debug!(pattern = query.pattern, error = %e, "Content search failed"),
            }
        }

        for direct in &strategy.direct_paths {
            let full_path = self.working_dir.join(&direct.path);
            if full_path.exists() && full_path.is_file() {
                results.files.push(VerifiedFile {
                    path: PathBuf::from(&direct.path),
                    relevance: direct.reason.clone(),
                    confidence: self.config.confidence.direct_path,
                    discovery_method: DiscoveryMethod::DirectPath,
                });
            } else {
                results.invalid_paths.push(direct.path.clone());
            }
        }

        Ok(results)
    }

    fn search_glob(&self, pattern: &str, reason: &str) -> Vec<VerifiedFile> {
        let full_pattern = self.working_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let Ok(entries) = glob(&pattern_str) else {
            return Vec::new();
        };

        entries
            .flatten()
            .filter(|e| e.is_file())
            .filter_map(|e| {
                e.strip_prefix(&self.working_dir)
                    .ok()
                    .map(|p| p.to_path_buf())
            })
            .map(|path| VerifiedFile {
                path,
                relevance: reason.to_string(),
                confidence: self.config.confidence.glob_pattern,
                discovery_method: DiscoveryMethod::GlobPattern,
            })
            .collect()
    }

    fn search_content(&self, pattern: &str) -> Result<(Vec<ContentMatch>, bool)> {
        let max_matches = self.config.max_content_matches;
        // Use higher per-file limit to avoid false "hit limit" when many files have few matches.
        // --max-count is per-file, so we set it high and limit total results ourselves.
        let per_file_limit = max_matches.max(self.config.min_per_file_matches);

        let output = Command::new("rg")
            .args([
                "--json",
                "--max-count",
                &per_file_limit.to_string(),
                pattern,
            ])
            .current_dir(&self.working_dir)
            .output()
            .map_err(|e| PilotError::AgentExecution(format!("ripgrep failed: {}", e)))?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            if code == 1 {
                return Ok((Vec::new(), false));
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PilotError::AgentExecution(format!(
                "ripgrep error ({}): {}",
                code, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut matches = Vec::new();
        let mut files_with_max_hits = 0;

        // Track files that hit per-file limit (actual rg truncation)
        let mut current_file: Option<String> = None;
        let mut current_file_hits = 0;

        for line in stdout.lines() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
                && json["type"] == "match"
                && let (Some(path), Some(line_num), Some(text)) = (
                    json["data"]["path"]["text"].as_str(),
                    json["data"]["line_number"].as_u64(),
                    json["data"]["lines"]["text"].as_str(),
                )
            {
                // Track per-file hit count
                if current_file.as_deref() != Some(path) {
                    if current_file_hits >= per_file_limit {
                        files_with_max_hits += 1;
                    }
                    current_file = Some(path.to_string());
                    current_file_hits = 0;
                }
                current_file_hits += 1;

                matches.push(ContentMatch {
                    path: PathBuf::from(path),
                    line: line_num as u32,
                    snippet: text.trim().chars().take(100).collect(),
                });
            }
        }

        // Check last file
        if current_file_hits >= per_file_limit {
            files_with_max_hits += 1;
        }

        // Flag hit_limit if:
        // 1. Per-file truncation occurred (some files had more matches than limit)
        // 2. Total matches exceed max_matches (we're discarding data)
        let per_file_truncated = files_with_max_hits > 0;
        let total_exceeded = matches.len() > max_matches;
        let hit_limit = per_file_truncated || total_exceeded;

        if per_file_truncated {
            warn!(
                pattern,
                files_truncated = files_with_max_hits,
                "Content search hit per-file limit in {} file(s)",
                files_with_max_hits
            );
        }

        if total_exceeded {
            warn!(
                pattern,
                total = matches.len(),
                limit = max_matches,
                "Content search exceeded total limit, truncating results"
            );
            matches.truncate(max_matches);
        }

        Ok((matches, hit_limit))
    }
}

fn build_strategy_prompt(mission: &str, task: Option<&str>, previous: &SearchResults) -> String {
    let mut prompt = format!(
        r"Analyze codebase to gather evidence for implementing a mission.

## Mission
{mission}
"
    );

    if let Some(t) = task {
        prompt.push_str(&format!("\n## Current Task\n{t}\n"));
    }

    if !previous.files.is_empty() || !previous.content_matches.is_empty() {
        prompt.push_str("\n## Previous Results\n");
        prompt.push_str(&previous.to_feedback());
    }

    if !previous.invalid_paths.is_empty() {
        prompt.push_str("\n**Note**: Some paths don't exist. Adjust your strategy.\n");
    }

    prompt.push_str(
        "\n## Task\nGenerate search queries: glob patterns, content patterns, and direct file paths.\n",
    );

    prompt
}

fn build_evaluation_prompt(mission: &str, results: &SearchResults) -> String {
    format!(
        r"Evaluate gathered evidence for a mission.

## Mission
{mission}

## Evidence
{feedback}

## Task
Evaluate coverage and determine if more evidence is needed. If converged=false, include additional search queries.",
        feedback = results.to_feedback()
    )
}

fn build_refinement_prompt(mission: &str, task: Option<&str>, previous: &SearchResults) -> String {
    let mut prompt = format!(
        r"Search limits were hit while gathering evidence. Generate MORE SPECIFIC queries.

## Mission
{mission}
"
    );

    if let Some(t) = task {
        prompt.push_str(&format!("\n## Current Task\n{t}\n"));
    }

    prompt.push_str("\n## Current Results (Truncated)\n");
    prompt.push_str(&previous.to_feedback());

    prompt.push_str(
        r"
## Issue
Content search hit limits before finding all relevant code.

## Requirements
Generate REFINED queries that are:
1. MORE SPECIFIC - narrow down to exact modules/files
2. TARGETED - focus on the most critical evidence gaps
3. ALTERNATIVE APPROACHES - try different search patterns

Do NOT repeat broad patterns that already hit limits.
",
    );

    prompt
}

fn build_plateau_consultation_prompt(
    mission: &str,
    reason: &str,
    context: &SufficiencyContext,
) -> String {
    format!(
        r"Evidence gathering has reached a coverage plateau.

## Mission
{mission}

## Evidence Quality
{context_summary}

## Plateau Reason
{reason}

## Decision Required
1. Is the current evidence SUFFICIENT to proceed?
2. Would MORE TARGETED searches likely find critical missing evidence?

Set `should_continue=true` with `refined_strategy` if targeted search can help.
Set `should_continue=false` if current evidence is sufficient or further search unlikely to help.",
        context_summary = context.format_for_llm()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_results_merge() {
        let mut r1 = SearchResults::default();
        r1.files.push(VerifiedFile {
            path: PathBuf::from("a.rs"),
            relevance: "test".to_string(),
            confidence: 0.9,
            discovery_method: DiscoveryMethod::GlobPattern,
        });

        let mut r2 = SearchResults::default();
        r2.files.push(VerifiedFile {
            path: PathBuf::from("a.rs"),
            relevance: "dup".to_string(),
            confidence: 0.9,
            discovery_method: DiscoveryMethod::GlobPattern,
        });
        r2.files.push(VerifiedFile {
            path: PathBuf::from("b.rs"),
            relevance: "new".to_string(),
            confidence: 0.9,
            discovery_method: DiscoveryMethod::GlobPattern,
        });

        r1.merge(r2);
        assert_eq!(r1.files.len(), 2);
    }

    #[test]
    fn test_is_retryable() {
        // Only explicit HTTP codes and patterns are retryable
        assert!(EvidenceGatheringLoop::is_retryable(
            &PilotError::AgentExecution("429 Too Many Requests".into())
        ));
        assert!(EvidenceGatheringLoop::is_retryable(
            &PilotError::AgentExecution("HTTP 503: Service Unavailable".into())
        ));
        assert!(EvidenceGatheringLoop::is_retryable(&PilotError::Timeout(
            "operation timed out".into()
        )));
        // Ambiguous messages are not automatically retryable
        assert!(!EvidenceGatheringLoop::is_retryable(
            &PilotError::AgentExecution("connection timeout".into())
        ));
        assert!(!EvidenceGatheringLoop::is_retryable(&PilotError::Planning(
            "parse error".into()
        )));
    }

    #[test]
    fn test_discovery_method_serialization() {
        let file = VerifiedFile {
            path: PathBuf::from("test.rs"),
            relevance: "test".to_string(),
            confidence: 0.9,
            discovery_method: DiscoveryMethod::GlobPattern,
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("glob_pattern"));
    }
}
