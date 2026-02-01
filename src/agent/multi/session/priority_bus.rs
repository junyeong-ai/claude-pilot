//! Priority-based message bus for agent communication.
//!
//! Provides a message bus with priority ordering guarantees where
//! high-priority messages are delivered before low-priority ones.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

/// Priority levels for messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Priority {
    /// Critical messages (conflicts, errors).
    Critical = 100,
    /// High priority (phase changes, important notifications).
    High = 75,
    #[default]
    /// Normal priority (task updates).
    Normal = 50,
    /// Low priority (progress updates).
    Low = 25,
    /// Background (metrics, logs).
    Background = 0,
}

impl Priority {
    pub fn value(&self) -> u32 {
        *self as u32
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value().cmp(&other.value())
    }
}

/// A message in the priority bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityMessage {
    /// Unique message ID.
    pub id: u64,
    /// Message priority.
    pub priority: Priority,
    /// Source agent.
    pub source: String,
    /// Target ("*" for broadcast, or specific agent ID).
    pub target: String,
    /// Message type.
    pub message_type: MessageType,
    /// Message payload.
    pub payload: String,
    /// Sequence number for ordering within same priority.
    #[serde(skip)]
    pub sequence: u64,
    /// Timestamp.
    #[serde(skip)]
    pub timestamp: Option<Instant>,
}

impl PriorityMessage {
    pub fn new(source: &str, target: &str, message_type: MessageType, payload: String) -> Self {
        Self {
            id: 0,
            priority: Priority::Normal,
            source: source.to_string(),
            target: target.to_string(),
            message_type,
            payload,
            sequence: 0,
            timestamp: Some(Instant::now()),
        }
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn broadcast(source: &str, message_type: MessageType, payload: String) -> Self {
        Self::new(source, "*", message_type, payload)
    }

    pub fn is_broadcast(&self) -> bool {
        self.target == "*"
    }

    pub fn is_for(&self, agent_id: &str) -> bool {
        self.target == "*" || self.target == agent_id
    }
}

/// Wrapper for heap ordering (max heap by priority, then by sequence).
#[derive(Debug)]
struct HeapEntry {
    message: PriorityMessage,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.message.priority == other.message.priority
            && self.message.sequence == other.message.sequence
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, then lower sequence (earlier message)
        match self.message.priority.cmp(&other.message.priority) {
            Ordering::Equal => other.message.sequence.cmp(&self.message.sequence),
            other => other,
        }
    }
}

/// Message types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    /// Task assignment.
    TaskAssignment,
    /// Task completion.
    TaskCompletion,
    /// Task failure.
    TaskFailure,
    /// Conflict alert.
    Conflict,
    /// Progress update.
    Progress,
    /// Coordination request.
    Coordination,
    /// Phase change.
    PhaseChange,
    /// Lock notification.
    Lock,
    /// Generic text.
    Text,
    /// Custom type.
    Custom(String),
}

impl MessageType {
    pub fn default_priority(&self) -> Priority {
        match self {
            Self::Conflict => Priority::Critical,
            Self::PhaseChange => Priority::High,
            Self::TaskAssignment | Self::TaskCompletion | Self::TaskFailure => Priority::Normal,
            Self::Coordination | Self::Lock => Priority::Normal,
            Self::Progress => Priority::Low,
            Self::Text | Self::Custom(_) => Priority::Normal,
        }
    }
}

/// Subscriber handle for receiving messages.
pub struct Subscriber {
    agent_id: String,
    queue: Arc<Mutex<BinaryHeap<HeapEntry>>>,
}

impl Subscriber {
    /// Try to receive the next message (non-blocking).
    pub fn try_recv(&self) -> Option<PriorityMessage> {
        let mut queue = self.queue.lock();
        queue.pop().map(|e| e.message)
    }

    /// Receive all pending messages in priority order.
    pub fn recv_all(&self) -> Vec<PriorityMessage> {
        let mut queue = self.queue.lock();
        let mut messages = Vec::with_capacity(queue.len());
        while let Some(entry) = queue.pop() {
            messages.push(entry.message);
        }
        messages
    }

    /// Get pending message count.
    pub fn pending_count(&self) -> usize {
        self.queue.lock().len()
    }

    /// Check if there are pending messages.
    pub fn has_pending(&self) -> bool {
        !self.queue.lock().is_empty()
    }

