use std::path::Path;

use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

use super::analyzer::FailureAnalyzer;
use super::checkpoint::CheckpointManager;
use super::escalation::EscalationHandler;
use super::types::{CompactionLevel, FailureAnalysis, RecoveryStatus, RecoveryStrategy};
use crate::config::{CheckpointConfig, CompactionConfig, CompactionLevelConfig};
use crate::context::{ContextCompactor, MissionContext};
use crate::error::{PilotError, Result};
use crate::mission::Mission;
use crate::notification::Notifier;

pub struct RecoveryExecutor {
    checkpoint_manager: CheckpointManager,
    escalation_handler: EscalationHandler,
    compaction_level_config: CompactionLevelConfig,
    checkpoint_config: CheckpointConfig,
}

impl RecoveryExecutor {
    pub fn new(
        missions_dir: impl AsRef<Path>,
        notifier: Option<Notifier>,
        compaction_level_config: CompactionLevelConfig,
        checkpoint_config: CheckpointConfig,
    ) -> Self {
        Self {
            checkpoint_manager: CheckpointManager::new(missions_dir),
            escalation_handler: EscalationHandler::new(notifier),
            compaction_level_config,
            checkpoint_config,
        }
    }

    pub async fn execute_recovery(
        &self,
        strategy: &RecoveryStrategy,
        mission: &mut Mission,
        context: &mut MissionContext,
    ) -> Result<RecoveryStatus> {
        info!(?strategy, "Executing recovery strategy");

        match strategy {
            RecoveryStrategy::CompactAndRetry {
                compaction_level,
                max_retries,
            } => {
                self.execute_compact_and_retry(context, *compaction_level, *max_retries)
                    .await
            }

            RecoveryStrategy::ChunkAndRetry {
                chunk_size_reduction,
                timeout_increase,
            } => {
                self.execute_chunk_and_retry(*chunk_size_reduction, *timeout_increase)
                    .await
            }

            RecoveryStrategy::ConvergentFix { max_fix_rounds } => {
                self.execute_convergent_fix(*max_fix_rounds).await
            }

            RecoveryStrategy::RollbackToCheckpoint {
                checkpoint_id,
                reset_retry_counts,
            } => {
                self.execute_rollback(
                    mission,
                    context,
                    checkpoint_id.as_deref(),
                    *reset_retry_counts,
                )
                .await
            }

            RecoveryStrategy::SimpleRetry {
                delay_secs,
                max_retries,
            } => self.execute_simple_retry(*delay_secs, *max_retries).await,

            RecoveryStrategy::Escalate {
                reason,
                suggested_actions,
            } => {
                self.execute_escalation(mission, reason, suggested_actions.clone())
                    .await
            }

            RecoveryStrategy::Skip { reason } => Ok(RecoveryStatus::skipped(reason)),
        }
    }

    async fn execute_compact_and_retry(
        &self,
        context: &mut MissionContext,
        level: CompactionLevel,
        max_retries: u32,
    ) -> Result<RecoveryStatus> {
        let settings = self.build_compaction_settings(level);
        let mut compactor = ContextCompactor::new(settings);

        if level == CompactionLevel::Emergency {
            compactor.emergency_compact(context).await?;
        } else {
            compactor.compact(context).await?;
        }

        info!(
            level = ?level,
            remaining_retries = max_retries,
            "Compaction complete, ready for retry"
        );

        Ok(RecoveryStatus::recovered(format!(
            "Context compacted ({:?}), {} retries remaining",
            level, max_retries
        )))
    }

    fn build_compaction_settings(&self, level: CompactionLevel) -> CompactionConfig {
        let cfg = &self.compaction_level_config;
        let base = CompactionConfig::default();

        match level {
            CompactionLevel::Light => CompactionConfig {
                compaction_threshold: cfg.light_threshold,
                aggressive_ratio: cfg.light_aggressive,
                ..base
            },
            CompactionLevel::Moderate => CompactionConfig {
                compaction_threshold: cfg.moderate_threshold,
                aggressive_ratio: cfg.moderate_aggressive,
                ..base
            },
            CompactionLevel::Aggressive => CompactionConfig {
                compaction_threshold: cfg.aggressive_threshold,
                aggressive_ratio: cfg.aggressive_ratio,
                preserve_recent_tasks: cfg.aggressive_preserve_tasks,
                preserve_recent_learnings: cfg.aggressive_preserve_learnings,
                ..base
            },
            CompactionLevel::Emergency => CompactionConfig {
                compaction_threshold: cfg.emergency_threshold,
                aggressive_ratio: cfg.emergency_aggressive,
                preserve_recent_tasks: cfg.emergency_preserve_tasks,
                preserve_recent_learnings: cfg.emergency_preserve_learnings,
                min_phase_age_for_compression: cfg.emergency_min_phase_age,
                ..base
            },
        }
    }

