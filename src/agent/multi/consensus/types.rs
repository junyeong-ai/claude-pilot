//! Type definitions for the consensus engine.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::super::shared::{AgentId, TaskComplexity};
use super::super::traits::{AgentRole, TaskPriority};
use super::conflict::Conflict;

use crate::config::ConsensusScoringConfig;

/// Historical performance metrics for an agent.
#[derive(Debug, Clone, Default)]
pub struct AgentPerformanceHistory {
    pub total_proposals: u64,
    pub accepted_proposals: u64,
    pub module_contributions: HashMap<String, u64>,
}

impl AgentPerformanceHistory {
    pub fn accuracy_rate(&self) -> f64 {
        if self.total_proposals == 0 {
            ConsensusScoringConfig::default().default_accuracy
        } else {
            self.accepted_proposals as f64 / self.total_proposals as f64
        }
    }

    pub fn module_strength(&self, module: &str) -> f64 {
        let total: u64 = self.module_contributions.values().sum();
        if total == 0 {
            return ConsensusScoringConfig::default().default_accuracy;
        }
        self.module_contributions.get(module).copied().unwrap_or(0) as f64 / total as f64
    }
}

/// Combined evidence quality assessment result.
#[derive(Debug, Clone, Copy)]
pub struct EvidenceQuality {
    pub strength: f64,
    pub confidence: f64,
}

impl EvidenceQuality {
    pub fn weighted(&self) -> f64 {
        self.strength * 0.6 + self.confidence * 0.4
    }
}

/// Score for an agent's proposal with evidence weighting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProposalScore {
    pub agent_id: String,
    pub module_relevance: f64,
    pub historical_accuracy: f64,
    pub evidence_strength: f64,
    pub confidence: f64,
}

impl AgentProposalScore {
    pub const MODULE_WEIGHT: f64 = 0.35;
    pub const HISTORY_WEIGHT: f64 = 0.25;
    pub const EVIDENCE_WEIGHT: f64 = 0.25;
    pub const CONFIDENCE_WEIGHT: f64 = 0.15;

    pub fn weighted_score(&self) -> f64 {
        self.module_relevance * Self::MODULE_WEIGHT
            + self.historical_accuracy * Self::HISTORY_WEIGHT
            + self.evidence_strength * Self::EVIDENCE_WEIGHT
            + self.confidence * Self::CONFIDENCE_WEIGHT
    }
}

/// A scored proposal from an agent.
#[derive(Debug, Clone)]
pub struct ScoredProposal {
    pub proposal: AgentProposal,
    pub score: AgentProposalScore,
}

/// A consensus round with proposals and synthesis.
#[derive(Debug, Clone)]
pub struct ScoredProposalRound {
    pub number: usize,
    pub proposals: Vec<ScoredProposal>,
    pub synthesis: String,
    pub converged: bool,
    pub conflicts: Vec<Conflict>,
    pub agent_requests: Vec<AgentRequest>,
}

/// Basic proposal from an agent.
#[derive(Debug, Clone)]
pub struct AgentProposal {
    pub agent_id: String,
    pub role: AgentRole,
    pub content: String,
}

/// Enhanced proposal with semantic information for cross-visibility consensus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedProposal {
    pub agent_id: String,
    pub module_id: String,
    pub summary: String,
    pub approach: String,
    pub affected_files: Vec<String>,
    pub api_changes: Vec<ProposedApiChange>,
    pub type_changes: Vec<ProposedTypeChange>,
    pub dependencies_used: Vec<String>,
    pub dependencies_provided: Vec<String>,
}

