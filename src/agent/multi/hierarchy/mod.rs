//! Agent hierarchy and consensus strategy based on manifest structure.
//!
//! Unified module combining:
//! - Tier levels (Module -> Group -> Domain -> Workspace)
//! - Participant selection and tracking
//! - Strategy selection (Direct, Flat, Hierarchical)
//! - Task routing

mod agent_hierarchy;
mod aggregation;
mod participants;
mod strategy;
#[cfg(test)]
mod tests;
pub(crate) mod types;

// Re-export types from types.rs (effectively pub(crate) due to pub(crate) mod types)
// ConsensusStrategy, ConsensusTier, ConsensusUnit, EdgeCaseResolution are internal
// HierarchyConfig, ParticipantSet are also used only within the crate
pub use types::{
    ConsensusStrategy, ConsensusTier, ConsensusUnit, EdgeCaseResolution,
    HierarchyConfig, ParticipantSet,
};

// Re-export structs from submodules
pub use agent_hierarchy::AgentHierarchy;
pub use aggregation::{HierarchicalAggregator, TierAggregation};
pub use participants::{MultiWorkspaceParticipantSelector, ParticipantSelector};
pub use strategy::StrategySelector;
