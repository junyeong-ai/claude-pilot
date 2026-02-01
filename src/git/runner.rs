use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;
use tracing::{debug, warn};

use crate::error::{PilotError, Result};

pub struct GitRunner {
    working_dir: PathBuf,
}

impl GitRunner {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    pub fn with_dir(&self, dir: &Path) -> Self {
        Self::new(dir)
    }

    pub async fn run(&self, args: &[&str]) -> Result<Output> {
        debug!(args = ?args, dir = %self.working_dir.display(), "Running git command");

        let output = Command::new("git")
            .args(args)
            .current_dir(&self.working_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(args = ?args, stderr = %stderr, "Git command failed");
        }

        Ok(output)
    }

    pub async fn run_checked(&self, args: &[&str]) -> Result<Output> {
        let output = self.run(args).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PilotError::Git(git2::Error::from_str(&stderr)));
        }

        Ok(output)
    }

    pub async fn add_all(&self) -> Result<()> {
        self.run_checked(&["add", "-A"]).await?;
        Ok(())
    }

    pub async fn commit(&self, message: &str) -> Result<bool> {
        let output = self.run(&["commit", "-m", message]).await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("nothing to commit") {
                return Ok(false);
            }
            return Err(PilotError::Git(git2::Error::from_str(&stderr)));
        }

        Ok(true)
    }

    pub async fn checkout(&self, branch: &str) -> Result<()> {
        self.run_checked(&["checkout", branch]).await?;
        Ok(())
    }

    pub async fn merge(&self, branch: &str, message: &str) -> Result<()> {
        self.run_checked(&["merge", "--no-ff", branch, "-m", message])
            .await?;
        Ok(())
    }

    pub async fn push(&self, remote: &str, branch: &str) -> Result<()> {
        self.run_checked(&["push", "-u", remote, branch]).await?;
        Ok(())
    }

    pub async fn diff_stat(&self, base: &str) -> Result<String> {
        let output = self.run(&["diff", "--stat", base]).await?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get the actual diff content for uncommitted changes.
    /// Returns unified diff format showing what was changed.
    pub async fn diff_unstaged(&self) -> Result<String> {
        // Get both staged and unstaged changes
        let output = self.run(&["diff", "HEAD"]).await?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get diff for specific files only.
    /// Useful for AI review to see exactly what changed.
    pub async fn diff_files(&self, files: &[String]) -> Result<String> {
        if files.is_empty() {
            return Ok(String::new());
        }

        let mut args = vec!["diff", "HEAD", "--"];
        for file in files {
            args.push(file.as_str());
        }

        let output = self.run(&args).await?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get diff for review including both modified and newly created files.
    /// Modified files use git diff; created files show full content as unified diff.
    pub async fn diff_for_review(
        &self,
        files_modified: &[String],
        files_created: &[String],
    ) -> Option<String> {
        let mut parts = Vec::new();

        // Modified files: use git diff
        if !files_modified.is_empty() {
            match self.diff_files(files_modified).await {
                Ok(d) if !d.is_empty() => parts.push(d),
                Ok(_) => {}
                Err(e) => {
                    warn!(error = %e, "Failed to compute git diff for modified files");
                }
            }
        }

        // Created files: show full content as new file diff
        for file in files_created {
            let path = self.working_dir.join(file);
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => {
                    let diff = Self::format_new_file_diff(file, &content);
                    parts.push(diff);
                }
                Err(e) => {
                    warn!(file = %file, error = %e, "Failed to read created file for diff");
                }
            }
        }

        if parts.is_empty() {
            None
        } else {
            debug!(
                modified_count = files_modified.len(),
                created_count = files_created.len(),
                total_len = parts.iter().map(|p| p.len()).sum::<usize>(),
                "Computed diff for AI review"
            );
            Some(parts.join("\n"))
        }
    }

    /// Format file content as unified diff for a new file.
    fn format_new_file_diff(file: &str, content: &str) -> String {
        let line_count = content.lines().count();
        let mut output = format!(
            "diff --git a/{file} b/{file}\nnew file mode 100644\n--- /dev/null\n+++ b/{file}\n@@ -0,0 +1,{line_count} @@\n"
        );
        for line in content.lines() {
            output.push('+');
            output.push_str(line);
            output.push('\n');
        }
        output
    }

    pub async fn log(&self, base: &str, limit: usize) -> Result<String> {
        let output = self
            .run(&[
                "log",
                &format!("{}..HEAD", base),
                &format!("-{}", limit),
                "--oneline",
            ])
            .await?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub async fn branch_exists(&self, branch: &str) -> Result<bool> {
        let output = self
            .run(&["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
            .await?;
        Ok(output.status.success())
    }

    pub async fn delete_branch(&self, branch: &str) -> Result<bool> {
        let output = self.run(&["branch", "-D", branch]).await?;
        Ok(output.status.success())
    }

    pub async fn list_branches_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let output = self
            .run(&["branch", "--list", &format!("{}*", prefix)])
            .await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .map(|l| l.trim().trim_start_matches("* ").to_string())
            .filter(|l| !l.is_empty())
            .collect())
    }

    pub async fn worktree_add(&self, path: &Path, branch: &str, base: &str) -> Result<()> {
        let path_str = path
            .to_str()
            .ok_or_else(|| PilotError::Other("Invalid path encoding".into()))?;

        let output = if self.branch_exists(branch).await? {
            self.run(&["worktree", "add", path_str, branch]).await?
        } else {
            self.run(&["worktree", "add", "-b", branch, path_str, base])
                .await?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PilotError::Worktree {
                message: stderr.to_string(),
                path: path.to_path_buf(),
            });
        }

        Ok(())
    }

    pub async fn worktree_remove(&self, path: &Path) -> Result<()> {
        let path_str = path
            .to_str()
            .ok_or_else(|| PilotError::Other("Invalid path encoding".into()))?;

        let output = self
            .run(&["worktree", "remove", "--force", path_str])
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PilotError::Worktree {
                message: stderr.to_string(),
                path: path.to_path_buf(),
            });
        }

        Ok(())
    }
}

pub struct GhRunner {
    working_dir: PathBuf,
}

impl GhRunner {
    pub fn new(working_dir: impl Into<PathBuf>) -> Self {
        Self {
            working_dir: working_dir.into(),
        }
    }

    pub async fn create_pr(&self, title: &str, body: &str, reviewers: &[String]) -> Result<String> {
        let mut args = vec!["pr", "create", "--title", title, "--body", body];

        let reviewer_args: Vec<String> = reviewers
            .iter()
            .flat_map(|r| vec!["--reviewer".to_string(), r.clone()])
            .collect();

        let reviewer_refs: Vec<&str> = reviewer_args.iter().map(|s| s.as_str()).collect();
        args.extend(reviewer_refs);

        debug!(args = ?args, "Running gh command");

        let output = Command::new("gh")
            .args(&args)
            .current_dir(&self.working_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(PilotError::Other(format!(
                "Failed to create PR: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}