    /// Get agent ID.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
}

/// Priority message bus.
#[derive(Debug)]
pub struct PriorityMessageBus {
    subscribers: RwLock<HashMap<String, Arc<Mutex<BinaryHeap<HeapEntry>>>>>,
    next_id: AtomicU64,
    next_sequence: AtomicU64,
    message_history: Mutex<Vec<PriorityMessage>>,
    history_limit: usize,
}

impl Default for PriorityMessageBus {
    fn default() -> Self {
        Self::new()
    }
}

impl PriorityMessageBus {
    pub fn new() -> Self {
        Self {
            subscribers: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            next_sequence: AtomicU64::new(1),
            message_history: Mutex::new(Vec::new()),
            history_limit: 1000,
        }
    }

    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit;
        self
    }

    /// Subscribe to messages.
    pub fn subscribe(&self, agent_id: &str) -> Subscriber {
        let queue = Arc::new(Mutex::new(BinaryHeap::new()));
        self.subscribers
            .write()
            .insert(agent_id.to_string(), Arc::clone(&queue));

        Subscriber {
            agent_id: agent_id.to_string(),
            queue,
        }
    }

    /// Unsubscribe from messages.
    pub fn unsubscribe(&self, agent_id: &str) {
        self.subscribers.write().remove(agent_id);
    }

    /// Send a message.
    pub fn send(&self, mut message: PriorityMessage) {
        message.id = self.next_id.fetch_add(1, AtomicOrdering::SeqCst);
        message.sequence = self.next_sequence.fetch_add(1, AtomicOrdering::SeqCst);

        // Store in history
        {
            let mut history = self.message_history.lock();
            history.push(message.clone());
            if history.len() > self.history_limit {
                history.remove(0);
            }
        }

        // Deliver to subscribers
        let subscribers = self.subscribers.read();

        if message.is_broadcast() {
            for queue in subscribers.values() {
                queue.lock().push(HeapEntry {
                    message: message.clone(),
                });
            }
        } else if let Some(queue) = subscribers.get(&message.target) {
            queue.lock().push(HeapEntry { message });
        }
    }

    /// Send with automatic priority based on message type.
    pub fn send_auto_priority(&self, mut message: PriorityMessage) {
        message.priority = message.message_type.default_priority();
        self.send(message);
    }

    /// Send a critical message.
    pub fn send_critical(
        &self,
        source: &str,
        target: &str,
        message_type: MessageType,
        payload: String,
    ) {
        self.send(
            PriorityMessage::new(source, target, message_type, payload)
                .with_priority(Priority::Critical),
        );
    }

    /// Broadcast a message to all subscribers.
    pub fn broadcast(&self, source: &str, message_type: MessageType, payload: String) {
        self.send(PriorityMessage::broadcast(source, message_type, payload));
    }

    /// Broadcast with priority.
    pub fn broadcast_with_priority(
        &self,
        source: &str,
        message_type: MessageType,
        payload: String,
        priority: Priority,
    ) {
        self.send(
            PriorityMessage::broadcast(source, message_type, payload).with_priority(priority),
        );
    }

    /// Get subscriber count.
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.read().len()
    }

    /// Get message history.
    pub fn history(&self, limit: usize) -> Vec<PriorityMessage> {
        let history = self.message_history.lock();
        history.iter().rev().take(limit).cloned().collect()
    }

    /// Get history for a specific agent.
    pub fn history_for(&self, agent_id: &str, limit: usize) -> Vec<PriorityMessage> {
        let history = self.message_history.lock();
        history
            .iter()
            .rev()
            .filter(|m| m.is_for(agent_id))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Clear all subscriber queues.
    pub fn clear_queues(&self) {
        let subscribers = self.subscribers.read();
        for queue in subscribers.values() {
            queue.lock().clear();
        }
    }

    /// Get statistics.
    pub fn stats(&self) -> BusStats {
        let subscribers = self.subscribers.read();
        let total_pending: usize = subscribers.values().map(|q| q.lock().len()).sum();
        let history_len = self.message_history.lock().len();

        BusStats {
            subscriber_count: subscribers.len(),
            total_pending_messages: total_pending,
            total_messages_sent: self.next_id.load(AtomicOrdering::SeqCst) - 1,
            history_size: history_len,
        }
    }
}

