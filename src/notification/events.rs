use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    MissionCreated,
    MissionStarted,
    MissionCompleted,
    MissionFailed,
    MissionCancelled,
    MissionPaused,
    TaskStarted,
    TaskCompleted,
    TaskFailed,
    VerificationFailed,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissionCreated => "mission.created",
            Self::MissionStarted => "mission.started",
            Self::MissionCompleted => "mission.completed",
            Self::MissionFailed => "mission.failed",
            Self::MissionCancelled => "mission.cancelled",
            Self::MissionPaused => "mission.paused",
            Self::TaskStarted => "task.started",
            Self::TaskCompleted => "task.completed",
            Self::TaskFailed => "task.failed",
            Self::VerificationFailed => "verification.failed",
        }
    }

    pub fn emoji(&self) -> &'static str {
        match self {
            Self::MissionCreated => "🚀",
            Self::MissionStarted => "▶️",
            Self::MissionCompleted => "✅",
            Self::MissionFailed => "❌",
            Self::MissionCancelled => "🚫",
            Self::MissionPaused => "⏸️",
            Self::TaskStarted => "🔄",
            Self::TaskCompleted => "✔️",
            Self::TaskFailed => "⚠️",
            Self::VerificationFailed => "✗",
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(
            self,
            Self::MissionFailed | Self::TaskFailed | Self::VerificationFailed
        )
    }

    pub fn is_mission_level(&self) -> bool {
        matches!(
            self,
            Self::MissionCreated
                | Self::MissionStarted
                | Self::MissionCompleted
                | Self::MissionFailed
                | Self::MissionCancelled
                | Self::MissionPaused
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionEvent {
    pub event_type: EventType,
    pub mission_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<(usize, usize)>,
}

impl MissionEvent {
    pub fn new(event_type: EventType, mission_id: impl Into<String>) -> Self {
        Self {
            event_type,
            mission_id: mission_id.into(),
            created_at: Utc::now(),
            task_id: None,
            message: None,
            progress: None,
        }
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    pub fn with_progress(mut self, completed: usize, total: usize) -> Self {
        self.progress = Some((completed, total));
        self
    }

    pub fn title(&self) -> String {
        format!(
            "{} Claude-Pilot: {}",
            self.event_type.emoji(),
            self.event_type.as_str()
        )
    }

    pub fn body(&self) -> String {
        let mut parts = vec![format!("Mission: {}", self.mission_id)];

        if let Some(task_id) = &self.task_id {
            parts.push(format!("Task: {}", task_id));
        }

        if let Some((completed, total)) = self.progress {
            parts.push(format!("Progress: {}/{}", completed, total));
        }

        if let Some(msg) = &self.message {
            parts.push(msg.clone());
        }

        parts.join("\n")
    }
}
