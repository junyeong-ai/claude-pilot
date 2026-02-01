use std::ffi::OsStr;
use std::path::Path;

use tracing::{debug, info, warn};
use walkdir::WalkDir;

use crate::config::ContextEstimatorConfig;
use crate::utils::estimate_tokens;

/// Context estimation result for pre-flight checks.
#[derive(Debug, Clone)]
pub struct ContextEstimate {
    /// Total estimated context size including all resources.
    pub total_estimated: usize,
    /// Estimated tokens from skills directory.
    pub skills_tokens: usize,
    /// Estimated tokens from commands directory.
    pub commands_tokens: usize,
    /// Estimated tokens from agents directory.
    pub agents_tokens: usize,
    /// Estimated tokens from CLAUDE.md file.
    pub claude_md_tokens: usize,
    /// Estimated base overhead (system prompt + tool definitions).
    pub base_overhead: usize,
    /// Maximum context window for the model.
    pub max_context: usize,
    /// Available budget for actual work.
    pub available_budget: usize,
    /// Recommendation for resource loading strategy.
    pub recommendation: LoadingRecommendation,
}

/// Recommendation for how to load MCP resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadingRecommendation {
    /// Safe to load all resources.
    LoadAll,
    /// Load only essential resources (CLAUDE.md, relevant skills).
    LoadSelective,
    /// Too large - skip MCP resources entirely for this operation.
    SkipMcpResources,
    /// Context already at limit - emergency mode.
    Emergency,
}

impl LoadingRecommendation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoadAll => "load_all",
            Self::LoadSelective => "load_selective",
            Self::SkipMcpResources => "skip_mcp",
            Self::Emergency => "emergency",
        }
    }
}

/// Estimates context size for a given working directory.
pub struct ContextEstimator {
    max_context: usize,
    config: ContextEstimatorConfig,
}

impl Default for ContextEstimator {
    fn default() -> Self {
        Self::with_config(200_000, ContextEstimatorConfig::default())
    }
}

impl ContextEstimator {
    /// Create a new estimator with the specified max context window.
    pub fn new(max_context: usize) -> Self {
        Self::with_config(max_context, ContextEstimatorConfig::default())
    }

    /// Create a new estimator with full configuration.
    pub fn with_config(max_context: usize, config: ContextEstimatorConfig) -> Self {
        Self {
            max_context,
            config,
        }
    }

    /// Estimate context size for the given working directory.
    pub async fn estimate(&self, working_dir: &Path) -> ContextEstimate {
        let skills_tokens = self
            .estimate_directory(working_dir.join(".claude/skills"))
            .await;
        let commands_tokens = self
            .estimate_directory(working_dir.join(".claude/commands"))
            .await;
        let agents_tokens = self
            .estimate_directory(working_dir.join(".claude/agents"))
            .await;
        let claude_md_tokens = self.estimate_file(working_dir.join("CLAUDE.md")).await;

        let mcp_total = skills_tokens + commands_tokens + agents_tokens + claude_md_tokens;
        let total_estimated = self.config.base_overhead_tokens + mcp_total;

        let usable_context = (self.max_context as f32 * self.config.safety_margin) as usize;
        let available_budget = usable_context.saturating_sub(total_estimated);

        let recommendation = self.determine_recommendation(total_estimated, available_budget);

        info!(
            skills_tokens,
            commands_tokens,
            agents_tokens,
            claude_md_tokens,
            mcp_total,
            total_estimated,
            available_budget,
            recommendation = recommendation.as_str(),
            "Context estimation complete"
        );

        ContextEstimate {
            total_estimated,
            skills_tokens,
            commands_tokens,
            agents_tokens,
            claude_md_tokens,
            base_overhead: self.config.base_overhead_tokens,
            max_context: self.max_context,
            available_budget,
            recommendation,
        }
    }

    /// Quick check if MCP resources would likely cause overflow.
    pub async fn would_overflow(&self, working_dir: &Path) -> bool {
        let estimate = self.estimate(working_dir).await;
        estimate.recommendation != LoadingRecommendation::LoadAll
    }

