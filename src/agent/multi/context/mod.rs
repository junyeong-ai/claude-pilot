//! Context system for agent execution.
//!
//! Provides module-based context building from modmap::Module + manifest data.
//!
//! # Layered Context Architecture
//!
//! ```text
//! Layer 1: modmap Module (WHAT) - Primary module context
//! Layer 0: Base Agent (WHO) - Required core capabilities
//! ```
//!
//! # Components
//!
//! - `ModuleContextBuilder`: Builds contexts from modmap::Module with optional manifest enhancements

mod module_context;

pub use module_context::{ModuleContext, ModuleContextBuilder};
