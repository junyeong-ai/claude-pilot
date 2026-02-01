use std::path::PathBuf;

use git2::{BranchType, Repository};
use tokio::fs;
use tracing::{debug, info, warn};

use crate::config::ProjectPaths;
use crate::error::{PilotError, Result};
use crate::git::{GhRunner, GitRunner};
use crate::mission::{IsolationMode, Mission};

pub struct IsolationManager {
    repo_path: PathBuf,
    worktrees_dir: PathBuf,
}

impl IsolationManager {
    pub fn new(paths: &ProjectPaths) -> Self {
        Self {
            repo_path: paths.root.clone(),
            worktrees_dir: paths.worktrees_dir.clone(),
        }
    }

    fn git(&self) -> GitRunner {
        GitRunner::new(&self.repo_path)
    }

    pub async fn cleanup_orphaned(&self, active_mission_ids: &[String]) -> Result<()> {
        if !self.worktrees_dir.exists() {
            return Ok(());
        }

        let mut dir = fs::read_dir(&self.worktrees_dir).await?;
        let mut orphaned = Vec::new();

        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if !active_mission_ids.contains(&name) {
                orphaned.push(entry.path());
            }
        }

        for path in orphaned {
            warn!(path = %path.display(), "Cleaning up orphaned worktree");
            if let Err(e) = self.git().worktree_remove(&path).await {
                debug!(path = %path.display(), error = %e, "Git worktree remove failed, using force remove");
                if let Err(e) = fs::remove_dir_all(&path).await {
                    warn!(path = %path.display(), error = %e, "Force remove failed");
                }
            }
        }

        Ok(())
    }

    pub async fn setup(&self, mission: &mut Mission) -> Result<()> {
        match mission.isolation {
            IsolationMode::None | IsolationMode::Auto => Ok(()),
            IsolationMode::Branch => self.setup_branch(mission).await,
            IsolationMode::Worktree => self.setup_worktree(mission).await,
        }
    }

    pub async fn cleanup(&self, mission: &Mission, delete_branch: bool) -> Result<()> {
        if mission.isolation == IsolationMode::Worktree {
            self.cleanup_worktree(mission).await?;
        }

        if delete_branch
            && let Some(branch) = &mission.branch
            && self.git().delete_branch(branch).await?
        {
            info!(branch = %branch, "Deleted branch");
        }

        Ok(())
    }

    pub async fn cleanup_orphaned_branches(
        &self,
        active_mission_ids: &[String],
        branch_prefix: &str,
    ) -> Result<Vec<String>> {
        let branches = self.git().list_branches_with_prefix(branch_prefix).await?;
        let mut deleted = Vec::new();

        for branch in branches {
            let mission_id = branch.trim_start_matches(&format!("{}/", branch_prefix));
            if !active_mission_ids.contains(&mission_id.to_string())
                && self.git().delete_branch(&branch).await?
            {
                warn!(branch = %branch, "Deleted orphaned branch");
                deleted.push(branch);
            }
        }

        Ok(deleted)
    }

    async fn setup_branch(&self, mission: &mut Mission) -> Result<()> {
        let branch_name = mission.branch_name();
        let repo = Repository::open(&self.repo_path)?;

        // Reuse existing branch if it exists (for retry scenarios)
        if repo.find_branch(&branch_name, BranchType::Local).is_ok() {
            debug!(branch = %branch_name, "Branch already exists, reusing");
            let refname = format!("refs/heads/{}", branch_name);
            let obj = repo.revparse_single(&refname)?;
            repo.checkout_tree(&obj, None)?;
            repo.set_head(&refname)?;
            mission.branch = Some(branch_name);
            return Ok(());
        }

        let head = repo.head()?;
        let commit = head.peel_to_commit()?;
        repo.branch(&branch_name, &commit, false)?;

        let refname = format!("refs/heads/{}", branch_name);
        let obj = repo.revparse_single(&refname)?;
        repo.checkout_tree(&obj, None)?;
        repo.set_head(&refname)?;

        debug!(branch = %branch_name, "Created and checked out branch");
        mission.branch = Some(branch_name);
        Ok(())
    }

    async fn setup_worktree(&self, mission: &mut Mission) -> Result<()> {
        let branch_name = mission.branch_name();
        let worktree_path = self.worktrees_dir.join(&mission.id);

        if worktree_path.exists() {
            debug!(path = %worktree_path.display(), "Worktree already exists");
            mission.branch = Some(branch_name);
            mission.worktree_path = Some(worktree_path);
            return Ok(());
        }

        fs::create_dir_all(&self.worktrees_dir).await?;

        self.git()
            .worktree_add(&worktree_path, &branch_name, &mission.base_branch)
            .await?;

        info!(
            branch = %branch_name,
            path = %worktree_path.display(),
            "Created worktree"
        );

        mission.branch = Some(branch_name);
        mission.worktree_path = Some(worktree_path);
        Ok(())
    }

    async fn cleanup_worktree(&self, mission: &Mission) -> Result<()> {
        let Some(worktree_path) = &mission.worktree_path else {
            return Ok(());
        };

        self.git().worktree_remove(worktree_path).await?;
        info!(path = %worktree_path.display(), "Removed worktree");
        Ok(())
    }

    pub fn working_dir(&self, mission: &Mission) -> PathBuf {
        mission
            .worktree_path
            .clone()
            .unwrap_or_else(|| self.repo_path.clone())
    }

    pub async fn commit(&self, mission: &Mission, message: &str) -> Result<()> {
        let working_dir = self.working_dir(mission);
        let git = GitRunner::new(&working_dir);

        git.add_all().await?;
        let committed = git.commit(message).await?;

        if committed {
            debug!(message = %message, "Created commit");
        }

        Ok(())
    }

    pub async fn get_diff(&self, mission: &Mission) -> Result<String> {
        let working_dir = self.working_dir(mission);
        GitRunner::new(&working_dir)
            .diff_stat(&mission.base_branch)
            .await
    }

    pub async fn merge_to_base(&self, mission: &Mission) -> Result<()> {
        let Some(branch) = &mission.branch else {
            return Ok(());
        };

        let git = self.git();
        git.checkout(&mission.base_branch).await?;
        git.merge(branch, &format!("Merge mission {}", mission.id))
            .await?;

        info!(
            branch = %branch,
            base = %mission.base_branch,
            "Merged branch to base"
        );

        Ok(())
    }

    pub async fn create_pull_request(
        &self,
        mission: &Mission,
        reviewers: &[String],
    ) -> Result<String> {
        let Some(branch) = &mission.branch else {
            return Err(PilotError::Other("No branch to create PR from".into()));
        };

        self.git().push("origin", branch).await?;

        let gh = GhRunner::new(&self.repo_path);
        let title = format!("[{}] {}", mission.id, mission.description);
        let body = format!(
            "## Mission: {}\n\n{}\n\n---\nGenerated by claude-pilot",
            mission.id, mission.description
        );

        let pr_url = gh.create_pr(&title, &body, reviewers).await?;
        info!(url = %pr_url, "Created pull request");

        Ok(pr_url)
    }
}