    async fn execute_chunk_and_retry(
        &self,
        chunk_reduction: f32,
        timeout_increase: f32,
    ) -> Result<RecoveryStatus> {
        info!(
            chunk_reduction,
            timeout_increase, "Adjusting planning parameters for retry"
        );

        Ok(RecoveryStatus::recovered_with_action(
            format!(
                "Planning parameters adjusted: chunks {}x, timeout {}x",
                chunk_reduction, timeout_increase
            ),
            super::types::RecoveryAction::RetryWithReducedChunks {
                reduction_factor: chunk_reduction,
            },
        ))
    }

    async fn execute_convergent_fix(&self, max_rounds: u32) -> Result<RecoveryStatus> {
        info!(max_rounds, "Starting convergent fix");

        Ok(RecoveryStatus::recovered_with_action(
            format!("Convergent fix initiated with {} max rounds", max_rounds),
            super::types::RecoveryAction::StartConvergentVerification { max_rounds },
        ))
    }

    async fn execute_rollback(
        &self,
        mission: &mut Mission,
        context: &mut MissionContext,
        checkpoint_id: Option<&str>,
        reset_retry_counts: bool,
    ) -> Result<RecoveryStatus> {
        let checkpoint = if let Some(id) = checkpoint_id {
            self.checkpoint_manager.load(&mission.id, id).await?
        } else {
            self.checkpoint_manager
                .latest(&mission.id)
                .await?
                .ok_or_else(|| crate::error::PilotError::Config("No checkpoint found".into()))?
        };

        // Validate checkpoint belongs to this mission (critical for state consistency)
        if checkpoint.mission_id != mission.id {
            return Err(crate::error::PilotError::Recovery(format!(
                "Checkpoint mission_id mismatch: expected '{}', found '{}'",
                mission.id, checkpoint.mission_id
            )));
        }

        // Restore task states including scope_factor for adaptive budget
        for task_state in &checkpoint.task_states {
            if let Some(task) = mission.task_mut(&task_state.task_id) {
                task.status = task_state.status;
                task.scope_factor = task_state.scope_factor;
                if reset_retry_counts {
                    task.retry_count = 0;
                }
            }
        }

        mission.iteration = checkpoint.iteration;

        // Restore context from snapshot if available
        let context_restored = if let Some(restored) = checkpoint.restore_context() {
            *context = restored;
            context.update_timestamp();
            true
        } else {
            false
        };

        // Restore evidence from snapshot for durable execution (avoid re-gathering)
        // Missing snapshot is OK (old checkpoint), but corrupted snapshot is an error
        let evidence_restored = match checkpoint.try_restore_evidence() {
            Ok(evidence) => {
                context.set_evidence(evidence);
                true
            }
            Err(e) => {
                if checkpoint.evidence_snapshot.is_some() {
                    return Err(PilotError::Recovery(format!(
                        "Evidence snapshot corrupted in checkpoint {}: {}",
                        checkpoint.id, e
                    )));
                }
                debug!(
                    checkpoint_id = checkpoint.id,
                    "No evidence snapshot (will re-gather)"
                );
                false
            }
        };

        // Restore reasoning context for hypothesis/decision tracking
        let reasoning_restored = match checkpoint.try_restore_reasoning() {
            Ok(reasoning) => {
                context.set_reasoning_context(reasoning);
                true
            }
            Err(e) => {
                if checkpoint.reasoning_snapshot.is_some() {
                    return Err(PilotError::Recovery(format!(
                        "Reasoning snapshot corrupted in checkpoint {}: {}",
                        checkpoint.id, e
                    )));
                }
                debug!(checkpoint_id = checkpoint.id, "No reasoning snapshot");
                false
            }
        };

        // Restore verification archive for learned patterns
        let archive_restored = match checkpoint.try_restore_verification() {
            Ok(archive) => {
                context.set_verification_archive(archive);
                true
            }
            Err(e) => {
                if checkpoint.verification_snapshot.is_some() {
                    return Err(PilotError::Recovery(format!(
                        "Verification snapshot corrupted in checkpoint {}: {}",
                        checkpoint.id, e
                    )));
                }
                debug!(checkpoint_id = checkpoint.id, "No verification snapshot");
                false
            }
        };

        info!(
            checkpoint_id = checkpoint.id,
            reset_retry_counts,
            context_restored,
            evidence_restored,
            reasoning_restored,
            archive_restored,
            "Rolled back to checkpoint"
        );

        // Build descriptive restore info
        let mut restored_parts = Vec::new();
        if context_restored {
            restored_parts.push("context");
        }
        if evidence_restored {
            restored_parts.push("evidence");
        }
        if reasoning_restored {
            restored_parts.push("reasoning");
        }
        if archive_restored {
            restored_parts.push("archive");
        }

        let restore_info = if restored_parts.is_empty() {
            String::new()
        } else {
            format!(" ({} restored)", restored_parts.join(" + "))
        };

        Ok(RecoveryStatus::recovered(format!(
            "Rolled back to checkpoint {}{}",
            checkpoint.id, restore_info
        )))
    }