    fn determine_recommendation(
        &self,
        total_estimated: usize,
        available_budget: usize,
    ) -> LoadingRecommendation {
        let usable = (self.max_context as f32 * self.config.safety_margin) as usize;

        // If total exceeds usable context, we need to skip resources
        if total_estimated > usable {
            if total_estimated > self.max_context {
                return LoadingRecommendation::Emergency;
            }
            return LoadingRecommendation::SkipMcpResources;
        }

        // If available budget is too small for useful work
        if available_budget < self.config.min_working_budget {
            return LoadingRecommendation::LoadSelective;
        }

        // If we're using more than selective_loading_ratio of context for base + MCP
        let selective_threshold = (usable as f32 * self.config.selective_loading_ratio) as usize;
        if total_estimated > selective_threshold {
            return LoadingRecommendation::LoadSelective;
        }

        LoadingRecommendation::LoadAll
    }

    async fn estimate_directory(&self, dir: impl AsRef<Path>) -> usize {
        let dir = dir.as_ref();
        if !dir.exists() {
            return 0;
        }

        let mut total_tokens = 0;
        let mut file_count = 0;
        let max_files_per_dir = self.config.max_files_per_directory;

        for entry in WalkDir::new(dir)
            .max_depth(self.config.max_directory_depth)
            .into_iter()
            .filter_entry(|e| !is_skippable_directory(e.file_name()))
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.is_file() {
                // Skip binary files (deterministic check, no content read)
                if is_likely_binary(path) {
                    continue;
                }

                let tokens = self.estimate_file(path).await;
                total_tokens += tokens;
                file_count += 1;

                if tokens > self.config.large_file_warning_threshold {
                    debug!(
                        path = %path.display(),
                        tokens,
                        "Large file detected"
                    );
                }

                // Early termination: extrapolate if too many files
                if file_count >= max_files_per_dir {
                    debug!(
                        dir = %dir.display(),
                        file_count,
                        "Hit file cap, extrapolating"
                    );
                    // Add 20% buffer for extrapolation uncertainty
                    total_tokens = (total_tokens as f32 * 1.2) as usize;
                    break;
                }
            }
        }

        debug!(
            dir = %dir.display(),
            file_count,
            total_tokens,
            "Directory estimation complete"
        );

        total_tokens
    }

    async fn estimate_file(&self, path: impl AsRef<Path>) -> usize {
        let path = path.as_ref();
        if !path.exists() {
            return 0;
        }

        match tokio::fs::read_to_string(path).await {
            Ok(content) => estimate_tokens(&content),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to read file for estimation");
                // Fallback: estimate based on file size using configured chars-per-token ratio
                match tokio::fs::metadata(path).await {
                    Ok(meta) => (meta.len() as usize) / self.config.fallback_chars_per_token,
                    Err(_) => 0,
                }
            }
        }
    }

    /// Estimate tokens for a set of specific files (for selective loading).
    pub async fn estimate_files(&self, files: &[impl AsRef<Path>]) -> usize {
        let mut total = 0;
        for file in files {
            total += self.estimate_file(file).await;
        }
        total
    }
}

impl ContextEstimate {
    /// Check if loading all resources is safe.
    pub fn is_safe_to_load_all(&self) -> bool {
        self.recommendation == LoadingRecommendation::LoadAll
    }

    /// Get MCP resources total (skills + commands + agents + claude_md).
    pub fn mcp_resources_total(&self) -> usize {
        self.skills_tokens + self.commands_tokens + self.agents_tokens + self.claude_md_tokens
    }

    /// Calculate available budget after reserving for output.
    pub fn available_for_prompt(&self, output_reserve: usize) -> usize {
        self.available_budget.saturating_sub(output_reserve)
    }

    /// Format as human-readable summary.
    pub fn summary(&self) -> String {
        format!(
            "Context: {}/{}k ({:.1}%) | MCP: {}k | Available: {}k | Recommendation: {}",
            self.total_estimated / 1000,
            self.max_context / 1000,
            (self.total_estimated as f64 / self.max_context as f64) * 100.0,
            self.mcp_resources_total() / 1000,
            self.available_budget / 1000,
            self.recommendation.as_str()
        )
    }
}

