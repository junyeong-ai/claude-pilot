//! Agent message bus for inter-agent communication.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tracing::{debug, warn};

use super::message::{AgentMessage, MessageType};
use crate::error::{PilotError, Result};
use crate::state::{DomainEvent, EventPayload, EventStore};

const DEFAULT_CHANNEL_CAPACITY: usize = 256;

pub struct AgentMessageBus {
    sender: broadcast::Sender<AgentMessage>,
    event_store: Option<Arc<EventStore>>,
}

impl AgentMessageBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            event_store: None,
        }
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub async fn send(&self, message: AgentMessage) -> Result<()> {
        if let Some(store) = &self.event_store {
            self.record_send_event(store, &message).await?;
        }

        self.sender
            .send(message)
            .map_err(|_| PilotError::Agent("No active receivers for message".into()))?;

        Ok(())
    }

    pub fn try_send(&self, message: AgentMessage) -> Result<()> {
        self.sender
            .send(message)
            .map_err(|_| PilotError::Agent("No active receivers for message".into()))?;
        Ok(())
    }

    pub fn subscribe(&self, agent_id: impl Into<String>) -> AgentReceiver {
        AgentReceiver {
            agent_id: agent_id.into(),
            receiver: self.sender.subscribe(),
            event_store: self.event_store.clone(),
        }
    }

    pub fn subscribe_filtered(
        &self,
        agent_id: impl Into<String>,
        types: Vec<MessageType>,
    ) -> FilteredReceiver {
        FilteredReceiver {
            inner: self.subscribe(agent_id),
            allowed_types: types,
        }
    }

    pub async fn request(&self, message: AgentMessage, timeout: Duration) -> Result<AgentMessage> {
        let correlation_id = message.correlation_id.clone();
        let mut receiver = self.subscribe(&message.from);

        self.send(message).await?;

        tokio::time::timeout(timeout, async {
            loop {
                match receiver.recv().await {
                    Ok(Some(reply)) if reply.reply_to.as_ref() == Some(&correlation_id) => {
                        return Ok(reply);
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        return Err(PilotError::Agent("Channel closed".into()));
                    }
                    Err(e) => return Err(e),
                }
            }
        })
        .await
        .map_err(|_| PilotError::Timeout("Message request timed out".into()))?
    }

    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }

    async fn record_send_event(&self, store: &EventStore, message: &AgentMessage) -> Result<()> {
        let event = DomainEvent::new(
            &message.correlation_id,
            EventPayload::AgentMessageSent {
                from_agent: message.from.clone(),
                to_agent: message.to.clone(),
                message_type: format!("{:?}", message.message_type()),
                correlation_id: message.correlation_id.clone(),
            },
        );
        store.append(event).await?;
        Ok(())
    }
}

impl Default for AgentMessageBus {
    fn default() -> Self {
        Self::new(DEFAULT_CHANNEL_CAPACITY)
    }
}

pub struct AgentReceiver {
    agent_id: String,
    receiver: broadcast::Receiver<AgentMessage>,
    event_store: Option<Arc<EventStore>>,
}

impl AgentReceiver {
    pub async fn recv(&mut self) -> Result<Option<AgentMessage>> {
        loop {
            match self.receiver.recv().await {
                Ok(msg) if msg.is_for(&self.agent_id) => {
                    if let Some(store) = &self.event_store
                        && let Err(e) = self.record_receive_event(store, &msg).await
                    {
                        warn!(
                            error = %e,
                            msg_id = %msg.id,
                            from = %msg.from,
                            "Failed to record receive event"
                        );
                    }
                    return Ok(Some(msg));
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Closed) => return Ok(None),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(agent = %self.agent_id, skipped = n, "Receiver lagged");
                    continue;
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<Option<AgentMessage>> {
        loop {
            match self.receiver.try_recv() {
                Ok(msg) if msg.is_for(&self.agent_id) => return Ok(Some(msg)),
                Ok(_) => continue,
                Err(broadcast::error::TryRecvError::Empty) => return Ok(None),
                Err(broadcast::error::TryRecvError::Closed) => return Ok(None),
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }
    }

    async fn record_receive_event(&self, store: &EventStore, message: &AgentMessage) -> Result<()> {
        let event = DomainEvent::new(
            &message.correlation_id,
            EventPayload::AgentMessageReceived {
                from_agent: message.from.clone(),
                to_agent: self.agent_id.clone(),
                message_type: format!("{:?}", message.message_type()),
                correlation_id: message.correlation_id.clone(),
            },
        );
        store.append(event).await?;
        Ok(())
    }
}

pub struct FilteredReceiver {
    inner: AgentReceiver,
    allowed_types: Vec<MessageType>,
}

impl FilteredReceiver {
    pub async fn recv(&mut self) -> Result<Option<AgentMessage>> {
        loop {
            match self.inner.recv().await? {
                Some(msg) if self.allowed_types.contains(&msg.message_type()) => {
                    return Ok(Some(msg));
                }
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<Option<AgentMessage>> {
        loop {
            match self.inner.try_recv()? {
                Some(msg) if self.allowed_types.contains(&msg.message_type()) => {
                    return Ok(Some(msg));
                }
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::messaging::message::MessagePayload;

    #[tokio::test]
    async fn test_send_receive() {
        let bus = AgentMessageBus::new(16);
        let mut receiver = bus.subscribe("agent-1");

        let msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::Text {
                content: "hello".into(),
            },
        );

        bus.try_send(msg).unwrap();

        let received = receiver.try_recv().unwrap();
        assert!(received.is_some());
        assert_eq!(received.unwrap().from, "sender");
    }

    #[tokio::test]
    async fn test_broadcast() {
        let bus = AgentMessageBus::new(16);
        let mut receiver1 = bus.subscribe("agent-1");
        let mut receiver2 = bus.subscribe("agent-2");

        let msg = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::Text {
                content: "announcement".into(),
            },
        );

        bus.try_send(msg).unwrap();

        assert!(receiver1.try_recv().unwrap().is_some());
        assert!(receiver2.try_recv().unwrap().is_some());
    }

    #[tokio::test]
    async fn test_filtered_receiver() {
        let bus = AgentMessageBus::new(16);
        let mut filtered = bus.subscribe_filtered("agent-1", vec![MessageType::ConsensusVote]);

        bus.try_send(AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::TaskAssignment {
                task: crate::agent::multi::traits::AgentTask::new("task-1", "test task"),
            },
        ))
        .unwrap();

        bus.try_send(AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::ConsensusVote {
                round: 1,
                decision: crate::agent::multi::shared::VoteDecision::Approve,
                rationale: "vote".into(),
            },
        ))
        .unwrap();

        let received = filtered.try_recv().unwrap();
        assert!(received.is_some());
        if let MessagePayload::ConsensusVote { round, .. } = received.unwrap().payload {
            assert_eq!(round, 1);
        } else {
            panic!("Expected ConsensusVote message after filtering");
        }
    }

    #[test]
    fn test_receiver_count() {
        let bus = AgentMessageBus::new(16);
        assert_eq!(bus.receiver_count(), 0);

        let _r1 = bus.subscribe("agent-1");
        assert_eq!(bus.receiver_count(), 1);

        let _r2 = bus.subscribe("agent-2");
        assert_eq!(bus.receiver_count(), 2);
    }
}
