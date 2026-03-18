use chrono::Utc;
use tracing::{debug, info, warn};

use super::Orchestrator;
use crate::error::{MissionError, Result};
use crate::learning::LearningExtractor;
use crate::mission::{IsolationMode, Mission, OnComplete, TaskStatus};
use crate::notification::{EventType, MissionEvent};
use crate::state::MissionState;

impl Orchestrator {
    pub(super) async fn checkpoint(&self, mission: &Mission) -> Result<()> {
        if !self.config.orchestrator.auto_commit {
            return Ok(());
        }

        let progress = mission.progress();
        let message = format!(
            "wip({}): checkpoint {}% ({}/{})",
            mission.id, progress.percentage, progress.completed, progress.total
        );

        self.isolation.commit(mission, &message).await?;

        info!(
            mission_id = %mission.id,
            progress = %progress,
            "Checkpoint created"
        );

        Ok(())
    }

    pub(super) async fn complete_mission(&self, mission: &mut Mission) -> Result<()> {
        match &mission.on_complete {
            OnComplete::Direct => {
                if mission.branch.is_some() {
                    self.isolation.merge_to_base(mission).await?;
                }
            }
            OnComplete::PullRequest { reviewers, .. } => {
                let pr_url = self
                    .isolation
                    .create_pull_request(mission, reviewers)
                    .await?;
                info!(mission_id = %mission.id, pr_url = %pr_url, "Pull request created");
            }
            OnComplete::Manual => {
                info!(
                    mission_id = %mission.id,
                    "Mission complete. Run 'claude-pilot merge {}' when ready.",
                    mission.id
                );
            }
        }

        if mission.isolation == IsolationMode::Worktree {
            self.isolation.cleanup(mission, false).await?;
        }

        self.transition_state(
            mission,
            MissionState::Completed,
            "All tasks completed successfully",
        )
        .await?;
        mission.completed_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionCompleted, &mission.id))
            .await;

        let mut signals = self.signals.write().await;
        signals.remove(&mission.id);

        info!(mission_id = %mission.id, "Mission completed");

        // Auto-extract learnings if enabled
        if self.config.learning.auto_extract {
            self.extract_learnings(mission).await;
        }

        // Clean up stale failure histories to prevent memory leak
        self.cleanup_stale_failure_histories().await;

        Ok(())
    }

    async fn extract_learnings(&self, mission: &mut Mission) {
        debug!(mission_id = %mission.id, "Auto-extracting learnings");

        let extractor = LearningExtractor::new(self.paths.clone(), &self.config.agent);
        let working_dir = self.isolation.working_dir(mission);

        match extractor.extract(mission, &working_dir).await {
            Ok(result) => {
                let filtered = self.config.learning.filter(result);
                if !filtered.is_empty() {
                    info!(
                        mission_id = %mission.id,
                        skills = filtered.skills.len(),
                        rules = filtered.rules.len(),
                        agents = filtered.agents.len(),
                        "Extracted learnings (pending approval)"
                    );
                    mission.extracted_candidates = Some(filtered);

                    // Save mission with extracted candidates
                    if let Err(e) = self.store.save(mission).await {
                        warn!(error = %e, "Failed to save extracted candidates");
                    }
                } else {
                    debug!(mission_id = %mission.id, "No learnings extracted (filtered out)");
                }
            }
            Err(e) => {
                warn!(mission_id = %mission.id, error = %e, "Learning extraction failed");
            }
        }
    }

    pub async fn pause(&self, mission_id: &str) -> Result<()> {
        let mission = self.store.load(mission_id).await?;

        if !mission.status.is_active() {
            return Err(MissionError::InvalidState {
                expected: "active".into(),
                actual: mission.status.to_string(),
            }
            .into());
        }

        let has_handler = {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.pause();
                info!(mission_id = %mission_id, "Pause signal sent");
                true
            } else {
                false
            }
        }; // Drop read lock before async operations

        if !has_handler {
            let mut mission = mission;
            self.transition_state(&mut mission, MissionState::Paused, "Pause requested")
                .await?;
            self.notifier
                .notify(&MissionEvent::new(EventType::MissionPaused, mission_id))
                .await;
            info!(mission_id = %mission_id, "Mission paused (not running)");
        }

        Ok(())
    }

    pub async fn resume(&self, mission_id: &str) -> Result<()> {
        let mut mission = self.store.load(mission_id).await?;

        if !mission.status.can_resume() {
            return Err(MissionError::InvalidState {
                expected: "paused or escalated".into(),
                actual: mission.status.to_string(),
            }
            .into());
        }

        // Transition from Escalated/Paused to Running before executing
        // This must happen BEFORE execute() which rejects Escalated state
        if mission.status == MissionState::Escalated || mission.status == MissionState::Paused {
            let previous_state = mission.status;
            self.transition_state(&mut mission, MissionState::Running, "Resumed by user")
                .await?;
            info!(
                mission_id = %mission_id,
                previous_state = %previous_state,
                "Mission resumed, transitioned to Running"
            );
        }

        // Clear escalation context on resume - the human intervention is complete
        if mission.escalation.is_some() {
            info!(
                mission_id = %mission_id,
                escalation_id = mission.escalation.as_ref().map(|e| e.id.as_str()).unwrap_or(""),
                "Clearing escalation context on resume"
            );
            mission.escalation = None;
        }

        // Save state changes before executing
        self.store.save(&mission).await?;

        {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.clear();
            }
        }

        self.execute(mission_id).await
    }

    pub async fn retry(&self, mission_id: &str, reset_all: bool, force: bool) -> Result<()> {
        let mut mission = self.store.load(mission_id).await?;

        let is_orphan = force
            && mission.status == MissionState::Running
            && self
                .lifecycle
                .is_orphan_state(mission_id)
                .await
                .unwrap_or(false);

        let can_retry = mission.status.can_create_retry_mission() || is_orphan;

        if !can_retry {
            if force && mission.status == MissionState::Running {
                return Err(MissionError::AlreadyRunning {
                    mission_id: mission_id.to_string(),
                    pid: self
                        .lifecycle
                        .read_lock(mission_id)
                        .await?
                        .map(|l| l.pid)
                        .unwrap_or(0),
                }
                .into());
            }
            return Err(MissionError::InvalidState {
                expected: "failed or cancelled".into(),
                actual: mission.status.to_string(),
            }
            .into());
        }

        if is_orphan {
            info!(mission_id, "Recovering orphan mission");
            if let Err(e) = self.lifecycle.release_lock(mission_id).await {
                warn!(mission_id = %mission_id, error = %e, "Failed to release stale lock during recovery");
            }
        }

        mission.iteration = 0;

        if reset_all {
            for task in &mut mission.tasks {
                task.status = TaskStatus::Pending;
                task.retry_count = 0;
            }
            self.transition_state(&mut mission, MissionState::Planning, "Retry from start")
                .await?;
            info!(mission_id = %mission_id, "Retrying mission from start");
        } else {
            for task in &mut mission.tasks {
                if task.status == TaskStatus::Failed || task.status == TaskStatus::InProgress {
                    task.status = TaskStatus::Pending;
                    task.retry_count = 0;
                }
            }
            self.transition_state(&mut mission, MissionState::Running, "Retry failed tasks")
                .await?;
            info!(mission_id = %mission_id, "Retrying failed and orphaned tasks");
        }

        // Sync context with updated task states to maintain consistency
        let mut context = self.context_manager.load_or_init(&mission).await?;
        self.context_manager
            .update_from_mission(&mut context, &mission);
        self.context_manager.save(&context).await?;

        {
            let signals = self.signals.read().await;
            if let Some(handler) = signals.get(mission_id) {
                handler.clear();
            }
        }

        self.execute(mission_id).await
    }

    pub async fn cancel(&self, mission_id: &str) -> Result<()> {
        let mission = self.store.load(mission_id).await?;

        if !mission.status.can_cancel() {
            return Err(MissionError::InvalidState {
                expected: "non-terminal".into(),
                actual: mission.status.to_string(),
            }
            .into());
        }

        let signals = self.signals.read().await;
        if let Some(handler) = signals.get(mission_id) {
            handler.cancel();
            info!(mission_id = %mission_id, "Cancel signal sent");
        } else {
            drop(signals);
            let mut mission = mission;
            if mission.isolation == IsolationMode::Worktree {
                self.isolation.cleanup(&mission, true).await?;
            }
            self.transition_state(&mut mission, MissionState::Cancelled, "Cancel requested")
                .await?;
            self.notifier
                .notify(&MissionEvent::new(EventType::MissionCancelled, mission_id))
                .await;
            info!(mission_id = %mission_id, "Mission cancelled (not running)");
        }

        Ok(())
    }

    pub async fn status(&self, mission_id: &str) -> Result<Mission> {
        self.store.load(mission_id).await
    }

    /// Check if a mission in "running" state is actually orphaned (process died).
    pub async fn is_orphan_state(&self, mission_id: &str) -> Result<bool> {
        let mission = self.store.load(mission_id).await?;

        // Only check for running missions
        if mission.status != MissionState::Running {
            return Ok(false);
        }

        self.lifecycle.is_orphan_state(mission_id).await
    }

    /// Mark an orphan running mission as failed.
    /// Returns true if the mission was actually orphaned and marked as failed.
    pub async fn recover_orphan_state(&self, mission_id: &str) -> Result<bool> {
        if !self.is_orphan_state(mission_id).await? {
            return Ok(false);
        }

        let mut mission = self.store.load(mission_id).await?;

        warn!(
            mission_id = %mission_id,
            "Detected orphan running state, marking as failed"
        );

        // Mark any in_progress tasks as failed too
        for task in &mut mission.tasks {
            if task.status == TaskStatus::InProgress {
                task.status = TaskStatus::Failed;
            }
        }

        self.transition_state(&mut mission, MissionState::Failed, "Orphan state detected")
            .await?;

        // Clean up any stale lock file
        if let Err(e) = self.lifecycle.release_lock(mission_id).await {
            warn!(mission_id = %mission_id, error = %e, "Failed to release lock after orphan cleanup");
        }

        Ok(true)
    }

    pub async fn list_missions(&self) -> Result<Vec<Mission>> {
        self.store.list().await
    }
}
