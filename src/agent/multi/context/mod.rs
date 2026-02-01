//! Context system for agent execution.
//!
//! Integrates Rules (WHAT), Skills (HOW), Personas (WHO), and Modules to compose
//! unified execution contexts for agents.
//!
//! # Layered Context Architecture
//!
//! ```text
//! Layer 3: Skills (HOW) - Optional task methodology
//! Layer 2: Rules (WHAT) - Optional path-based domain knowledge
//! Layer 1: modmap Module (WHAT) - Primary module context
//! Layer 0: Base Agent (WHO) - Required core capabilities
//! ```
//!
//! # Components
//!
//! - `ContextComposer`: Composes contexts from personas, skills, and rules
//! - `ModuleContextBuilder`: Builds contexts from modmap::Module with optional enhancements

mod composer;
mod module_context;
mod persona;

pub use composer::{ComposedContext, ContextComposer};
pub use module_context::{ModuleContext, ModuleContextBuilder};
pub use persona::{AgentPersona, ConsensusRole, PersonaLoader};
