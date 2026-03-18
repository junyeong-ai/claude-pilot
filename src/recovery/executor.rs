use std::path::Path;

use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

use super::analyzer::FailureAnalyzer;
use super::checkpoint::MissionCheckpointManager;
use super::escalation::EscalationHandler;
use super::types::{CompactionLevel, FailureAnalysis, RecoveryStatus, RecoveryStrategy};
use crate::config::{CheckpointConfig, CompactionConfig, CompactionLevelConfig};
use crate::context::{ContextCompactor, MissionContext};
use crate::error::{PilotError, Result};
use crate::mission::Mission;
use crate::notification::Notifier;

pub struct RecoveryExecutor {
    checkpoint_manager: MissionCheckpointManager,
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
            checkpoint_manager: MissionCheckpointManager::new(missions_dir),
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
        let strategy = match level {
            CompactionLevel::Light => &cfg.light,
            CompactionLevel::Moderate => &cfg.moderate,
            CompactionLevel::Aggressive => &cfg.aggressive,
            CompactionLevel::Emergency => &cfg.emergency,
        };

        let mut result = CompactionConfig {
            compaction_threshold: strategy.threshold,
            aggressive_ratio: strategy.aggressive,
            preserve_recent_tasks: strategy.preserve_tasks,
            preserve_recent_learnings: strategy.preserve_learnings,
            ..base
        };

        if matches!(level, CompactionLevel::Emergency) {
            result.min_phase_age_for_compression = cfg.emergency_min_phase_age;
        }

        result
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::types::{
        CompactionLevel, FailureAnalysis, FailureCategory, FailurePhase, RecoveryAction,
    };

    fn test_executor() -> RecoveryExecutor {
        let temp = std::env::temp_dir().join("claude-pilot-test-executor");
        RecoveryExecutor::new(
            temp,
            None,
            CompactionLevelConfig::default(),
            CheckpointConfig::default(),
        )
    }

    // ── build_compaction_settings ──

    #[test]
    fn test_build_compaction_settings_light() {
        let executor = test_executor();
        let cfg = &executor.compaction_level_config;
        let settings = executor.build_compaction_settings(CompactionLevel::Light);
        assert_eq!(settings.compaction_threshold, cfg.light.threshold);
        assert_eq!(settings.aggressive_ratio, cfg.light.aggressive);
        assert_eq!(settings.preserve_recent_tasks, cfg.light.preserve_tasks);
    }

    #[test]
    fn test_build_compaction_settings_moderate() {
        let executor = test_executor();
        let cfg = &executor.compaction_level_config;
        let settings = executor.build_compaction_settings(CompactionLevel::Moderate);
        assert_eq!(settings.compaction_threshold, cfg.moderate.threshold);
        assert_eq!(settings.aggressive_ratio, cfg.moderate.aggressive);
    }

    #[test]
    fn test_build_compaction_settings_aggressive() {
        let executor = test_executor();
        let cfg = &executor.compaction_level_config;
        let settings = executor.build_compaction_settings(CompactionLevel::Aggressive);
        assert_eq!(settings.compaction_threshold, cfg.aggressive.threshold);
        assert_eq!(settings.aggressive_ratio, cfg.aggressive.aggressive);
        assert_eq!(settings.preserve_recent_tasks, cfg.aggressive.preserve_tasks);
        assert_eq!(
            settings.preserve_recent_learnings,
            cfg.aggressive.preserve_learnings
        );
    }

    #[test]
    fn test_build_compaction_settings_emergency() {
        let executor = test_executor();
        let cfg = &executor.compaction_level_config;
        let settings = executor.build_compaction_settings(CompactionLevel::Emergency);
        assert_eq!(settings.compaction_threshold, cfg.emergency.threshold);
        assert_eq!(settings.aggressive_ratio, cfg.emergency.aggressive);
        assert_eq!(settings.preserve_recent_tasks, cfg.emergency.preserve_tasks);
        assert_eq!(
            settings.preserve_recent_learnings,
            cfg.emergency.preserve_learnings
        );
        assert_eq!(
            settings.min_phase_age_for_compression,
            cfg.emergency_min_phase_age
        );
    }

