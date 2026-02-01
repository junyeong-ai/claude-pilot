//! Shared utility functions.
//!
//! Common utilities used across the codebase:
//! - String truncation (UTF-8 safe, boundary-aware)
//! - Token estimation for context budgeting
//! - File operations (line counting)
//! - Percentage formatting

mod file_ops;
mod format;
mod string;
mod tokenizer;

pub use file_ops::{count_lines_in_file, load_gitignore};
pub use format::ratio_to_percent_u8;
pub use string::{truncate_at_boundary, truncate_chars, truncate_str, truncate_with_marker};
pub use tokenizer::estimate_tokens;