    async fn execute_simple_retry(
        &self,
        delay_secs: u64,
        max_retries: u32,
    ) -> Result<RecoveryStatus> {
        if delay_secs > 0 {
            info!(delay_secs, "Waiting before retry");
            sleep(Duration::from_secs(delay_secs)).await;
        }

        Ok(RecoveryStatus::recovered(format!(
            "Ready for retry ({} remaining)",
            max_retries
        )))
    }

    async fn execute_escalation(
        &self,
        mission: &Mission,
        reason: &str,
        suggested_actions: Vec<String>,
    ) -> Result<RecoveryStatus> {
        let analysis = FailureAnalyzer::analyze(
            &crate::error::PilotError::Config(reason.to_string()),
            mission,
            super::types::FailurePhase::Execution,
        );

        let escalation = self
            .escalation_handler
            .escalate(mission, reason, analysis, suggested_actions)
            .await?;

        self.escalation_handler.notify_user(&escalation).await?;

        Ok(RecoveryStatus::escalated(reason))
    }

    pub async fn create_checkpoint(
        &self,
        mission: &Mission,
        context: &MissionContext,
        evidence: Option<&crate::planning::Evidence>,
        failure_histories: Option<
            &std::collections::HashMap<String, super::retry_analyzer::FailureHistory>,
        >,
    ) -> Result<()> {
        self.checkpoint_manager
            .create_with_state(mission, context, evidence, failure_histories)
            .await?;

        if let Err(e) = self
            .checkpoint_manager
            .cleanup_old(&mission.id, self.checkpoint_config.max_checkpoints)
            .await
        {
            tracing::warn!(error = %e, "Failed to cleanup old checkpoints");
        }

        Ok(())
    }

    pub async fn should_auto_recover(&self, analysis: &FailureAnalysis) -> bool {
        analysis.is_recoverable && !FailureAnalyzer::should_escalate(analysis)
    }

    /// Attempt to restore mission state from the latest checkpoint on startup.
    /// Returns (checkpoint_id, failure_histories) if restoration occurred.
    pub async fn try_restore_on_startup(
        &self,
        mission: &mut Mission,
        context: &mut MissionContext,
    ) -> Result<
        Option<(
            String,
            Option<std::collections::HashMap<String, super::retry_analyzer::FailureHistory>>,
        )>,
    > {
        let checkpoint = match self.checkpoint_manager.latest(&mission.id).await? {
            Some(cp) => cp,
            None => return Ok(None),
        };

        if checkpoint.mission_id != mission.id {
            warn!(
                expected = %mission.id,
                found = %checkpoint.mission_id,
                "Checkpoint mission_id mismatch, skipping restore"
            );
            return Ok(None);
        }

        // Restore task states (preserve retry_count and scope_factor for recovery effectiveness)
        for task_state in &checkpoint.task_states {
            if let Some(task) = mission.task_mut(&task_state.task_id) {
                task.status = task_state.status;
                task.retry_count = task_state.retry_count;
                task.scope_factor = task_state.scope_factor;
            }
        }

        mission.iteration = checkpoint.iteration;

        // Restore evidence (critical for avoiding expensive re-gathering)
        if let Ok(evidence) = checkpoint.try_restore_evidence() {
            context.set_evidence(evidence);
        }

        // Restore reasoning context (hypothesis/decision tracking)
        if let Ok(reasoning) = checkpoint.try_restore_reasoning() {
            context.set_reasoning_context(reasoning);
        }

        // Restore verification archive (learned fix patterns)
        if let Ok(archive) = checkpoint.try_restore_verification() {
            context.set_verification_archive(archive);
        }

        // Restore failure histories (retry pattern learning)
        let failure_histories = checkpoint.restore_failure_history();

        info!(
            checkpoint_id = %checkpoint.id,
            completed_tasks = checkpoint.task_states.iter()
                .filter(|t| t.status == crate::mission::TaskStatus::Completed)
                .count(),
            iteration = checkpoint.iteration,
            has_evidence = checkpoint.evidence_snapshot.is_some(),
            has_failure_history = failure_histories.is_some(),
            "Restored from checkpoint on startup"
        );

        Ok(Some((checkpoint.id, failure_histories)))
    }
}
