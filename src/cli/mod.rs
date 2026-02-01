//! Command-line interface definitions.
//!
//! Provides CLI structure and output formatting:
//! - `Cli`, `Commands`: CLI argument definitions via clap
//! - `Display`: Formatted terminal output with colors and status
//! - `InteractiveHandler`: Human-in-the-loop escalation handling

mod commands;
mod display;
mod interactive;

pub use commands::{
    Cli, Commands, ConfigAction, OnCompleteArg, OutputFormat, PriorityArg, StatusFilterArg,
};
pub use display::Display;
pub use interactive::{InteractiveHandler, NonInteractiveHandler, NonInteractivePolicy};
