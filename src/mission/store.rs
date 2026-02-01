use std::path::{Path, PathBuf};

use tokio::fs;
use tracing::debug;

use super::Mission;
use crate::error::{PilotError, Result};
use crate::state::MissionState;

pub struct MissionStore {
    missions_dir: PathBuf,
}

impl MissionStore {
    pub fn new(pilot_dir: &Path) -> Self {
        Self {
            missions_dir: pilot_dir.join("missions"),
        }
    }

    pub async fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.missions_dir).await?;
        self.recover_interrupted_writes().await;
        Ok(())
    }

    pub async fn save(&self, mission: &Mission) -> Result<()> {
        let path = self.mission_path(&mission.id);
        let content = serde_yaml_bw::to_string(mission)?;
        self.write_atomic(&path, &content).await
    }

    async fn write_atomic(&self, path: &Path, content: &str) -> Result<()> {
        let tmp_path = path.with_extension("yaml.tmp");

        // 1. Write to temp file
        fs::write(&tmp_path, content).await?;

        // 2. Sync to disk using spawn_blocking to avoid blocking async runtime
        let tmp_path_clone = tmp_path.clone();
        let sync_result = tokio::task::spawn_blocking(move || {
            std::fs::File::open(&tmp_path_clone).and_then(|file| file.sync_all())
        })
        .await;

        if let Err(e) = sync_result {
            tracing::warn!(error = %e, "Failed to sync temp file to disk");
        } else if let Ok(Err(e)) = sync_result {
            tracing::warn!(error = %e, "Failed to sync temp file to disk");
        }

        // 3. Atomic rename (POSIX guarantees atomicity)
        fs::rename(&tmp_path, path).await?;

        debug!(path = %path.display(), "Atomic write completed");
        Ok(())
    }

    async fn recover_interrupted_writes(&self) {
        if let Ok(mut entries) = fs::read_dir(&self.missions_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "tmp") {
                    debug!(path = %path.display(), "Removing interrupted write");
                    let _ = fs::remove_file(&path).await;
                }
            }
        }
    }

    pub async fn load(&self, mission_id: &str) -> Result<Mission> {
        let path = self.mission_path(mission_id);
        if !path.exists() {
            return Err(PilotError::MissionNotFound(mission_id.to_string()));
        }
        let content = fs::read_to_string(&path).await?;
        let mission: Mission = serde_yaml_bw::from_str(&content)?;
        Ok(mission)
    }

    pub async fn delete(&self, mission_id: &str) -> Result<()> {
        let path = self.mission_path(mission_id);
        if path.exists() {
            fs::remove_file(&path).await?;
        }
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<Mission>> {
        let mut missions = Vec::new();

        if !self.missions_dir.exists() {
            return Ok(missions);
        }

        let mut entries = fs::read_dir(&self.missions_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "yaml")
                && let Ok(content) = fs::read_to_string(&path).await
                && let Ok(mission) = serde_yaml_bw::from_str::<Mission>(&content)
            {
                missions.push(mission);
            }
        }

        missions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(missions)
    }

    pub async fn list_by_status(&self, status: MissionState) -> Result<Vec<Mission>> {
        let missions = self.list().await?;
        Ok(missions
            .into_iter()
            .filter(|m| m.status == status)
            .collect())
    }

    pub async fn get_active(&self) -> Result<Vec<Mission>> {
        let missions = self.list().await?;
        Ok(missions
            .into_iter()
            .filter(|m| m.status.is_active())
            .collect())
    }

    pub async fn exists(&self, mission_id: &str) -> bool {
        self.mission_path(mission_id).exists()
    }

    pub async fn next_id(&self) -> Result<String> {
        let missions = self.list().await?;
        let max_num = missions
            .iter()
            .filter_map(|m| m.id.strip_prefix("m-").and_then(|s| s.parse::<u32>().ok()))
            .max()
            .unwrap_or(0);

        Ok(format!("m-{:03}", max_num + 1))
    }

    fn mission_path(&self, mission_id: &str) -> PathBuf {
        self.missions_dir.join(format!("{}.yaml", mission_id))
    }
}
