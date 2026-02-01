use std::path::PathBuf;

use chrono::Utc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, warn};

use super::MissionEvent;
use crate::config::NotificationConfig;

#[derive(Clone)]
pub struct Notifier {
    config: NotificationConfig,
    logs_dir: Option<PathBuf>,
}

impl Notifier {
    pub fn new(config: NotificationConfig, logs_dir: Option<PathBuf>) -> Self {
        Self { config, logs_dir }
    }

    pub async fn notify(&self, event: &MissionEvent) {
        if !self.config.enabled {
            return;
        }

        // Desktop notifications only for mission-level events to avoid noise
        if self.config.desktop && event.event_type.is_mission_level() {
            self.send_desktop_notification(event).await;
        }

        if self.config.event_log {
            self.write_event_log(event).await;
        }

        if let Some(hook) = &self.config.hook_command {
            self.run_hook(hook, event).await;
        }
    }

    async fn send_desktop_notification(&self, event: &MissionEvent) {
        let title = event.title();
        let body = event.body();

        #[cfg(target_os = "macos")]
        {
            let script = format!(
                r#"display notification "{}" with title "{}""#,
                body.replace('"', r#"\""#).replace('\n', " "),
                title.replace('"', r#"\""#)
            );

            let result = Command::new("osascript")
                .args(["-e", &script])
                .output()
                .await;

            if let Err(e) = result {
                debug!(error = %e, "Failed to send desktop notification");
            }
        }

        #[cfg(target_os = "linux")]
        {
            let result = Command::new("notify-send")
                .args([&title, &body])
                .output()
                .await;

            if let Err(e) = result {
                debug!(error = %e, "Failed to send desktop notification");
            }
        }

        #[cfg(target_os = "windows")]
        {
            let script = format!(
                r#"[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); $text = $template.GetElementsByTagName('text'); $text[0].AppendChild($template.CreateTextNode('{}')) | Out-Null; $text[1].AppendChild($template.CreateTextNode('{}')) | Out-Null; $toast = [Windows.UI.Notifications.ToastNotification]::new($template); [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Claude-Pilot').Show($toast)"#,
                title.replace("'", "''"),
                body.replace("'", "''").replace('\n', " ")
            );

            let result = Command::new("powershell")
                .args(["-Command", &script])
                .output()
                .await;

            if let Err(e) = result {
                debug!(error = %e, "Failed to send desktop notification");
            }
        }
    }

    async fn write_event_log(&self, event: &MissionEvent) {
        let Some(logs_dir) = &self.logs_dir else {
            return;
        };

        let log_path = logs_dir.join(format!("{}.log", event.mission_id));
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let log_line = format!(
            "[{}] {}: {}\n",
            timestamp,
            event.event_type.as_str(),
            event.message.as_deref().unwrap_or("")
        );

        if let Err(e) = tokio::fs::create_dir_all(logs_dir).await {
            warn!(error = %e, "Failed to create logs directory");
            return;
        }

        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await;

        match result {
            Ok(mut file) => {
                if let Err(e) = file.write_all(log_line.as_bytes()).await {
                    warn!(error = %e, "Failed to write event log");
                }
            }
            Err(e) => {
                warn!(error = %e, path = %log_path.display(), "Failed to open event log");
            }
        }
    }

    async fn run_hook(&self, hook_cmd: &str, event: &MissionEvent) {
        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(_) => return,
        };

        let result = Command::new("sh")
            .args(["-c", hook_cmd])
            .env("PILOT_EVENT", event.event_type.as_str())
            .env("PILOT_MISSION_ID", &event.mission_id)
            .env("PILOT_EVENT_JSON", &json)
            .output()
            .await;

        if let Err(e) = result {
            debug!(error = %e, hook = %hook_cmd, "Failed to run hook");
        }
    }

    pub async fn notify_escalation(
        &self,
        mission_id: &str,
        message: &str,
    ) -> crate::error::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        if self.config.desktop {
            let event = MissionEvent::new(super::EventType::MissionFailed, mission_id)
                .with_message(format!("Escalation: {}", message));
            self.send_desktop_notification(&event).await;
        }

        debug!(mission_id, message, "Escalation notification sent");
        Ok(())
    }
}
