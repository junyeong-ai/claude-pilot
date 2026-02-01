use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::config::AgentConfig;
use crate::error::{PilotError, Result};
use crate::planning::{
    CodebaseAnalysis, DependencyAnalysis, Evidence, EvidenceCompleteness, EvidenceSource,
    PriorKnowledge, RelevantFile, TestStructure,
};
use crate::quality::{EvidenceGatheringConfig, EvidenceSufficiencyConfig};
use crate::search::{AnalysisIndex, ChunkType, IndexedChunk};

use super::filter::EvidenceFilter;
use super::gathering::{DiscoveryMethod, EvidenceGatheringLoop, GatheringResult, VerifiedFile};
use super::sufficiency::{CoverageResult, EvidenceRequirement, EvidenceSufficiencyChecker};

pub struct EnhancedEvidenceGatherer {
    working_dir: PathBuf,
    gathering_loop: EvidenceGatheringLoop,
    sufficiency_checker: EvidenceSufficiencyChecker,
    filter: EvidenceFilter,
    index: Option<Arc<RwLock<AnalysisIndex>>>,
    cache_relevance_threshold: f32,
}

impl EnhancedEvidenceGatherer {
    pub fn new(
        working_dir: &Path,
        gathering_config: EvidenceGatheringConfig,
        sufficiency_config: EvidenceSufficiencyConfig,
        agent_config: AgentConfig,
        exclude_dirs: &[String],
    ) -> Self {
        let cache_relevance_threshold = gathering_config.cache_relevance_threshold;
        Self {
            working_dir: working_dir.to_path_buf(),
            gathering_loop: EvidenceGatheringLoop::new(working_dir, gathering_config, agent_config),
            sufficiency_checker: EvidenceSufficiencyChecker::new(sufficiency_config),
            filter: EvidenceFilter::new(working_dir, exclude_dirs),
            index: None,
            cache_relevance_threshold,
        }
    }

    pub fn with_index(mut self, index: Arc<RwLock<AnalysisIndex>>) -> Self {
        self.index = Some(index);
        self
    }

    pub async fn gather(&self, description: &str, task: Option<&str>) -> Result<Evidence> {
        info!(
            description = description,
            task = task,
            "Starting evidence gathering"
        );

        if let Some(ref index) = self.index
            && let Some(cached) = self.check_cache(index, description).await
        {
            info!("Using cached evidence");
            return Ok(cached);
        }

        let mut result = self.gathering_loop.gather(description, task).await?;

        let before_filter = result.files.len();
        result
            .files
            .retain(|f| self.filter.is_path_allowed(&f.path));
        let after_filter = result.files.len();

        debug!(
            iteration_count = result.iteration_count,
            converged = result.converged,
            coverage = result.coverage_score,
            files_before = before_filter,
            files_after = after_filter,
            "Gathering complete (filtered)"
        );

        let evidence = convert_to_evidence(&result, &self.working_dir);

        let requirements = self
            .sufficiency_checker
            .generate_requirements_from_mission(description, Path::new("."));

        match self
            .sufficiency_checker
            .check_coverage(&requirements, &evidence)
        {
            Ok(coverage) => {
                info!(coverage = coverage.coverage_ratio, "Evidence sufficient");
                if !coverage.sufficient {
                    warn!(missing = coverage.missing.len(), "Coverage below threshold");
                }
            }
            Err(e) => {
                warn!(
                    coverage = e.coverage,
                    required = e.required,
                    "Insufficient evidence"
                );
                return Err(PilotError::EvidenceGathering(format!(
                    "Insufficient evidence: {:.1}% (required: {:.1}%)",
                    e.coverage * 100.0,
                    e.required * 100.0
                )));
            }
        }

        if let Some(ref index) = self.index {
            cache_evidence(index, description, &evidence).await;
        }

        Ok(evidence)
    }

    pub async fn gather_with_requirements(
        &self,
        description: &str,
        task: Option<&str>,
        requirements: &[EvidenceRequirement],
    ) -> Result<(Evidence, CoverageResult)> {
        let result = self.gathering_loop.gather(description, task).await?;
        let evidence = convert_to_evidence(&result, &self.working_dir);

        let coverage = self
            .sufficiency_checker
            .check_coverage(requirements, &evidence)
            .map_err(|e| {
                PilotError::EvidenceGathering(format!(
                    "Insufficient evidence: {:.1}% (required {:.1}%)",
                    e.coverage * 100.0,
                    e.required * 100.0
                ))
            })?;

        Ok((evidence, coverage))
    }