impl EnhancedProposal {
    pub fn new(agent_id: impl Into<String>, module_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            module_id: module_id.into(),
            summary: String::new(),
            approach: String::new(),
            affected_files: Vec::new(),
            api_changes: Vec::new(),
            type_changes: Vec::new(),
            dependencies_used: Vec::new(),
            dependencies_provided: Vec::new(),
        }
    }

    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = summary.into();
        self
    }

    pub fn with_approach(mut self, approach: impl Into<String>) -> Self {
        self.approach = approach.into();
        self
    }

    pub fn brief_summary(&self) -> &str {
        self.summary.lines().next().unwrap_or(&self.summary)
    }

    /// Convert to SemanticProposal for convergence checking.
    pub fn to_semantic_proposal(&self) -> super::super::convergence::SemanticProposal {
        use super::super::convergence::SemanticProposal;
        use super::super::shared::{ApiChange, ApiChangeType, TypeChange, TypeChangeType};

        // Convert ProposedApiChange to ApiChange
        let api_changes: Vec<ApiChange> = self
            .api_changes
            .iter()
            .map(|p| {
                let change_type = match p.change_type.to_lowercase().as_str() {
                    "add" => ApiChangeType::Add,
                    "remove" => ApiChangeType::Remove,
                    "rename" => ApiChangeType::Rename {
                        old_name: p.old_name.clone().unwrap_or_else(|| p.name.clone()),
                    },
                    _ => ApiChangeType::ModifySignature,
                };
                ApiChange {
                    api_name: p.name.clone(),
                    change_type,
                    old_signature: None,
                    new_signature: p.signature.clone(),
                    module: None,
                }
            })
            .collect();

        // Convert ProposedTypeChange to TypeChange
        let type_changes: Vec<TypeChange> = self
            .type_changes
            .iter()
            .map(|p| {
                let change_type = match p.change_type.to_lowercase().as_str() {
                    "add" | "addfield" | "add_field" => TypeChangeType::AddField,
                    "remove" | "removefield" | "remove_field" => TypeChangeType::RemoveField,
                    "changetype" | "change_type" => TypeChangeType::ChangeFieldType,
                    "addvariant" | "add_variant" => TypeChangeType::AddVariant,
                    "removevariant" | "remove_variant" => TypeChangeType::RemoveVariant,
                    _ => TypeChangeType::ChangeFieldType,
                };
                TypeChange {
                    type_name: p.type_name.clone(),
                    change_type,
                    details: p.details.clone(),
                    module: None,
                }
            })
            .collect();

        SemanticProposal::new(&self.agent_id, &self.module_id)
            .with_summary(&self.summary)
            .with_api_changes(api_changes)
            .with_type_changes(type_changes)
            .with_dependencies(
                self.dependencies_used.clone(),
                self.dependencies_provided.clone(),
            )
    }
}

/// Proposed API change in an enhanced proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedApiChange {
    /// The current (new) name of the API.
    pub name: String,
    /// Type of change: "add", "remove", "rename", "modify".
    pub change_type: String,
    /// For rename operations, the original name before renaming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_name: Option<String>,
    /// Optional signature information.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Proposed type change in an enhanced proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedTypeChange {
    pub type_name: String,
    pub change_type: String,
    pub details: String,
}

/// Result of consensus process.
pub enum ConsensusResult {
    Agreed {
        plan: String,
        tasks: Vec<ConsensusTask>,
        rounds: usize,
        respondent_count: usize,
    },
    PartialAgreement {
        plan: String,
        tasks: Vec<ConsensusTask>,
        dissents: Vec<String>,
        unresolved_conflicts: Vec<Conflict>,
        respondent_count: usize,
    },
    NoConsensus {
        summary: String,
        blocking_conflicts: Vec<Conflict>,
        respondent_count: usize,
    },
}

/// Request for additional agent during consensus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRequest {
    pub module: String,
    pub reason: String,
}

/// Task from consensus with structured data.
///
/// Uses qualified module format for cross-workspace support:
/// - Simple: "module_name" (workspace-local)
/// - Full: "workspace::module" or "workspace::domain::module"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusTask {
    pub id: String,
    pub description: String,
    /// Module assignment (supports qualified format: workspace::domain::module).
    #[serde(default)]
    pub assigned_module: Option<String>,
    pub dependencies: Vec<String>,
    pub priority: TaskPriority,
    #[serde(default)]
    pub estimated_complexity: TaskComplexity,
    #[serde(default)]
    pub files_affected: Vec<String>,
    #[serde(default)]
    pub priority_score: f64,
}

