//! Context management module.
//!
//! This module was planned for 3-Zone hierarchical context management,
//! but was removed as over-engineering for modern LLMs that don't
//! significantly suffer from "lost in the middle" issues.
//!
//! Token budget management is handled directly by PromptBuilder
//! with natural information ordering.

// Module intentionally empty - context management delegated to PromptBuilder
