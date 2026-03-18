pub(crate) mod analysis;
mod fixes;
mod rounds;
#[cfg(test)]
mod tests;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::checks::VerificationScope;
use super::issue::IssueExtractor;
use super::pattern_bank::PatternBank;
use super::semantic::SemanticValidator;
use super::verifier::Verifier;
use crate::agent::TaskAgent;
use crate::config::{ConvergentVerificationConfig, SearchConfig};
use crate::error::Result;
use crate::mission::Mission;
use crate::search::FixSearcher;

pub use types::{ConvergenceResult, PatternContext};

/// Parameters for building a fix prompt.
struct FixPromptParams<'a> {
    issue: &'a super::issue::Issue,
    mission: Option<&'a Mission>,
    strategy: super::history::FixStrategy,
    attempts: u32,
    similar_fixes_ctx: Option<&'a str>,
    pattern_ctx: Option<&'a types::PatternContext>,
    working_dir: &'a Path,
}

/// AI review context for convergent verification loop.
struct ReviewContext<'a> {
    mission: Option<&'a Mission>,
    description: &'a str,
    files_created: &'a [String],
    files_modified: &'a [String],
    diff: Option<&'a str>,
}

impl<'a> ReviewContext<'a> {
    fn from_mission(
        mission: &'a Mission,
        files_created: &'a [String],
        files_modified: &'a [String],
        diff: Option<&'a str>,
    ) -> Self {
        Self {
            mission: Some(mission),
            description: &mission.description,
            files_created,
            files_modified,
            diff,
        }
    }

    fn without_mission(
        description: &'a str,
        files_created: &'a [String],
        files_modified: &'a [String],
        diff: Option<&'a str>,
    ) -> Self {
        Self {
            mission: None,
            description,
            files_created,
            files_modified,
            diff,
        }
    }
}

pub struct ConvergentVerifier {
    config: ConvergentVerificationConfig,
    search_config: SearchConfig,
    base_verifier: Verifier,
    agent: Arc<TaskAgent>,
    index_dir: PathBuf,
    fix_searcher: FixSearcher,
    semantic_validator: SemanticValidator,
    issue_extractor: IssueExtractor,
    solvability: super::solvability::SolvabilityAnalyzer,
    pattern_bank: Option<Arc<PatternBank>>,
}

impl ConvergentVerifier {
    pub fn new(
        config: ConvergentVerificationConfig,
        search_config: SearchConfig,
        base_verifier: Verifier,
        agent: Arc<TaskAgent>,
        index_dir: PathBuf,
        working_dir: &Path,
    ) -> Self {
        let fix_searcher = FixSearcher::new(&index_dir);
        let semantic_validator = SemanticValidator::new(working_dir, search_config.timeout_secs);
        let issue_extractor = IssueExtractor::new(Arc::clone(&agent), working_dir);
        let solvability =
            super::solvability::SolvabilityAnalyzer::new(Arc::clone(&agent), working_dir);

        Self {
            config,
            search_config,
            base_verifier,
            agent,
            index_dir,
            fix_searcher,
            semantic_validator,
            issue_extractor,
            solvability,
            pattern_bank: None,
        }
    }

    pub fn with_pattern_bank(mut self, pattern_bank: Arc<PatternBank>) -> Self {
        self.pattern_bank = Some(pattern_bank);
        self
    }

    pub fn new_without_search(
        config: ConvergentVerificationConfig,
        base_verifier: Verifier,
        agent: Arc<TaskAgent>,
        working_dir: &Path,
    ) -> Self {
        let temp_dir = std::env::temp_dir().join("claude-pilot-search");
        let fix_searcher = FixSearcher::new(&temp_dir);
        let semantic_validator = SemanticValidator::new(working_dir, 10);
        let issue_extractor = IssueExtractor::new(Arc::clone(&agent), working_dir);
        let solvability =
            super::solvability::SolvabilityAnalyzer::new(Arc::clone(&agent), working_dir);

        Self {
            config,
            search_config: SearchConfig {
                enabled: false,
                ..Default::default()
            },
            base_verifier,
            agent,
            index_dir: temp_dir,
            fix_searcher,
            semantic_validator,
            issue_extractor,
            solvability,
            pattern_bank: None,
        }
    }

    /// Primary verification method for mission-based execution.
    pub async fn verify_until_convergent_with_mission_changes(
        &self,
        mission: &Mission,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let review_ctx = ReviewContext::from_mission(mission, files_created, files_modified, diff);
        self.run_convergent_loop_core(
            &mission.id,
            review_ctx,
            VerificationScope::Full,
            working_dir,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn verify_until_convergent_task_scoped(
        &self,
        mission: &Mission,
        test_patterns: Vec<String>,
        module: Option<String>,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let review_ctx = ReviewContext::from_mission(mission, files_created, files_modified, diff);
        self.run_convergent_loop_core(
            &mission.id,
            review_ctx,
            VerificationScope::for_task(test_patterns, module),
            working_dir,
            None,
        )
        .await
    }

    pub async fn verify_until_convergent_with_changes(
        &self,
        description: &str,
        files_created: &[String],
        files_modified: &[String],
        diff: Option<&str>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let id = format!("direct-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let review_ctx =
            ReviewContext::without_mission(description, files_created, files_modified, diff);
        self.run_convergent_loop_core(&id, review_ctx, VerificationScope::Full, working_dir, None)
            .await
    }

    pub async fn verify_until_convergent_recovery(
        &self,
        mission: &Mission,
        max_rounds_override: Option<u32>,
        working_dir: &Path,
    ) -> Result<ConvergenceResult> {
        let id = format!("recovery-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let review_ctx = ReviewContext::without_mission(&mission.description, &[], &[], None);
        self.run_convergent_loop_core(
            &id,
            review_ctx,
            VerificationScope::Full,
            working_dir,
            max_rounds_override,
        )
        .await
    }
}
