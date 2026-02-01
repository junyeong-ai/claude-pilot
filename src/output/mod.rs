//! Mission output and artifact writing.
//!
//! Handles structured output generation:
//! - `OutputWriter`: Writes mission results to files
//! - `MissionOutput`: Structured output format for completed missions

mod writer;

pub use writer::{MissionOutput, OutputWriter};