    pub fn index(&self) -> Option<&Arc<RwLock<AnalysisIndex>>> {
        self.index.as_ref()
    }

    async fn check_cache(
        &self,
        index: &Arc<RwLock<AnalysisIndex>>,
        query: &str,
    ) -> Option<Evidence> {
        let index = index.read().await;
        let results = index.search(query);

        let threshold = self.cache_relevance_threshold;
        let relevant: Vec<_> = results
            .into_iter()
            .filter(|r| r.score > threshold)
            .collect();

        if relevant.is_empty() {
            return None;
        }

        let mut files = Vec::new();
        let mut patterns = Vec::new();
        let mut keywords = Vec::new();
        let mut skipped_count = 0;

        for result in relevant {
            match result.chunk.chunk_type {
                ChunkType::FileAnalysis => {
                    for path in &result.chunk.file_paths {
                        // Verify cached file still exists
                        if !path.exists() {
                            debug!(path = %path.display(), "Cached file no longer exists");
                            skipped_count += 1;
                            continue;
                        }

                        files.push(RelevantFile {
                            path: path.to_string_lossy().to_string(),
                            relevance: "Cached".to_string(),
                            summary: result.chunk.content.chars().take(100).collect(),
                            // Correctly mark as cache-derived, not filesystem-verified
                            source: EvidenceSource::PriorKnowledge {
                                source_path: "analysis_cache".to_string(),
                            },
                            // Reduce confidence for cached items
                            confidence: result.score * 0.9,
                        });
                    }
                }
                ChunkType::PatternAnalysis => {
                    patterns.push(result.chunk.content.clone());
                }
                ChunkType::EvidenceSummary => {
                    keywords.extend(result.chunk.keywords.clone());
                }
                _ => {}
            }
        }

        if skipped_count > 0 {
            debug!(skipped_count, "Skipped non-existent cached files");
        }

        if files.is_empty() {
            return None;
        }

        keywords.sort();
        keywords.dedup();
        keywords.truncate(10);

        let affected_areas = extract_affected_areas_from_paths(&files);

        Some(Evidence {
            codebase_analysis: CodebaseAnalysis {
                relevant_files: files,
                existing_patterns: patterns,
                conventions: Vec::new(),
                affected_areas,
                test_structure: TestStructure::default(),
            },
            dependency_analysis: DependencyAnalysis::default(),
            prior_knowledge: PriorKnowledge::default(),
            gathered_at: Utc::now(),
            completeness: EvidenceCompleteness::complete(),
        })
    }
}

fn extract_affected_areas_from_paths(files: &[RelevantFile]) -> Vec<String> {
    let mut areas = HashSet::new();
    for file in files {
        if let Some(parent) = Path::new(&file.path).parent() {
            let parent_str = parent.to_string_lossy();
            if !parent_str.is_empty() {
                areas.insert(parent_str.to_string());
            }
        }
    }
    areas.into_iter().collect()
}

fn extract_file_summary(working_dir: &Path, file_path: &Path) -> String {
    use std::io::{BufRead, BufReader};

    let full_path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        working_dir.join(file_path)
    };

    let Ok(file) = std::fs::File::open(&full_path) else {
        return String::new();
    };

    let reader = BufReader::new(file);
    let mut summary_lines = Vec::new();

    // Collect first non-empty lines as summary
    for line in reader.lines().take(30).flatten() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && summary_lines.len() < 5 {
            summary_lines.push(trimmed.to_string());
        }
        if summary_lines.len() >= 5 {
            break;
        }
    }

    summary_lines.join(" | ")
}

