//! Mission event notification system.
//!
//! Provides notifications for mission lifecycle events:
//! - `MissionEvent`: Event types (started, completed, failed, etc.)
//! - `Notifier`: Cross-platform notification delivery

mod events;
mod notifier;

pub use events::{EventType, MissionEvent};
pub use notifier::Notifier;