/// Check if a directory should be skipped during scanning.
///
/// **Conservative approach**: Only skip directories that are UNIVERSALLY safe to skip
/// across ALL languages, frameworks, and project types.
///
/// ## Safe to skip (universal):
/// - Version control metadata (not user code)
/// - Package manager caches (dependencies, not user code)
/// - Python bytecode cache (generated, not source)
///
/// ## NOT safe to skip (project-dependent):
/// - `build`, `dist`, `target` - may contain build scripts or config
/// - `env` - may be environment config, not Python venv
/// - `tests`, `test` - user may want to estimate test code
fn is_skippable_directory(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    matches!(
        name.as_ref(),
        // Version control - never user code
        ".git" | ".svn" | ".hg" |
        // Package manager caches - dependencies, not user code
        "node_modules" |
        // Python bytecode - generated from .py files
        "__pycache__"
    )
}

/// Check if a file is likely binary (shouldn't be read for tokens).
///
/// **Conservative approach**: Only skip files that are DEFINITELY binary.
/// When uncertain, include the file (may overestimate, but won't miss content).
///
/// ## Definitely binary (universal):
/// - Compiled executables and libraries (.exe, .dll, .so, .dylib)
/// - Compressed archives (.zip, .tar.gz)
/// - Compiled bytecode (.pyc, .class)
/// - Media files (.png, .jpg, .mp4)
///
/// ## NOT considered binary:
/// - .svg (XML text)
/// - Files without extension (may be scripts)
fn is_likely_binary(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false; // No extension - could be script, include it
    };

    matches!(
        ext.to_lowercase().as_str(),
        // Compiled executables/libraries (universal binary formats)
        "exe" | "dll" | "so" | "dylib" | "a" | "o" | "obj" |
        // Compiled bytecode
        "pyc" | "pyo" | "class" |
        // Archives (compressed, unreadable as text)
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar" |
        // Images (binary pixel data)
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "tiff" |
        // Audio/Video (binary media)
        "mp3" | "mp4" | "wav" | "avi" | "mov" | "mkv" | "flac" |
        // Fonts (binary glyph data)
        "ttf" | "otf" | "woff" | "woff2" |
        // Java archives
        "jar" | "war" | "ear" |
        // Databases
        "db" | "sqlite" | "sqlite3" |
        // WebAssembly
        "wasm" // NOTE: .svg is NOT included (it's XML text)
               // NOTE: .pdf is NOT included (may want to flag large files)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_empty_directory() {
        let temp = TempDir::new().unwrap();
        let config = ContextEstimatorConfig::default();
        let estimator = ContextEstimator::with_config(200_000, config.clone());
        let estimate = estimator.estimate(temp.path()).await;

        assert_eq!(estimate.skills_tokens, 0);
        assert_eq!(estimate.commands_tokens, 0);
        assert_eq!(estimate.total_estimated, config.base_overhead_tokens);
        assert_eq!(estimate.recommendation, LoadingRecommendation::LoadAll);
    }

    #[tokio::test]
    async fn test_recommendation_thresholds() {
        let config = ContextEstimatorConfig::default();
        let estimator = ContextEstimator::with_config(200_000, config.clone());

        // Well under limit
        assert_eq!(
            estimator.determine_recommendation(50_000, 130_000),
            LoadingRecommendation::LoadAll
        );

        // Using 70%+ of context (selective_loading_ratio = 0.7)
        assert_eq!(
            estimator.determine_recommendation(140_000, 40_000),
            LoadingRecommendation::LoadSelective
        );

        // Too small working budget (< min_working_budget=30_000) → LoadSelective
        assert_eq!(
            estimator.determine_recommendation(160_000, 20_000),
            LoadingRecommendation::LoadSelective
        );

        // Exceeds usable context (180_000 with safety_margin=0.9) → SkipMcpResources
        assert_eq!(
            estimator.determine_recommendation(185_000, 0),
            LoadingRecommendation::SkipMcpResources
        );
    }
}
