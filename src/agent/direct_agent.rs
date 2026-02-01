use std::path::Path;
use std::sync::Arc;

use tracing::{debug, info};

use super::{PromptBuilder, TaskAgent};
use crate::error::Result;
use crate::git::GitRunner;
use crate::verification::{ConvergentVerifier, FileTracker};

#[derive(Debug)]
pub struct DirectResult {
    pub success: bool,
    pub output: String,
    pub files_created: Vec<String>,
    pub files_modified: Vec<String>,
    pub convergence_rounds: u32,
}

impl DirectResult {
    pub fn success(
        output: String,
        files_created: Vec<String>,
        files_modified: Vec<String>,
    ) -> Self {
        Self {
            success: true,
            output,
            files_created,
            files_modified,
            convergence_rounds: 0,
        }
    }

    pub fn failure(reason: impl Into<String>) -> Self {
        Self {
            success: false,
            output: reason.into(),
            files_created: Vec::new(),
            files_modified: Vec::new(),
            convergence_rounds: 0,
        }
    }

    pub fn with_convergence_rounds(mut self, rounds: u32) -> Self {
        self.convergence_rounds = rounds;
        self
    }
}

pub struct DirectAgent {
    task_agent: Arc<TaskAgent>,
    convergent_verifier: Arc<ConvergentVerifier>,
    prompt_builder: PromptBuilder,
}

impl DirectAgent {
    pub fn new(task_agent: Arc<TaskAgent>, convergent_verifier: Arc<ConvergentVerifier>) -> Self {
        Self {
            task_agent,
            convergent_verifier,
            prompt_builder: PromptBuilder::default(),
        }
    }

    pub async fn execute(&self, description: &str, working_dir: &Path) -> Result<DirectResult> {
        info!(description = %description, "Starting direct execution");

        let mut file_tracker = FileTracker::new();
        file_tracker.capture_before(working_dir);

        let prompt = self.prompt_builder.build_direct_prompt(description);
        let output = self.task_agent.run_prompt(&prompt, working_dir).await?;

        file_tracker.capture_after(working_dir);
        let changes = file_tracker.changes();

        let files_created: Vec<String> = changes
            .created
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let files_modified: Vec<String> = changes
            .modified
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        debug!(
            output_len = output.len(),
            files_created = files_created.len(),
            files_modified = files_modified.len(),
            "Direct execution completed"
        );

        // Compute git diff for AI review (includes both modified and created files)
        let diff = GitRunner::new(working_dir)
            .diff_for_review(&files_modified, &files_created)
            .await;

        // Run convergent verification to ensure 2-pass requirement (NON-NEGOTIABLE)
        // Convergent loop handles all verification internally - no separate verify_full needed
        info!(
            files_created = files_created.len(),
            files_modified = files_modified.len(),
            has_diff = diff.is_some(),
            "Running convergent verification with AI review for 2-pass requirement"
        );

        match self
            .convergent_verifier
            .verify_until_convergent_with_changes(
                description,
                &files_created,
                &files_modified,
                diff.as_deref(),
                working_dir,
            )
            .await
        {
            Ok(convergence) => {
                if convergence.converged {
                    info!(
                        rounds = convergence.total_rounds,
                        "Convergent verification succeeded"
                    );
                    Ok(DirectResult::success(output, files_created, files_modified)
                        .with_convergence_rounds(convergence.total_rounds))
                } else {
                    info!(
                        rounds = convergence.total_rounds,
                        "Convergent verification failed"
                    );
                    Ok(DirectResult::failure(format!(
                        "Failed to converge after {} rounds",
                        convergence.total_rounds
                    ))
                    .with_convergence_rounds(convergence.total_rounds))
                }
            }
            Err(e) => {
                info!(error = %e, "Convergent verification error");
                Ok(DirectResult::failure(format!(
                    "Convergent verification error: {}",
                    e
                )))
            }
        }
    }
}
