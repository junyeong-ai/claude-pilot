use std::path::{Path, PathBuf};

use tokio::fs;
use tracing::warn;

use super::types::MissionContext;
use crate::error::Result;

pub struct ContextPersistence {
    base_dir: PathBuf,
}

impl ContextPersistence {
    pub fn new(missions_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: missions_dir.as_ref().to_path_buf(),
        }
    }

    fn context_dir(&self, mission_id: &str) -> PathBuf {
        self.base_dir.join(mission_id).join("context")
    }

    fn context_file(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("mission-context.yaml")
    }

    fn summary_file(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("mission-summary.md")
    }

    fn task_registry_file(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("task-registry.yaml")
    }

    fn token_budget_file(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("token-budget.yaml")
    }

    fn compaction_log_file(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("compaction-log.yaml")
    }

    fn phase_summaries_dir(&self, mission_id: &str) -> PathBuf {
        self.context_dir(mission_id).join("phase-summaries")
    }

    pub async fn exists(&self, mission_id: &str) -> bool {
        self.context_file(mission_id).exists()
    }

    pub async fn save(&self, context: &MissionContext) -> Result<()> {
        let context_dir = self.context_dir(&context.mission_id);
        fs::create_dir_all(&context_dir).await?;

        let yaml = serde_yaml_bw::to_string(context)?;
        fs::write(self.context_file(&context.mission_id), &yaml).await?;

        self.save_summary_markdown(context).await?;
        self.save_task_registry(context).await?;
        self.save_token_budget(context).await?;
        self.save_phase_summaries(context).await?;

        Ok(())
    }

    pub async fn load(&self, mission_id: &str) -> Result<MissionContext> {
        let content = fs::read_to_string(self.context_file(mission_id)).await?;
        let context: MissionContext = serde_yaml_bw::from_str(&content)?;
        Ok(context)
    }

    pub async fn load_or_init(&self, mission_id: &str) -> Result<MissionContext> {
        if self.exists(mission_id).await {
            self.load(mission_id).await
        } else {
            let context = MissionContext::new(mission_id);
            self.save(&context).await?;
            Ok(context)
        }
    }

    async fn save_summary_markdown(&self, context: &MissionContext) -> Result<()> {
        let summary = &context.summary;
        let md = format!(
            r"# Mission Summary

## Objective
{}

## Progress
- Current Phase: {}
- Completed: {}%

## Blockers
{}

## Critical Learnings
{}

## Key Decisions
{}

---
Last updated: {}
",
            summary.objective,
            summary.current_phase,
            summary.progress.percentage,
            if summary.blockers.is_empty() {
                "None".to_string()
            } else {
                summary
                    .blockers
                    .iter()
                    .map(|b| format!("- {}", b))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            if summary.critical_learnings.is_empty() {
                "None".to_string()
            } else {
                summary
                    .critical_learnings
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{}. {}", i + 1, l))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            if summary.key_decisions.is_empty() {
                "None".to_string()
            } else {
                summary
                    .key_decisions
                    .iter()
                    .enumerate()
                    .map(|(i, d)| format!("{}. {}", i + 1, d))
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            context.last_updated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );

        fs::write(self.summary_file(&context.mission_id), md).await?;
        Ok(())
    }

    async fn save_task_registry(&self, context: &MissionContext) -> Result<()> {
        let yaml = serde_yaml_bw::to_string(&context.task_registry)?;
        fs::write(self.task_registry_file(&context.mission_id), yaml).await?;
        Ok(())
    }

    async fn save_token_budget(&self, context: &MissionContext) -> Result<()> {
        let yaml = serde_yaml_bw::to_string(&context.token_budget)?;
        fs::write(self.token_budget_file(&context.mission_id), yaml).await?;
        Ok(())
    }

    async fn save_phase_summaries(&self, context: &MissionContext) -> Result<()> {
        let phase_dir = self.phase_summaries_dir(&context.mission_id);
        fs::create_dir_all(&phase_dir).await?;

        for phase in &context.phase_summaries {
            let md = format!(
                r"# {}

**Status**: {:?}
**Progress**: {}/{}

## Summary
{}

## Key Changes
{}

## Files Modified
{}

## Learnings
{}
",
                phase.name,
                phase.status,
                phase.completed_count,
                phase.task_count,
                phase.one_line_summary,
                if phase.key_changes.is_empty() {
                    "None".to_string()
                } else {
                    phase
                        .key_changes
                        .iter()
                        .map(|c| format!("- {}", c))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
                if phase.files_modified.is_empty() {
                    "None".to_string()
                } else {
                    phase
                        .files_modified
                        .iter()
                        .map(|f| format!("- `{}`", f))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
                if phase.learnings.is_empty() {
                    "None".to_string()
                } else {
                    phase
                        .learnings
                        .iter()
                        .map(|l| format!("- {}", l))
                        .collect::<Vec<_>>()
                        .join("\n")
                },
            );

            let filename = format!("{}.md", phase.phase_id.to_lowercase().replace(' ', "-"));
            fs::write(phase_dir.join(filename), md).await?;
        }

        Ok(())
    }

    pub async fn save_compaction_log(
        &self,
        mission_id: &str,
        result: &super::CompactionResult,
    ) -> Result<()> {
        let log_file = self.compaction_log_file(mission_id);

        let mut entries: Vec<serde_yaml_bw::Value> = if log_file.exists() {
            let content = fs::read_to_string(&log_file).await?;
            match serde_yaml_bw::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        file = ?log_file,
                        error = %e,
                        "Failed to parse compaction log, starting fresh"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let entry = serde_yaml_bw::to_value(result)?;
        entries.push(entry);

        let yaml = serde_yaml_bw::to_string(&entries)?;
        fs::write(log_file, yaml).await?;

        Ok(())
    }

    pub async fn delete(&self, mission_id: &str) -> Result<()> {
        let context_dir = self.context_dir(mission_id);
        if context_dir.exists() {
            fs::remove_dir_all(context_dir).await?;
        }
        Ok(())
    }
}
