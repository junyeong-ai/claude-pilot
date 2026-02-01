//! Claude Code session integration.
//!
//! Bridges claude-pilot with Claude Code CLI sessions:
//! - `SessionBridge`: Connects to existing Claude Code sessions
//! - `SessionConfig`: Session directory configuration

mod bridge;
mod config;

pub use bridge::SessionBridge;
pub use config::SessionConfig;
