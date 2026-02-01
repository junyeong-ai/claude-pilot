//! Message handler trait for agents to receive and process messages.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use parking_lot::RwLock;

use super::message::{AgentMessage, MessageType};
use crate::error::Result;

/// Handler response indicating how the message was processed.
#[derive(Debug, Clone)]
pub enum HandlerResponse {
    /// Message was handled successfully.
    Handled,
    /// Message was handled and a reply should be sent.
    Reply(Box<AgentMessage>),
    /// Message was not handled by this handler.
    NotHandled,
    /// Handler failed to process the message.
    Error(String),
}

/// Trait for handling incoming messages.
///
/// Implement this trait to receive messages from the message bus.
/// The handler can filter messages by type and process them asynchronously.
pub trait MessageHandler: Send + Sync {
    /// Get the agent ID this handler is for.
    fn agent_id(&self) -> &str;

    /// Get the message types this handler is interested in.
    /// Return empty slice to receive all message types.
    fn subscribed_types(&self) -> &[MessageType] {
        &[]
    }

    /// Handle an incoming message.
    fn handle<'a>(
        &'a self,
        message: &'a AgentMessage,
    ) -> Pin<Box<dyn Future<Output = Result<HandlerResponse>> + Send + 'a>>;

    /// Check if this handler should process the given message.
    fn should_handle(&self, message: &AgentMessage) -> bool {
        // Check if message is for this agent
        if !message.is_for(self.agent_id()) {
            return false;
        }

        // Check message type filter
        let subscribed = self.subscribed_types();
        if subscribed.is_empty() {
            return true;
        }
        subscribed.contains(&message.message_type())
    }
}

/// Type alias for boxed message handlers.
pub type BoxedHandler = Arc<dyn MessageHandler>;

/// Registry for message handlers.
///
/// Manages multiple handlers and dispatches messages to appropriate handlers.
#[derive(Default)]
pub struct MessageHandlerRegistry {
    handlers: RwLock<HashMap<String, BoxedHandler>>,
}

impl MessageHandlerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for an agent.
    pub fn register(&self, handler: BoxedHandler) {
        let agent_id = handler.agent_id().to_string();
        self.handlers.write().insert(agent_id, handler);
    }

    /// Unregister a handler.
    pub fn unregister(&self, agent_id: &str) -> bool {
        self.handlers.write().remove(agent_id).is_some()
    }

    /// Get a handler by agent ID.
    pub fn get(&self, agent_id: &str) -> Option<BoxedHandler> {
        self.handlers.read().get(agent_id).cloned()
    }

    /// Get all handlers that should process the given message.
    pub fn handlers_for(&self, message: &AgentMessage) -> Vec<BoxedHandler> {
        self.handlers
            .read()
            .values()
            .filter(|h| h.should_handle(message))
            .cloned()
            .collect()
    }

    /// Dispatch a message to all relevant handlers.
    pub async fn dispatch(&self, message: &AgentMessage) -> Vec<(String, Result<HandlerResponse>)> {
        let handlers = self.handlers_for(message);
        let mut results = Vec::with_capacity(handlers.len());

        for handler in handlers {
            let agent_id = handler.agent_id().to_string();
            let result = handler.handle(message).await;
            results.push((agent_id, result));
        }

        results
    }

    /// Get the number of registered handlers.
    pub fn handler_count(&self) -> usize {
        self.handlers.read().len()
    }

    /// Get all registered agent IDs.
    pub fn registered_agents(&self) -> Vec<String> {
        self.handlers.read().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::messaging::MessagePayload;

    struct TestHandler {
        agent_id: String,
        types: Vec<MessageType>,
    }

    impl MessageHandler for TestHandler {
        fn agent_id(&self) -> &str {
            &self.agent_id
        }

        fn subscribed_types(&self) -> &[MessageType] {
            &self.types
        }

        fn handle<'a>(
            &'a self,
            _message: &'a AgentMessage,
        ) -> Pin<Box<dyn Future<Output = Result<HandlerResponse>> + Send + 'a>> {
            Box::pin(async { Ok(HandlerResponse::Handled) })
        }
    }

    #[test]
    fn test_registry_register_unregister() {
        let registry = MessageHandlerRegistry::new();

        let handler = Arc::new(TestHandler {
            agent_id: "agent-1".to_string(),
            types: vec![],
        });

        registry.register(handler);
        assert_eq!(registry.handler_count(), 1);

        assert!(registry.unregister("agent-1"));
        assert_eq!(registry.handler_count(), 0);
    }

    #[test]
    fn test_handler_filtering() {
        let handler = TestHandler {
            agent_id: "agent-1".to_string(),
            types: vec![MessageType::ConsensusVote],
        };

        let vote_msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::ConsensusVote {
                round: 1,
                decision: crate::agent::multi::shared::VoteDecision::Approve,
                rationale: "vote".into(),
            },
        );

        let task_msg = AgentMessage::new(
            "sender",
            "agent-1",
            MessagePayload::TaskAssignment {
                task: crate::agent::multi::traits::AgentTask::new("task-1", "test task"),
            },
        );

        let other_msg = AgentMessage::new(
            "sender",
            "agent-2",
            MessagePayload::ConsensusVote {
                round: 1,
                decision: crate::agent::multi::shared::VoteDecision::Approve,
                rationale: "vote".into(),
            },
        );

        assert!(handler.should_handle(&vote_msg));
        assert!(!handler.should_handle(&task_msg));
        assert!(!handler.should_handle(&other_msg));
    }

    #[tokio::test]
    async fn test_registry_dispatch() {
        let registry = MessageHandlerRegistry::new();

        registry.register(Arc::new(TestHandler {
            agent_id: "agent-1".to_string(),
            types: vec![],
        }));

        registry.register(Arc::new(TestHandler {
            agent_id: "agent-2".to_string(),
            types: vec![],
        }));

        let broadcast = AgentMessage::broadcast(
            "coordinator",
            MessagePayload::Text {
                content: "hello".into(),
            },
        );

        let results = registry.dispatch(&broadcast).await;
        assert_eq!(results.len(), 2);
    }
}
