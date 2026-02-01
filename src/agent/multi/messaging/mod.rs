//! Inter-agent messaging system.
//!
//! Provides typed message passing between agents with event sourcing integration.
//!
//! ```text
//! ┌─────────────┐         ┌─────────────────┐         ┌─────────────┐
//! │   Agent A   │──send──▶│ AgentMessageBus │──recv──▶│   Agent B   │
//! └─────────────┘         │   (broadcast)   │         └─────────────┘
//!                         └────────┬────────┘
//!                                  │
//!                                  ▼
//!                         ┌─────────────────┐
//!                         │   EventStore    │
//!                         │ (audit trail)   │
//!                         └─────────────────┘
//! ```

mod bus;
mod handler;
mod message;

pub use bus::{AgentMessageBus, AgentReceiver, FilteredReceiver};
pub use handler::{MessageHandler, MessageHandlerRegistry};
pub use message::{AgentMessage, MessagePayload, MessageType};
