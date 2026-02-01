use std::path::{Path, PathBuf};

use claude_agent::session::{JsonlConfig, SyncMode};

pub struct SessionConfig {
    base_dir: PathBuf,
}

impl SessionConfig {
    pub fn new(project_root: &Path) -> Self {
        Self {
            base_dir: project_root.join(".claude/pilot/sessions"),
        }
    }

    pub fn mission_sessions_dir(&self, mission_id: &str) -> PathBuf {
        self.base_dir.join(mission_id)
    }

    pub fn to_jsonl_config(&self, mission_id: &str) -> JsonlConfig {
        JsonlConfig::builder()
            .base_dir(self.mission_sessions_dir(mission_id))
            .retention_days(90)
            .sync_mode(SyncMode::OnWrite)
            .build()
    }
}
