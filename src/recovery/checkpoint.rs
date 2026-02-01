use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{info, warn};

use crate::error::Result;
use crate::mission::Mission;

/// Generates snapshot methods for checkpoint fields.
/// Creates: with_{name}_snapshot(), try_restore_{name}(), restore_{name}()
macro_rules! impl_snapshot_methods {
    ($($name:ident : $field:ident => $type:ty , $label:expr);+ $(;)?) => {
        $(
            paste::paste! {
                #[doc = concat!("Add ", $label, " snapshot for durable recovery.")]
                pub fn [<with_ $name _snapshot>](mut self, item: &$type) -> Self {
                    match serde_yaml_bw::to_string(item) {
                        Ok(yaml) => self.$field = Some(yaml),
                        Err(e) => tracing::warn!(error = %e, concat!("Failed to serialize ", $label, " for checkpoint")),
                    }
                    self
                }

                #[doc = concat!("Restore ", $label, " from checkpoint (strict mode).")]
                pub fn [<try_restore_ $name>](&self) -> Result<$type> {
                    use crate::error::PilotError;
                    let yaml = self.$field.as_ref()
                        .ok_or_else(|| PilotError::Recovery(
                            concat!("Checkpoint has no ", $label, " snapshot").into()
                        ))?;
                    serde_yaml_bw::from_str(yaml)
                        .map_err(|e| PilotError::Recovery(
                            format!(concat!("Failed to deserialize ", $label, ": {}"), e)
                        ))
                }

                #[doc = concat!("Restore ", $label, " from checkpoint (lenient mode).")]
                pub fn [<restore_ $name>](&self) -> Option<$type> {
                    match self.[<try_restore_ $name>]() {
                        Ok(item) => Some(item),
                        Err(e) => {
                            tracing::warn!(error = %e, concat!("Failed to restore ", $label, " from checkpoint"));
                            None
                        }
                    }
                }
            }
        )+
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub mission_id: String,
    pub created_at: DateTime<Utc>,
    pub task_states: Vec<TaskCheckpoint>,
    pub phase_index: usize,
    pub iteration: u32,
    pub git_commit: Option<String>,
    pub context_snapshot: Option<String>,
    pub evidence_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_snapshot: Option<String>,
    /// Serialized failure histories for retry pattern learning across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_history_snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCheckpoint {
    pub task_id: String,
    pub status: crate::mission::TaskStatus,
    pub retry_count: u32,
    /// Scope adjustment factor for adaptive token budget (default 1.0).
    /// Preserved across checkpoints to maintain recovery effectiveness.
    #[serde(default = "default_scope_factor")]
    pub scope_factor: f32,
    pub result_summary: Option<String>,
}

fn default_scope_factor() -> f32 {
    1.0
}

impl Checkpoint {
    pub fn from_mission(mission: &Mission, checkpoint_id: impl Into<String>) -> Self {
        let task_states = mission
            .tasks
            .iter()
            .map(|t| TaskCheckpoint {
                task_id: t.id.clone(),
                status: t.status,
                retry_count: t.retry_count,
                scope_factor: t.scope_factor,
                result_summary: t.result.as_ref().map(|r| {
                    if r.success {
                        format!("Success: {} files modified", r.files_modified.len())
                    } else {
                        "Failed".to_string()
                    }
                }),
            })
            .collect();

        let phase_index = mission
            .phases
            .iter()
            .position(|p| {
                p.task_ids.iter().any(|tid| {
                    mission
                        .tasks
                        .iter()
                        .find(|t| &t.id == tid)
                        .map(|t| t.status.is_active())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);

        Self {
            id: checkpoint_id.into(),
            mission_id: mission.id.clone(),
            created_at: Utc::now(),
            task_states,
            phase_index,
            iteration: mission.iteration,
            git_commit: None,
            context_snapshot: None,
            evidence_snapshot: None,
            reasoning_snapshot: None,
            verification_snapshot: None,
            failure_history_snapshot: None,
        }
    }

    impl_snapshot_methods! {
        context: context_snapshot => crate::context::MissionContext, "context";
        evidence: evidence_snapshot => crate::planning::Evidence, "evidence";
        reasoning: reasoning_snapshot => super::reasoning::ReasoningContext, "reasoning";
        verification: verification_snapshot => crate::verification::VerificationArchive, "verification";
        failure_history: failure_history_snapshot => std::collections::HashMap<String, super::retry_analyzer::FailureHistory>, "failure_history";
    }
}

pub struct CheckpointManager {
    checkpoints_dir: PathBuf,
}

impl CheckpointManager {
    pub fn new(missions_dir: impl AsRef<Path>) -> Self {
        Self {
            checkpoints_dir: missions_dir.as_ref().to_path_buf(),
        }
    }

    fn checkpoint_dir(&self, mission_id: &str) -> PathBuf {
        self.checkpoints_dir
            .join(mission_id)
            .join("recovery")
            .join("checkpoints")
    }

    fn checkpoint_file(&self, mission_id: &str, checkpoint_id: &str) -> PathBuf {
        self.checkpoint_dir(mission_id)
            .join(format!("{}.yaml", checkpoint_id))
    }

    pub async fn save(&self, checkpoint: &Checkpoint) -> Result<()> {
        let dir = self.checkpoint_dir(&checkpoint.mission_id);
        fs::create_dir_all(&dir).await?;

        let file = self.checkpoint_file(&checkpoint.mission_id, &checkpoint.id);
        let temp_file = file.with_extension("yaml.tmp");

        // Atomic write: serialize → temp file → rename
        let yaml = serde_yaml_bw::to_string(checkpoint)?;
        fs::write(&temp_file, &yaml).await?;

        // Platform-specific atomic replace:
        // - Unix: rename() replaces atomically
        // - Windows: rename() fails if target exists, use backup strategy
        //
        // Windows strategy preserves data integrity:
        // 1. If target exists, rename to .bak (old checkpoint preserved)
        // 2. Rename temp to target
        // 3. Remove .bak on success
        // If crash occurs between steps, .bak contains last good state.
        #[cfg(windows)]
        {
            let backup_file = file.with_extension("yaml.bak");
            if file.exists() {
                // Backup existing checkpoint before replacement
                if let Err(e) = fs::rename(&file, &backup_file).await {
                    // If backup fails, try direct removal as fallback
                    tracing::warn!(error = %e, "Backup rename failed, attempting direct removal");
                    let _ = fs::remove_file(&file).await;
                }
            }

            match fs::rename(&temp_file, &file).await {
                Ok(_) => {
                    // Success: remove backup
                    let _ = fs::remove_file(&backup_file).await;
                }
                Err(e) => {
                    // Failure: restore from backup if available
                    if backup_file.exists() {
                        let _ = fs::rename(&backup_file, &file).await;
                    }
                    let _ = std::fs::remove_file(&temp_file);
                    return Err(e.into());
                }
            }
        }

        #[cfg(not(windows))]
        fs::rename(&temp_file, &file).await.inspect_err(|_| {
            let _ = std::fs::remove_file(&temp_file);
        })?;

        info!(
            checkpoint_id = checkpoint.id,
            mission_id = checkpoint.mission_id,
            "Checkpoint saved"
        );

        Ok(())
    }

    pub async fn load(&self, mission_id: &str, checkpoint_id: &str) -> Result<Checkpoint> {
        let file = self.checkpoint_file(mission_id, checkpoint_id);
        let content = fs::read_to_string(&file).await?;
        let checkpoint: Checkpoint = serde_yaml_bw::from_str(&content)?;
        Ok(checkpoint)
    }

    pub async fn list(&self, mission_id: &str) -> Result<Vec<Checkpoint>> {
        let dir = self.checkpoint_dir(mission_id);

        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut checkpoints = Vec::new();
        let mut entries = fs::read_dir(&dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().map(|e| e == "yaml").unwrap_or(false)
                && let Ok(content) = fs::read_to_string(&path).await
                && let Ok(checkpoint) = serde_yaml_bw::from_str(&content)
            {
                checkpoints.push(checkpoint);
            }
        }

        checkpoints.sort_by(|a: &Checkpoint, b: &Checkpoint| b.created_at.cmp(&a.created_at));
        Ok(checkpoints)
    }

    /// Get the most recent checkpoint without loading all checkpoints.
    /// Uses filename sorting (timestamp-prefixed) to find the latest.
    pub async fn latest(&self, mission_id: &str) -> Result<Option<Checkpoint>> {
        let dir = self.checkpoint_dir(mission_id);

        if !dir.exists() {
            return Ok(None);
        }

        let mut entries = fs::read_dir(&dir).await?;
        let mut filenames: Vec<String> = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                filenames.push(name.to_string());
            }
        }

        if filenames.is_empty() {
            return Ok(None);
        }

        // Sort descending - timestamp-prefixed filenames sort naturally
        filenames.sort_by(|a, b| b.cmp(a));

        // Load only the most recent checkpoint
        let latest_id = &filenames[0];
        match self.load(mission_id, latest_id).await {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(e) => {
                warn!(checkpoint_id = latest_id, error = %e, "Failed to load latest checkpoint");
                Ok(None)
            }
        }
    }

    /// Create checkpoint with full state for complete durable recovery.
    pub async fn create_with_state(
        &self,
        mission: &Mission,
        context: &crate::context::MissionContext,
        evidence: Option<&crate::planning::Evidence>,
        failure_histories: Option<
            &std::collections::HashMap<String, super::retry_analyzer::FailureHistory>,
        >,
    ) -> Result<Checkpoint> {
        let completed_count = mission
            .tasks
            .iter()
            .filter(|t| t.status == crate::mission::TaskStatus::Completed)
            .count();

        let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let id = format!("{}_checkpoint-{:03}", timestamp, completed_count);

        let mut checkpoint = Checkpoint::from_mission(mission, id).with_context_snapshot(context);

        if let Some(ev) = evidence {
            checkpoint = checkpoint.with_evidence_snapshot(ev);
        }

        if let Some(ref reasoning) = context.reasoning_context {
            checkpoint = checkpoint.with_reasoning_snapshot(reasoning);
        }

        if let Some(ref archive) = context.verification_archive {
            checkpoint = checkpoint.with_verification_snapshot(archive);
        }

        if let Some(histories) = failure_histories {
            checkpoint = checkpoint.with_failure_history_snapshot(histories);
        }

        self.save(&checkpoint).await?;
        info!(
            checkpoint_id = checkpoint.id,
            has_context = checkpoint.context_snapshot.is_some(),
            has_evidence = checkpoint.evidence_snapshot.is_some(),
            "Checkpoint created with full state"
        );
        Ok(checkpoint)
    }

    /// Delete old checkpoints, keeping only the most recent `keep_count`.
    /// Uses filename sorting to avoid loading checkpoint content.
    pub async fn cleanup_old(&self, mission_id: &str, keep_count: usize) -> Result<usize> {
        let dir = self.checkpoint_dir(mission_id);

        if !dir.exists() {
            return Ok(0);
        }

        let mut entries = fs::read_dir(&dir).await?;
        let mut filenames: Vec<String> = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml")
                && let Some(name) = path.file_stem().and_then(|s| s.to_str())
            {
                filenames.push(name.to_string());
            }
        }

        if filenames.len() <= keep_count {
            return Ok(0);
        }

        // Sort descending (newest first)
        filenames.sort_by(|a, b| b.cmp(a));

        // Delete everything after keep_count
        let to_delete = &filenames[keep_count..];
        let mut deleted = 0;

        for checkpoint_id in to_delete {
            let file = self.checkpoint_file(mission_id, checkpoint_id);
            if fs::remove_file(&file).await.is_ok() {
                deleted += 1;
            }
        }

        info!(mission_id, deleted, "Cleaned up old checkpoints");
        Ok(deleted)
    }
}
