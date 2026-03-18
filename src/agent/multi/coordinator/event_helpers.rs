use std::path::Path;

use tracing::{debug, warn};

use crate::domain::Severity;
use super::super::messaging::{AgentMessage, MessagePayload};
use super::super::traits::{AgentTask, AgentTaskResult};
use super::Coordinator;
use crate::state::DomainEvent;

impl Coordinator {
    pub(super) async fn emit_event(&self, event: DomainEvent) {
        self.emit_event_internal(event, false).await;
    }

    pub(super) async fn emit_critical_event(&self, event: DomainEvent) {
        self.emit_event_internal(event, true).await;
    }

    async fn emit_event_internal(&self, event: DomainEvent, is_critical: bool) {
        if let Some(store) = &self.event_store {
            let event_type = event.event_type();
            if let Err(e) = store.append(event).await {
                if is_critical {
                    tracing::error!(
                        error = %e,
                        event_type,
                        "Failed to append critical event to store - audit trail may be incomplete"
                    );
                } else {
                    warn!(error = %e, event_type, "Failed to emit event");
                }
            }
        }
    }

    pub(super) async fn emit_manifest_context_event(
        &self,
        mission_id: &str,
        agent_id: &str,
        task_id: &str,
        module_ids: &[String],
    ) {
        self.emit_event(DomainEvent::manifest_context_injected(
            mission_id,
            agent_id,
            task_id,
            module_ids.to_vec(),
        ))
        .await;
    }

    pub(super) async fn broadcast_task_assignment(&self, task: &AgentTask, agent_id: &str) {
        if let Some(bus) = &self.message_bus {
            let message = AgentMessage::task_assignment("coordinator", agent_id, task.clone());
            if let Err(e) = bus.send(message).await {
                debug!(error = %e, "Failed to broadcast task assignment");
            }
        }
    }

    pub(super) async fn broadcast_task_result(&self, result: &AgentTaskResult, from_agent: &str) {
        if let Some(bus) = &self.message_bus {
            let message = AgentMessage::task_result(from_agent, "coordinator", result.clone());
            if let Err(e) = bus.send(message).await {
                debug!(error = %e, "Failed to send task result to coordinator");
            }

            let broadcast_msg = AgentMessage::new(
                from_agent,
                "*",
                MessagePayload::TaskResult {
                    result: result.clone(),
                },
            );
            if let Err(e) = bus.send(broadcast_msg).await {
                debug!(error = %e, "Failed to broadcast task result to all agents");
            }
        }
    }

    pub(super) async fn broadcast_conflict_alert(
        &self,
        file: &Path,
        description: &str,
        severity: Severity,
    ) {
        let Some(bus) = &self.message_bus else { return };

        let msg = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::ConflictAlert {
                conflict_id: uuid::Uuid::new_v4().to_string(),
                severity,
                description: format!("{}: {}", file.display(), description),
            },
        );

        if let Err(e) = bus.send(msg).await {
            debug!(error = %e, file = %file.display(), "Failed to broadcast conflict alert");
        }
    }
}