/// Bus statistics.
#[derive(Debug, Clone)]
pub struct BusStats {
    pub subscriber_count: usize,
    pub total_pending_messages: usize,
    pub total_messages_sent: u64,
    pub history_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_ordering() {
        let bus = PriorityMessageBus::new();
        let sub = bus.subscribe("agent-1");

        // Send messages in reverse priority order
        bus.send(
            PriorityMessage::new(
                "coordinator",
                "agent-1",
                MessageType::Progress,
                "progress".into(),
            )
            .with_priority(Priority::Low),
        );
        bus.send(
            PriorityMessage::new(
                "coordinator",
                "agent-1",
                MessageType::Conflict,
                "conflict".into(),
            )
            .with_priority(Priority::Critical),
        );
        bus.send(
            PriorityMessage::new("coordinator", "agent-1", MessageType::Text, "normal".into())
                .with_priority(Priority::Normal),
        );

        // Should receive in priority order (Critical, Normal, Low)
        let m1 = sub.try_recv().unwrap();
        assert_eq!(m1.priority, Priority::Critical);

        let m2 = sub.try_recv().unwrap();
        assert_eq!(m2.priority, Priority::Normal);

        let m3 = sub.try_recv().unwrap();
        assert_eq!(m3.priority, Priority::Low);

        assert!(sub.try_recv().is_none());
    }

    #[test]
    fn test_broadcast() {
        let bus = PriorityMessageBus::new();
        let sub1 = bus.subscribe("agent-1");
        let sub2 = bus.subscribe("agent-2");
        let sub3 = bus.subscribe("agent-3");

        bus.broadcast("coordinator", MessageType::PhaseChange, "planning".into());

        assert_eq!(sub1.pending_count(), 1);
        assert_eq!(sub2.pending_count(), 1);
        assert_eq!(sub3.pending_count(), 1);
    }

    #[test]
    fn test_targeted_message() {
        let bus = PriorityMessageBus::new();
        let sub1 = bus.subscribe("agent-1");
        let sub2 = bus.subscribe("agent-2");

        bus.send(PriorityMessage::new(
            "coordinator",
            "agent-1",
            MessageType::TaskAssignment,
            "task-1".into(),
        ));

        assert_eq!(sub1.pending_count(), 1);
        assert_eq!(sub2.pending_count(), 0);
    }

    #[test]
    fn test_same_priority_fifo() {
        let bus = PriorityMessageBus::new();
        let sub = bus.subscribe("agent-1");

        // Send multiple messages with same priority
        for i in 1..=5 {
            bus.send(
                PriorityMessage::new(
                    "coordinator",
                    "agent-1",
                    MessageType::Text,
                    format!("msg-{}", i),
                )
                .with_priority(Priority::Normal),
            );
        }

        // Should receive in FIFO order within same priority
        let messages = sub.recv_all();
        assert_eq!(messages.len(), 5);
        assert!(messages[0].payload.contains("msg-1"));
        assert!(messages[4].payload.contains("msg-5"));
    }

    #[test]
    fn test_auto_priority() {
        let bus = PriorityMessageBus::new();
        let sub = bus.subscribe("agent-1");

        bus.send_auto_priority(PriorityMessage::new(
            "coordinator",
            "agent-1",
            MessageType::Conflict,
            "conflict".into(),
        ));

        let msg = sub.try_recv().unwrap();
        assert_eq!(msg.priority, Priority::Critical);
    }

    #[test]
    fn test_history() {
        let bus = PriorityMessageBus::new();
        let _sub = bus.subscribe("agent-1");

        for i in 1..=10 {
            bus.send(PriorityMessage::new(
                "coordinator",
                "agent-1",
                MessageType::Text,
                format!("msg-{}", i),
            ));
        }

        let history = bus.history(5);
        assert_eq!(history.len(), 5);
        // Most recent first
        assert!(history[0].payload.contains("msg-10"));
    }

    #[test]
    fn test_stats() {
        let bus = PriorityMessageBus::new();
        let _sub1 = bus.subscribe("agent-1");
        let _sub2 = bus.subscribe("agent-2");

        bus.broadcast("coordinator", MessageType::Text, "test".into());
        bus.send(PriorityMessage::new(
            "coordinator",
            "agent-1",
            MessageType::Text,
            "direct".into(),
        ));

        let stats = bus.stats();
        assert_eq!(stats.subscriber_count, 2);
        assert_eq!(stats.total_messages_sent, 2);
    }
}