    #[test]
    fn test_build_compaction_settings_progressive_thresholds() {
        let executor = test_executor();
        let light = executor.build_compaction_settings(CompactionLevel::Light);
        let moderate = executor.build_compaction_settings(CompactionLevel::Moderate);
        let aggressive = executor.build_compaction_settings(CompactionLevel::Aggressive);
        let emergency = executor.build_compaction_settings(CompactionLevel::Emergency);

        // More aggressive levels should have lower thresholds (compact more)
        assert!(light.compaction_threshold > moderate.compaction_threshold);
        assert!(moderate.compaction_threshold > aggressive.compaction_threshold);
        assert!(aggressive.compaction_threshold > emergency.compaction_threshold);

        // More aggressive levels should have lower aggressive_ratio (discard more)
        assert!(light.aggressive_ratio > moderate.aggressive_ratio);
        assert!(moderate.aggressive_ratio > aggressive.aggressive_ratio);
        assert!(aggressive.aggressive_ratio > emergency.aggressive_ratio);
    }

    // ── should_auto_recover ──

    #[tokio::test]
    async fn test_should_auto_recover_recoverable() {
        let executor = test_executor();
        let analysis = FailureAnalysis::new(
            FailureCategory::BuildFailure,
            FailurePhase::Execution,
            "build failed",
        );
        assert!(executor.should_auto_recover(&analysis).await);
    }

    #[tokio::test]
    async fn test_should_auto_recover_unrecoverable() {
        let executor = test_executor();
        let analysis = FailureAnalysis::new(
            FailureCategory::VerificationLoop,
            FailurePhase::Verification,
            "stuck in loop",
        );
        // VerificationLoop is fundamentally unrecoverable
        assert!(!executor.should_auto_recover(&analysis).await);
    }

    #[tokio::test]
    async fn test_should_auto_recover_exhausted_retries() {
        let executor = test_executor();
        let analysis = FailureAnalysis::new(
            FailureCategory::BuildFailure,
            FailurePhase::Execution,
            "build failed",
        )
        .with_retry_count(100); // way over max retries
        // Should escalate because retry count exceeds max
        assert!(!executor.should_auto_recover(&analysis).await);
    }

    // ── execute_simple_retry ──

    #[tokio::test]
    async fn test_execute_simple_retry_no_delay() {
        let executor = test_executor();
        let result = executor.execute_simple_retry(0, 3).await.unwrap();
        assert!(result.is_recovered());
    }

    // ── execute_chunk_and_retry ──

    #[tokio::test]
    async fn test_execute_chunk_and_retry() -> std::result::Result<(), String> {
        let executor = test_executor();
        let result = executor.execute_chunk_and_retry(0.5, 1.5).await.unwrap();
        assert!(result.is_recovered());
        if let RecoveryStatus::Recovered { action, .. } = &result {
            assert!(matches!(
                action,
                RecoveryAction::RetryWithReducedChunks { .. }
            ));
            Ok(())
        } else {
            Err(format!("Expected Recovered status, got {:?}", result))
        }
    }

    // ── execute_convergent_fix ──

    #[tokio::test]
    async fn test_execute_convergent_fix() -> std::result::Result<(), String> {
        let executor = test_executor();
        let result = executor.execute_convergent_fix(5).await.unwrap();
        assert!(result.is_recovered());
        if let RecoveryStatus::Recovered { action, .. } = &result {
            assert!(matches!(
                action,
                RecoveryAction::StartConvergentVerification { max_rounds: 5 }
            ));
            Ok(())
        } else {
            Err(format!("Expected Recovered status, got {:?}", result))
        }
    }
}
