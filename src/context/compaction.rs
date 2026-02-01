use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::types::{MissionContext, PhaseStatus, VerificationStatus};
use crate::agent::multi::core::TaskContext;
use crate::config::CompactionConfig;
use crate::error::Result;
use crate::summarization::TextCompressor;
use crate::utils::estimate_tokens;

#[derive(Debug, Clone)]
pub enum CompactionTrigger {
    ThresholdExceeded {
        current: usize,
        threshold: usize,
    },
    TimeoutDetected {
        operation: String,
        duration_secs: u64,
    },
    PhaseCompleted {
        phase_id: String,
    },
    ManualRequest,
    OutOfMemory {
        error: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionResult {
    pub original_tokens: usize,
    pub final_tokens: usize,
    pub compression_ratio: f32,
    pub phases_compressed: usize,
    pub tasks_compressed: usize,
    pub learnings_deduplicated: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionArchive {
    snapshots: VecDeque<ArchivedSnapshot>,
    #[serde(default = "default_max_snapshots")]
    max_snapshots: usize,
}

fn default_max_snapshots() -> usize {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivedSnapshot {
    pub created_at: DateTime<Utc>,
    pub trigger: String,
    pub original_tokens: usize,
    pub context_yaml: String,
}

impl Default for CompactionArchive {
    fn default() -> Self {
        Self::new()
    }
}

impl CompactionArchive {
    pub fn new() -> Self {
        Self {
            snapshots: VecDeque::new(),
            max_snapshots: default_max_snapshots(),
        }
    }

    pub fn with_max_snapshots(max_snapshots: usize) -> Self {
        Self {
            snapshots: VecDeque::new(),
            max_snapshots,
        }
    }

    pub fn archive(&mut self, context: &MissionContext, trigger: &str) {
        if let Ok(yaml) = serde_yaml_bw::to_string(context) {
            let snapshot = ArchivedSnapshot {
                created_at: Utc::now(),
                trigger: trigger.to_string(),
                original_tokens: context.token_budget.current_usage.total,
                context_yaml: yaml,
            };
            self.snapshots.push_back(snapshot);

            while self.snapshots.len() > self.max_snapshots {
                self.snapshots.pop_front();
            }
        }
    }

    pub fn restore_latest(&self) -> Option<MissionContext> {
        self.snapshots
            .back()
            .and_then(|s| serde_yaml_bw::from_str(&s.context_yaml).ok())
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }
}

pub struct ContextCompactor {
    config: CompactionConfig,
    compressor: TextCompressor,
    archive: CompactionArchive,
}

impl ContextCompactor {
    pub fn new(config: CompactionConfig) -> Self {
        let archive = CompactionArchive::with_max_snapshots(config.max_archived_snapshots);
        Self {
            config,
            compressor: TextCompressor::new(),
            archive,
        }
    }

    pub fn archive(&self) -> &CompactionArchive {
        &self.archive
    }

    pub fn restore_from_archive(&self) -> Option<MissionContext> {
        self.archive.restore_latest()
    }

    /// Check if compaction is needed using effective (best available) data.
    /// Uses actual usage from API responses if available, otherwise falls back to estimation.
    pub fn needs_compaction(&self, context: &MissionContext) -> Option<CompactionTrigger> {
        if context.token_budget.exceeds_threshold_effective() {
            let current = context.token_budget.current_input_tokens();
            let threshold = (context.token_budget.max_total as f32
                * context.token_budget.compaction_threshold) as usize;

            Some(CompactionTrigger::ThresholdExceeded { current, threshold })
        } else {
            None
        }
    }

    pub async fn compact(&self, context: &mut MissionContext) -> Result<CompactionResult> {
        // Use actual tokens if available for more accurate reporting
        let original_tokens = context.token_budget.current_input_tokens();
        let target_tokens =
            (context.token_budget.max_total as f32 * self.config.aggressive_ratio) as usize;

        info!(
            original = original_tokens,
            target = target_tokens,
            "Starting context compaction"
        );

        let phases_compressed = self.compress_old_phases(context).await?;
        let tasks_compressed = self.compress_completed_tasks(context).await?;
        let learnings_deduplicated = if self.config.enable_semantic_dedup {
            self.deduplicate_learnings(context).await?
        } else {
            0
        };

        let final_tokens = self.estimate_context_tokens(context);
        context
            .compaction_state
            .record_compaction(original_tokens, final_tokens);

        // Reset actual usage tracking after compaction.
        // The cumulative counts are no longer accurate after context reduction.
        // New API calls will start fresh tracking.
        context.token_budget.clear_actual_counts();

        let result = CompactionResult {
            original_tokens,
            final_tokens,
            compression_ratio: if original_tokens > 0 {
                final_tokens as f32 / original_tokens as f32
            } else {
                1.0
            },
            phases_compressed,
            tasks_compressed,
            learnings_deduplicated,
        };

        info!(
            ?result.compression_ratio,
            "Context compaction complete"
        );

        Ok(result)
    }

    async fn compress_old_phases(&self, context: &mut MissionContext) -> Result<usize> {
        let current_phase_idx = context
            .phase_summaries
            .iter()
            .position(|p| p.status == PhaseStatus::InProgress)
            .unwrap_or(context.phase_summaries.len());

        let mut compressed = 0;

        for (idx, phase) in context.phase_summaries.iter_mut().enumerate() {
            let age = current_phase_idx.saturating_sub(idx);
            if age >= self.config.min_phase_age_for_compression
                && phase.status == PhaseStatus::Completed
            {
                // Preserve critical information before compression
                // 1. Consolidate learnings into one_line_summary if not empty
                if !phase.learnings.is_empty() {
                    let learnings_summary = format!(" [Learnings: {}]", phase.learnings.join("; "));
                    let available_space = self
                        .config
                        .phase_summary_max_chars
                        .saturating_sub(phase.one_line_summary.len());
                    let truncated_learnings = if learnings_summary.len() > available_space {
                        format!(" [{}+ learnings]", phase.learnings.len())
                    } else {
                        learnings_summary
                    };
                    phase.one_line_summary.push_str(&truncated_learnings);
                }

                // 2. Preserve file count in key_changes before clearing
                if !phase.files_modified.is_empty() {
                    let file_note = format!("[{} files modified]", phase.files_modified.len());
                    if phase.key_changes.is_empty() {
                        phase.key_changes.push(file_note);
                    } else {
                        phase.key_changes[0] = format!("{} {}", phase.key_changes[0], file_note);
                    }
                }

                // Now compress
                phase.one_line_summary = self
                    .compressor
                    .compress(&phase.one_line_summary, self.config.phase_summary_max_chars);
                phase.key_changes.truncate(1);
                phase.files_modified.clear();
                phase.learnings.clear();
                compressed += 1;
            }
        }

        Ok(compressed)
    }

    async fn compress_completed_tasks(&self, context: &mut MissionContext) -> Result<usize> {
        let total_tasks = context.task_registry.tasks.len();
        let preserve_count = self.config.preserve_recent_tasks;

        let mut compressed = 0;

        for (idx, task) in context.task_registry.tasks.iter_mut().enumerate() {
            // Use verification_status as proxy for completion (status is now only in Mission.tasks)
            if idx < total_tasks.saturating_sub(preserve_count)
                && task.verification_status == VerificationStatus::Passed
            {
                // Preserve file count information in summary before clearing
                let file_info = if !task.files_changed.is_empty() {
                    format!(" [{} files]", task.files_changed.len())
                } else {
                    String::new()
                };

                if let Some(ref summary) = task.summary {
                    let enhanced_summary = format!("{}{}", summary, file_info);
                    task.summary = Some(
                        self.compressor
                            .compress(&enhanced_summary, self.config.task_summary_max_chars),
                    );
                } else if !file_info.is_empty() {
                    // Even without summary, preserve file count
                    task.summary = Some(format!("Completed{}", file_info));
                }

                task.description = task.one_line.clone();
                task.files_changed.clear();
                compressed += 1;
            }
        }

        Ok(compressed)
    }

    async fn deduplicate_learnings(&self, context: &mut MissionContext) -> Result<usize> {
        let original_count = context.summary.critical_learnings.len();
        let learnings_vec: Vec<String> =
            context.summary.critical_learnings.iter().cloned().collect();
        let unique = self
            .compressor
            .deduplicate(&learnings_vec, self.config.dedup_similarity_threshold);

        context.summary.critical_learnings = unique
            .into_iter()
            .take(self.config.preserve_recent_learnings)
            .collect();

        Ok(original_count.saturating_sub(context.summary.critical_learnings.len()))
    }

    fn estimate_context_tokens(&self, context: &MissionContext) -> usize {
        let summary_tokens = estimate_tokens(&context.summary.objective)
            + context
                .summary
                .critical_learnings
                .iter()
                .map(|l| estimate_tokens(l))
                .sum::<usize>();

        let phase_tokens: usize = context
            .phase_summaries
            .iter()
            .map(|p| {
                estimate_tokens(&p.one_line_summary)
                    + p.key_changes
                        .iter()
                        .map(|c| estimate_tokens(c))
                        .sum::<usize>()
            })
            .sum();

        let task_tokens: usize = context
            .task_registry
            .tasks
            .iter()
            .map(|t| {
                estimate_tokens(&t.one_line)
                    + t.summary.as_ref().map(|s| estimate_tokens(s)).unwrap_or(0)
            })
            .sum();

        summary_tokens + phase_tokens + task_tokens
    }

    pub async fn emergency_compact(&mut self, context: &mut MissionContext) -> Result<()> {
        warn!("Emergency compaction triggered!");

        // Archive context before destructive compaction
        self.archive.archive(context, "emergency_compaction");
        info!(
            archived_snapshots = self.archive.snapshot_count(),
            "Pre-compaction snapshot archived"
        );

        // Build a consolidated summary of what's being destroyed
        let mut destroyed_info = Vec::new();

        for phase in &mut context.phase_summaries {
            if phase.status == PhaseStatus::Completed {
                // Capture critical info before destruction
                if !phase.learnings.is_empty() {
                    destroyed_info.push(format!(
                        "Phase '{}': {} learnings archived",
                        phase.name,
                        phase.learnings.len()
                    ));
                }

                phase.one_line_summary =
                    format!("{}: {} tasks done", phase.name, phase.completed_count);
                phase.key_changes.clear();
                phase.files_modified.clear();
                phase.learnings.clear();
            }
        }

        let mut completed_task_count = 0;
        let mut total_files_affected = 0;

        for task in &mut context.task_registry.tasks {
            if task.verification_status == VerificationStatus::Passed {
                completed_task_count += 1;
                total_files_affected += task.files_changed.len();
                task.summary = None;
                task.description = task.one_line.clone();
                task.files_changed.clear();
            }
        }

        // Add summary note to critical learnings before truncation
        if completed_task_count > 0 || !destroyed_info.is_empty() {
            let emergency_note = format!(
                "[Emergency compaction: {} tasks/{} files compressed. Archive available for recovery.]",
                completed_task_count, total_files_affected
            );
            context.summary.critical_learnings.insert(0, emergency_note);
        }

        context.summary.critical_learnings = context
            .summary
            .critical_learnings
            .iter()
            .take(self.config.emergency_max_critical_learnings)
            .cloned()
            .collect();

        info!(
            destroyed_phases = destroyed_info.len(),
            compressed_tasks = completed_task_count,
            files_affected = total_files_affected,
            "Emergency compaction complete"
        );
        Ok(())
    }

    pub async fn compact_with_archive(
        &mut self,
        context: &mut MissionContext,
    ) -> Result<CompactionResult> {
        // Archive before any compaction
        self.archive.archive(context, "standard_compaction");
        self.compact(context).await
    }

    // =========================================================================
    // TaskContext compaction
    // =========================================================================

    /// Compact TaskContext by trimming collections to configured limits.
    /// Returns true if any compaction was performed.
    pub fn compact_task_context(&self, context: &mut TaskContext) -> bool {
        let usage = self.estimate_task_context_tokens(context);
        if usage <= self.config.task_context_threshold {
            return false;
        }

        info!(
            usage = usage,
            threshold = self.config.task_context_threshold,
            "TaskContext compaction triggered"
        );

        let mut compacted = false;

        if context.key_findings.len() > self.config.max_key_findings {
            let keep_count = self.config.max_key_findings / 2;
            context
                .key_findings
                .drain(..context.key_findings.len() - keep_count);
            compacted = true;
        }

        if context.blockers.len() > self.config.max_blockers {
            let keep_count = self.config.max_blockers / 2;
            context
                .blockers
                .drain(..context.blockers.len() - keep_count);
            compacted = true;
        }

        if context.related_files.len() > self.config.max_related_files {
            let keep_count = self.config.max_related_files / 2;
            context
                .related_files
                .drain(..context.related_files.len() - keep_count);
            compacted = true;
        }

        if compacted {
            debug!(
                key_findings = context.key_findings.len(),
                blockers = context.blockers.len(),
                related_files = context.related_files.len(),
                "TaskContext compacted"
            );
        }

        compacted
    }

    fn estimate_task_context_tokens(&self, context: &TaskContext) -> usize {
        let key_findings: usize = context.key_findings.iter().map(|f| f.len()).sum();
        let blockers: usize = context.blockers.iter().map(|b| b.len()).sum();
        let related_files: usize = context.related_files.iter().map(|f| f.len()).sum();
        let composed_prompt = context
            .composed_prompt
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(0);
        (key_findings + blockers + related_files + composed_prompt) / 4
    }
}
