//! Skill system for task methodology definition.
//!
//! Skills represent HOW to perform a task. Unlike rules (WHAT knowledge),
//! skills are explicitly invoked and define the methodology for specific operations.
//!
//! The v3.0 architecture defines 5 fixed skills:
//!
//! | Skill       | Purpose                              |
//! |-------------|--------------------------------------|
//! | code-review | Systematic code quality review       |
//! | implement   | Feature implementation methodology   |
//! | plan        | Design and planning methodology      |
//! | debug       | Issue investigation methodology      |
//! | refactor    | Code improvement methodology         |

mod composer;
mod registry;
mod types;

pub use composer::SkillComposer;
pub use registry::SkillRegistry;
pub use types::{Skill, SkillType};
