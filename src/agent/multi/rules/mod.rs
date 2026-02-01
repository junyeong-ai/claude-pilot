//! Rule system for context-aware knowledge injection.
//!
//! Rules represent domain knowledge (WHAT) that gets auto-injected based on context.
//! They follow a 6-category priority system:
//!
//! | Priority | Category  | Injection Trigger |
//! |----------|-----------|-------------------|
//! | 100      | Project   | Always            |
//! | 90       | Tech      | File extension    |
//! | 85       | Framework | Path + keywords   |
//! | 80       | Module    | Module path       |
//! | 70       | Group     | Group membership  |
//! | 60       | Domain    | Content keywords  |

mod registry;
mod resolver;
mod types;

pub use registry::RuleRegistry;
pub use resolver::RuleResolver;
pub use types::{InjectionConfig, ResolvedRules, Rule, RuleCategory, RuleMetadata};
