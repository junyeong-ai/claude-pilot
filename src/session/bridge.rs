use std::path::Path;
use std::sync::Arc;

use claude_agent::session::{
    JsonlPersistence, Session, SessionConfig as AgentSessionConfig, SessionId, SessionManager,
    SessionMessage,
};
use claude_agent::types::ContentBlock;

use super::SessionConfig;
use crate::error::Result;
use crate::mission::{Mission, Task};

pub struct SessionBridge {
    config: SessionConfig,
    managers: tokio::sync::RwLock<std::collections::HashMap<String, Arc<SessionManager>>>,
}

impl SessionBridge {
    pub fn new(project_root: &Path) -> Self {
        Self {
            config: SessionConfig::new(project_root),
            managers: tokio::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub async fn get_or_create_manager(&self, mission_id: &str) -> Result<Arc<SessionManager>> {
        let mut managers = self.managers.write().await;

        if let Some(manager) = managers.get(mission_id) {
            return Ok(Arc::clone(manager));
        }

        let jsonl_config = self.config.to_jsonl_config(mission_id);
        let persistence = JsonlPersistence::new(jsonl_config).await.map_err(|e| {
            crate::error::PilotError::Session(format!("Failed to create persistence: {}", e))
        })?;

        let manager = Arc::new(SessionManager::new(Arc::new(persistence)));
        managers.insert(mission_id.to_string(), Arc::clone(&manager));

        Ok(manager)
    }

    pub async fn create_task_session(
        &self,
        mission: &Mission,
        task: &Task,
    ) -> Result<(SessionId, Arc<SessionManager>)> {
        let manager = self.get_or_create_manager(&mission.id).await?;

        let session = manager
            .create(AgentSessionConfig::default())
            .await
            .map_err(|e| {
                crate::error::PilotError::Session(format!("Failed to create session: {}", e))
            })?;

        let context = format!(
            "Mission: {} ({})\nTask: {} - {}\nPhase: {}",
            mission.id,
            mission.description,
            task.id,
            task.description,
            task.phase.as_deref().unwrap_or("default")
        );

        manager
            .add_message(
                &session.id,
                SessionMessage::user(vec![ContentBlock::text(context)]),
            )
            .await
            .map_err(|e| {
                crate::error::PilotError::Session(format!("Failed to add message: {}", e))
            })?;

        Ok((session.id, manager))
    }

    pub async fn create_planning_session(
        &self,
        mission: &Mission,
        phase: &str,
    ) -> Result<(SessionId, Arc<SessionManager>)> {
        let manager = self.get_or_create_manager(&mission.id).await?;

        let session = manager
            .create(AgentSessionConfig::default())
            .await
            .map_err(|e| {
                crate::error::PilotError::Session(format!("Failed to create session: {}", e))
            })?;

        let context = format!(
            "Mission: {} ({})\nPlanning Phase: {}",
            mission.id, mission.description, phase
        );

        manager
            .add_message(
                &session.id,
                SessionMessage::user(vec![ContentBlock::text(context)]),
            )
            .await
            .map_err(|e| {
                crate::error::PilotError::Session(format!("Failed to add message: {}", e))
            })?;

        Ok((session.id, manager))
    }

    pub async fn load_session(
        &self,
        mission_id: &str,
        session_id: &SessionId,
    ) -> Result<Option<Session>> {
        let manager = self.get_or_create_manager(mission_id).await?;

        manager.get(session_id).await.map(Some).or_else(|e| {
            if matches!(e, claude_agent::session::SessionError::NotFound { .. }) {
                Ok(None)
            } else {
                Err(crate::error::PilotError::Session(format!(
                    "Failed to load session: {}",
                    e
                )))
            }
        })
    }

    pub async fn add_user_message(
        &self,
        mission_id: &str,
        session_id: &SessionId,
        content: &str,
    ) -> Result<()> {
        let manager = self.get_or_create_manager(mission_id).await?;

        manager
            .add_message(
                session_id,
                SessionMessage::user(vec![ContentBlock::text(content)]),
            )
            .await
            .map_err(|e| crate::error::PilotError::Session(format!("Failed to add message: {}", e)))
    }

    pub async fn add_assistant_message(
        &self,
        mission_id: &str,
        session_id: &SessionId,
        content: &str,
    ) -> Result<()> {
        let manager = self.get_or_create_manager(mission_id).await?;

        manager
            .add_message(
                session_id,
                SessionMessage::assistant(vec![ContentBlock::text(content)]),
            )
            .await
            .map_err(|e| crate::error::PilotError::Session(format!("Failed to add message: {}", e)))
    }

    pub fn sessions_dir(&self, mission_id: &str) -> std::path::PathBuf {
        self.config.mission_sessions_dir(mission_id)
    }
}