fn convert_to_evidence(result: &GatheringResult, working_dir: &Path) -> Evidence {
    let relevant_files = result
        .files
        .iter()
        .map(|f| RelevantFile {
            path: f.path.to_string_lossy().to_string(),
            relevance: f.relevance.clone(),
            summary: extract_file_summary(working_dir, &f.path),
            source: match f.discovery_method {
                DiscoveryMethod::DirectPath | DiscoveryMethod::GlobPattern => {
                    EvidenceSource::FilesystemStructure
                }
                DiscoveryMethod::ContentMatch => EvidenceSource::PatternDetection {
                    pattern: f.relevance.clone(),
                },
            },
            confidence: f.confidence,
        })
        .collect();

    let existing_patterns = result
        .patterns
        .iter()
        .map(|p| format!("{}: {}", p.name, p.description))
        .collect();

    let affected_areas = extract_affected_areas(&result.files);

    Evidence {
        codebase_analysis: CodebaseAnalysis {
            relevant_files,
            existing_patterns,
            conventions: Vec::new(),
            affected_areas,
            test_structure: TestStructure::default(),
        },
        dependency_analysis: DependencyAnalysis::default(),
        prior_knowledge: PriorKnowledge::default(),
        gathered_at: Utc::now(),
        completeness: {
            let files_found = result.files.len();
            // Evidence is complete if files were found and no search limit was hit.
            // Convergence is a nice-to-have but not required - the sufficiency check
            // (which runs after this) determines if we have enough evidence.
            // The key quality gate is the sufficiency checker, not convergence.
            let is_complete = !result.hit_search_limit && files_found > 0;

            let mut c = EvidenceCompleteness {
                is_complete,
                files_found,
                files_limit: 0, // No hard limit in convergent gathering
                warnings: Vec::new(),
            };

            // Convergence is informational only - sufficiency check is the real gate
            if result.hit_search_limit {
                c.add_warning(
                    "Search hit per-file limit - some files may have additional matches"
                        .to_string(),
                );
            }
            if files_found == 0 {
                c.add_warning("No relevant files found".to_string());
            }
            c
        },
    }
}

fn extract_affected_areas(files: &[VerifiedFile]) -> Vec<String> {
    let mut areas = HashSet::new();
    for file in files {
        let path_str = file.path.to_string_lossy();
        let parts: Vec<&str> = path_str.split('/').filter(|p| !p.is_empty()).collect();
        if let Some(first) = parts.first() {
            areas.insert(first.to_string());
        }
        if parts.len() >= 2 {
            areas.insert(format!("{}/{}", parts[0], parts[1]));
        }
    }
    areas.into_iter().collect()
}

async fn cache_evidence(
    index: &Arc<RwLock<AnalysisIndex>>,
    description: &str,
    evidence: &Evidence,
) {
    let mut index = index.write().await;

    for file in &evidence.codebase_analysis.relevant_files {
        let chunk = IndexedChunk::new(
            format!("file:{}", file.path),
            ChunkType::FileAnalysis,
            format!("{}: {}", file.relevance, file.summary),
        )
        .with_file(&file.path)
        .with_relevance(file.confidence);

        index.add(chunk);
    }

    for (i, pattern) in evidence
        .codebase_analysis
        .existing_patterns
        .iter()
        .enumerate()
    {
        let chunk = IndexedChunk::new(
            format!("pattern:{}", i),
            ChunkType::PatternAnalysis,
            pattern.clone(),
        );

        index.add(chunk);
    }

    let summary = IndexedChunk::new(
        format!("evidence:{}", crate::utils::truncate_str(description, 50)),
        ChunkType::EvidenceSummary,
        description.to_string(),
    )
    .with_files(
        evidence
            .codebase_analysis
            .relevant_files
            .iter()
            .map(|f| PathBuf::from(&f.path))
            .collect(),
    );

    index.add(summary);
    debug!("Evidence cached");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        fs::write(src.join("auth.rs"), "pub struct Auth {}").unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        dir
    }

    #[tokio::test]
    async fn test_with_caching() {
        let project = create_test_project();
        let index = Arc::new(RwLock::new(AnalysisIndex::new()));

        let gatherer = EnhancedEvidenceGatherer::new(
            project.path(),
            EvidenceGatheringConfig::default(),
            EvidenceSufficiencyConfig {
                min_coverage_ratio: 0.1,
                halt_on_insufficient: false,
                ..Default::default()
            },
            AgentConfig::default(),
            &[],
        )
        .with_index(index.clone());

        assert!(gatherer.index().is_some());
    }
}