impl ConsensusTask {
    /// Create a new task with required fields.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            assigned_module: None,
            dependencies: Vec::new(),
            priority: TaskPriority::Normal,
            estimated_complexity: TaskComplexity::default(),
            files_affected: Vec::new(),
            priority_score: 0.0,
        }
    }

    /// Assign to a module.
    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.assigned_module = Some(module.into());
        self
    }

    /// Assign to a qualified module (workspace::module).
    pub fn with_qualified_module(mut self, workspace: &str, module: &str) -> Self {
        self.assigned_module = Some(format!("{}::{}", workspace, module));
        self
    }

    /// Assign with full qualification (workspace::domain::module).
    pub fn with_full_qualification(mut self, workspace: &str, domain: &str, module: &str) -> Self {
        self.assigned_module = Some(format!("{}::{}::{}", workspace, domain, module));
        self
    }

    /// Set dependencies.
    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    /// Set priority.
    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Set complexity.
    pub fn with_complexity(mut self, complexity: TaskComplexity) -> Self {
        self.estimated_complexity = complexity;
        self
    }

    /// Set affected files.
    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_affected = files;
        self
    }

    /// Parse the assigned module as qualified module.
    pub fn parse_qualified(&self) -> Option<super::super::shared::QualifiedModule> {
        self.assigned_module
            .as_ref()
            .and_then(|m| m.parse().ok())
    }

    /// Get the workspace from the assignment (if qualified).
    ///
    /// Returns `None` if the module is not qualified (no `::` separator).
    pub fn workspace(&self) -> Option<&str> {
        self.assigned_module
            .as_ref()
            .filter(|m| m.contains("::"))
            .and_then(|m| m.split("::").next())
    }

    /// Get the simple module name (last component).
    pub fn module_name(&self) -> Option<&str> {
        self.assigned_module
            .as_ref()
            .map(|m| m.rsplit("::").next().unwrap_or(m))
    }

    /// Check if this task is assigned to a specific workspace.
    pub fn is_in_workspace(&self, workspace_id: &str) -> bool {
        self.assigned_module
            .as_ref()
            .is_some_and(|m| m.starts_with(&format!("{}::", workspace_id)))
    }

    /// Check if this task uses qualified module format.
    pub fn is_qualified(&self) -> bool {
        self.assigned_module
            .as_ref()
            .is_some_and(|m| m.contains("::"))
    }
}

/// JSON output structure for consensus synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusSynthesisOutput {
    /// Status string from LLM: "agreed", "partial_agreement", or "no_agreement".
    pub consensus: String,
    pub plan: String,
    #[serde(default)]
    pub tasks: Vec<ConsensusTask>,
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
    #[serde(default)]
    pub dissents: Vec<String>,
    #[serde(default)]
    pub agent_needed: Vec<AgentRequest>,
}

impl ConsensusSynthesisOutput {
    pub fn is_agreed(&self) -> bool {
        self.consensus == "agreed"
    }

    pub fn is_partial_agreement(&self) -> bool {
        self.consensus == "partial_agreement"
    }
}

// --- Internal types (visible within the consensus module only) ---

pub(super) struct ConsensusSession {
    pub(super) participants: Vec<AgentId>,
    pub(super) rounds: Vec<ScoredProposalRound>,
    pub(super) previous_synthesis: String,
}

impl ConsensusSession {
    pub(super) fn new(initial_participants: &[AgentId]) -> Self {
        Self {
            participants: initial_participants.to_vec(),
            rounds: Vec::new(),
            previous_synthesis: String::new(),
        }
    }

    pub(super) fn add_participant(&mut self, id: AgentId) {
        if !self.participants.contains(&id) {
            self.participants.push(id);
        }
    }
}

pub(super) enum RoundOutcomeResult {
    Converged(ConsensusResult),
    Blocked(ConsensusResult),
    Continue,
}

/// Context for consensus round execution.
/// Groups parameters that remain constant across rounds.
pub(super) struct RoundExecutionContext<'a> {
    pub(super) topic: &'a str,
    pub(super) task_context: &'a super::super::traits::TaskContext,
    pub(super) pool: &'a super::super::pool::AgentPool,
    pub(super) timeout_deadline: std::time::Instant,
}
