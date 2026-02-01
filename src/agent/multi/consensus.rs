//! Consensus engine for multi-agent planning through evidence-weighted voting.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

use super::escalation::EscalationEngine;
use super::messaging::{AgentMessage, AgentMessageBus, MessagePayload};
use super::pool::AgentPool;
use super::shared::{AgentId, RoundOutcome, VoteDecision};
use super::traits::{
    AgentRole, AgentTask, AgentTaskResult, TaskContext, TaskPriority, extract_field,
};
use crate::agent::TaskAgent;
use crate::config::ConsensusConfig;
use crate::error::{PilotError, Result};

pub use super::shared::{ConflictSeverity, TaskComplexity};
use crate::state::{ConsensusProjection, DomainEvent, EventStore};

mod scoring {
    pub const DEFAULT_ACCURACY: f64 = 0.5;
    pub const APPROVE_THRESHOLD: f64 = 0.7;
    pub const APPROVE_WITH_CHANGES_THRESHOLD: f64 = 0.4;
    pub const MODULE_MATCH_SCORE: f64 = 0.9;
    pub const MODULE_PARTIAL_SCORE: f64 = 0.6;
    pub const CORE_ROLE_SCORE: f64 = 0.7;
    pub const DEFAULT_ROLE_SCORE: f64 = 0.5;
    pub const CONVERGENCE_THRESHOLD: f64 = 0.6;
    pub const PARTIAL_CONVERGENCE_THRESHOLD: f64 = 0.5;
    pub const BASE_EVIDENCE_SCORE: f64 = 0.5;
    pub const FILE_REFERENCE_BONUS: f64 = 0.1;
    pub const CODE_SNIPPET_BONUS: f64 = 0.15;
    pub const LINE_REFERENCE_BONUS: f64 = 0.1;
    pub const ERROR_CODE_BONUS: f64 = 0.1;
    pub const IDENTIFIER_PATTERN_BONUS: f64 = 0.05;
    pub const TOPIC_MATCH_BONUS: f64 = 0.15;
    pub const IMPLEMENTATION_DETAIL_BONUS: f64 = 0.1;
    pub const CONSISTENCY_BONUS: f64 = 0.15;
    pub const REFERENCE_BONUS: f64 = 0.05;
    pub const MIN_EVIDENCE_SCORE: f64 = 0.1;
}

mod limits {
    pub const MAX_AGENT_HISTORY_ENTRIES: usize = 1000;
    pub const HISTORY_PRUNE_THRESHOLD: usize = 1200;
    pub const CONSENSUS_TIMEOUT_SECS: u64 = 300;
}

struct ConsensusSession {
    participants: Vec<AgentId>,
    rounds: Vec<ConsensusRound>,
    previous_synthesis: String,
}

impl ConsensusSession {
    fn new(initial_participants: &[AgentId]) -> Self {
        Self {
            participants: initial_participants.to_vec(),
            rounds: Vec::new(),
            previous_synthesis: String::new(),
        }
    }

    fn add_participant(&mut self, id: AgentId) {
        if !self.participants.contains(&id) {
            self.participants.push(id);
        }
    }
}

enum RoundOutcomeResult {
    Converged(ConsensusResult),
    Blocked(ConsensusResult),
    Continue,
}

/// Context for consensus round execution.
/// Groups parameters that remain constant across rounds.
struct RoundExecutionContext<'a> {
    topic: &'a str,
    task_context: &'a TaskContext,
    pool: &'a AgentPool,
    timeout_deadline: std::time::Instant,
}

/// Consensus engine with evidence-weighted voting and conflict resolution.
pub struct ConsensusEngine {
    aggregator: Arc<TaskAgent>,
    config: ConsensusConfig,
    agent_history: RwLock<HashMap<String, AgentPerformanceHistory>>,
    event_store: Option<Arc<EventStore>>,
    escalation_engine: Option<EscalationEngine>,
    message_bus: Option<Arc<AgentMessageBus>>,
}

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
            scoring::DEFAULT_ACCURACY
        } else {
            self.accepted_proposals as f64 / self.total_proposals as f64
        }
    }

    pub fn module_strength(&self, module: &str) -> f64 {
        let total: u64 = self.module_contributions.values().sum();
        if total == 0 {
            return scoring::DEFAULT_ACCURACY;
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
pub struct ProposalScore {
    pub agent_id: String,
    pub module_relevance: f64,
    pub historical_accuracy: f64,
    pub evidence_strength: f64,
    pub confidence: f64,
}

impl ProposalScore {
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
    pub score: ProposalScore,
}

/// A consensus round with proposals and synthesis.
#[derive(Debug, Clone)]
pub struct ConsensusRound {
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
    pub fn to_semantic_proposal(&self) -> super::convergence::SemanticProposal {
        use super::convergence::SemanticProposal;
        use super::shared::{ApiChange, ApiChangeType, TypeChange, TypeChangeType};

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

/// Conflict between agent proposals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub agents: Vec<String>,
    pub topic: String,
    pub positions: Vec<String>,
    pub severity: ConflictSeverity,
    pub resolution: Option<ConflictResolution>,
}

impl Conflict {
    /// Create a conflict from structured parameters.
    pub fn new(
        id: impl Into<String>,
        topic: impl Into<String>,
        severity: ConflictSeverity,
    ) -> Self {
        Self {
            id: id.into(),
            agents: Vec::new(),
            topic: topic.into(),
            positions: Vec::new(),
            severity,
            resolution: None,
        }
    }

    pub fn with_agents(mut self, agents: Vec<String>) -> Self {
        self.agents = agents;
        self
    }

    pub fn with_positions(mut self, positions: Vec<String>) -> Self {
        self.positions = positions;
        self
    }

    /// Parse a conflict description string into a structured Conflict.
    ///
    /// Handles common patterns:
    /// - "API conflict: func_name between module_a, module_b"
    /// - "Type mismatch in TypeName"
    /// - Generic conflict descriptions
    pub fn from_description(id: impl Into<String>, description: &str) -> Self {
        let id = id.into();
        let lower = description.to_lowercase();

        // Detect severity from keywords
        let severity = if lower.contains("critical")
            || lower.contains("breaking")
            || lower.contains("blocking")
        {
            ConflictSeverity::Blocking
        } else if lower.contains("incompatible")
            || lower.contains("mismatch")
            || lower.contains("major")
        {
            ConflictSeverity::Major
        } else if lower.contains("conflict") || lower.contains("differ") {
            ConflictSeverity::Moderate
        } else {
            ConflictSeverity::Minor
        };

        // Extract agents from common patterns like "between X and Y" or "X, Y"
        let agents = Self::extract_agents(description);

        Self {
            id,
            agents,
            topic: description.to_string(),
            positions: Vec::new(),
            severity,
            resolution: None,
        }
    }

    /// Extract agent/module names from conflict description.
    fn extract_agents(description: &str) -> Vec<String> {
        const STOP_WORDS: &[&str] = &[
            "and",
            "or",
            "the",
            "a",
            "an",
            "in",
            "on",
            "at",
            "to",
            "for",
            "of",
            "with",
            "by",
            "from",
            "module",
            "modules",
            "agent",
            "agents",
            "naming",
            "conflict",
            "conflicts",
            "issue",
            "issues",
        ];

        // Pattern: "between X and Y" or "between X, Y"
        if let Some(between_idx) = description.find("between ") {
            let after = &description[between_idx + 8..];
            let end = after.find(['.', ':']).unwrap_or(after.len());
            let agents_str = &after[..end];

            return agents_str
                .split(&[',', ' '][..])
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty() && !STOP_WORDS.contains(&s.as_str()))
                .collect();
        }

        // Pattern: "X vs Y" or "X versus Y"
        if description.contains(" vs ") || description.contains(" versus ") {
            let parts: Vec<&str> = description
                .split(&[' '][..])
                .filter(|s| !s.is_empty())
                .collect();

            if let Some(vs_idx) = parts.iter().position(|&s| s == "vs" || s == "versus") {
                let mut agents = Vec::new();
                if vs_idx > 0 {
                    let word = parts[vs_idx - 1]
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase();
                    if !STOP_WORDS.contains(&word.as_str()) {
                        agents.push(word);
                    }
                }
                if vs_idx + 1 < parts.len() {
                    let word = parts[vs_idx + 1]
                        .trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase();
                    if !STOP_WORDS.contains(&word.as_str()) {
                        agents.push(word);
                    }
                }
                return agents;
            }
        }

        Vec::new()
    }

    /// Convert a list of conflict description strings to structured Conflicts.
    pub fn from_descriptions(descriptions: &[String]) -> Vec<Self> {
        descriptions
            .iter()
            .enumerate()
            .map(|(i, desc)| Self::from_description(format!("conflict-{}", i), desc))
            .collect()
    }
}

/// How a conflict was resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictResolution {
    pub strategy: ResolutionStrategy,
    pub chosen_position: String,
    pub rationale: String,
}

/// Strategy for resolving conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStrategy {
    HighestScore,
    ModuleExpertPriority,
    Compromise,
    Escalate,
}

impl ResolutionStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HighestScore => "highest_score",
            Self::ModuleExpertPriority => "module_expert_priority",
            Self::Compromise => "compromise",
            Self::Escalate => "escalate",
        }
    }
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
    pub fn parse_qualified(&self) -> Option<super::shared::QualifiedModule> {
        self.assigned_module
            .as_ref()
            .and_then(|m| super::shared::QualifiedModule::from_qualified_string(m))
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
    pub consensus: ConsensusStatus,
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

/// Consensus status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusStatus {
    Yes,
    Partial,
    No,
}

impl ConsensusEngine {
    pub fn new(aggregator: Arc<TaskAgent>, config: ConsensusConfig) -> Self {
        Self {
            aggregator,
            config,
            agent_history: RwLock::new(HashMap::new()),
            event_store: None,
            escalation_engine: None,
            message_bus: None,
        }
    }

    pub fn with_event_store(mut self, store: Arc<EventStore>) -> Self {
        self.event_store = Some(store);
        self
    }

    pub fn with_escalation(mut self, engine: EscalationEngine) -> Self {
        self.escalation_engine = Some(engine);
        self
    }

    pub fn with_message_bus(mut self, bus: Arc<AgentMessageBus>) -> Self {
        self.message_bus = Some(bus);
        self
    }

    #[inline]
    fn is_timed_out(deadline: std::time::Instant) -> bool {
        std::time::Instant::now() >= deadline
    }

    async fn broadcast_consensus_request(&self, round: u32, proposal_hash: &str, proposal: &str) {
        let Some(bus) = &self.message_bus else { return };

        let message = AgentMessage::new(
            "consensus-engine",
            "*",
            MessagePayload::ConsensusRequest {
                round,
                proposal_hash: proposal_hash.to_string(),
                proposal: proposal.to_string(),
            },
        );

        if let Err(e) = bus.send(message).await {
            debug!(error = %e, "Failed to broadcast consensus request");
        }
    }

    async fn broadcast_consensus_vote(
        &self,
        agent_id: &str,
        round: u32,
        decision: VoteDecision,
        rationale: &str,
    ) {
        let Some(bus) = &self.message_bus else { return };

        let message =
            AgentMessage::consensus_vote(agent_id, round, decision, rationale.to_string());

        if let Err(e) = bus.send(message).await {
            debug!(error = %e, "Failed to broadcast consensus vote");
        }
    }

    async fn broadcast_conflict_alert(&self, conflict: &Conflict, round: u32) {
        let Some(bus) = &self.message_bus else { return };

        let message = AgentMessage::new(
            "consensus-engine",
            "*",
            MessagePayload::ConflictAlert {
                conflict_id: conflict.id.clone(),
                severity: conflict.severity,
                description: conflict.topic.clone(),
            },
        );

        if let Err(e) = bus.send(message).await {
            debug!(error = %e, round = round, "Failed to broadcast conflict alert");
        }
    }

    fn compute_proposal_hash(&self, content: &str, round: u32) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hasher.update(round.to_le_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn extract_affected_modules(&self, content: &str) -> Vec<String> {
        let mut modules = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            let Some(path) = line
                .split_whitespace()
                .find(|s| s.contains("src/") || s.contains("lib/"))
            else {
                continue;
            };

            let path = path.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '/' && c != '_' && c != '-' && c != '.'
            });
            if let Some(module) = path.split('/').nth(1) {
                let module = module.trim_end_matches(".rs");
                if !modules.contains(&module.to_string()) && !module.is_empty() {
                    modules.push(module.to_string());
                }
            }
        }
        modules.into_iter().take(10).collect()
    }

    async fn emit_event(&self, event: DomainEvent) {
        if let Some(ref store) = self.event_store
            && let Err(e) = store.append(event).await
        {
            warn!(error = %e, "Failed to emit consensus event");
        }
    }

    async fn load_projection(&self, mission_id: &str) -> Option<ConsensusProjection> {
        use crate::state::Projection;

        if let Some(ref store) = self.event_store {
            let events = store.query(mission_id, 0).await.ok()?;
            let mut projection = ConsensusProjection::default();
            for event in &events {
                projection.apply(event);
            }
            Some(projection)
        } else {
            None
        }
    }

    pub async fn run_with_dynamic_joining(
        &self,
        topic: &str,
        context: &TaskContext,
        initial_participants: &[AgentId],
        pool: &AgentPool,
        register_agents: impl Fn(&[AgentRequest]) -> Vec<AgentId>,
    ) -> Result<ConsensusResult> {
        info!(topic = %topic, participants = initial_participants.len(), "Starting consensus with dynamic joining");

        let mission_id = &context.mission_id;
        let mut session = ConsensusSession::new(initial_participants);
        let start_round = self.determine_start_round(mission_id).await;

        let consensus_start = std::time::Instant::now();
        let timeout_duration = std::time::Duration::from_secs(limits::CONSENSUS_TIMEOUT_SECS);

        for round_num in start_round..=self.config.max_rounds {
            // Check timeout before each round
            if consensus_start.elapsed() >= timeout_duration {
                warn!(
                    elapsed_secs = consensus_start.elapsed().as_secs(),
                    max_secs = limits::CONSENSUS_TIMEOUT_SECS,
                    round = round_num,
                    "Consensus timeout reached"
                );
                return Ok(ConsensusResult::NoConsensus {
                    summary: format!(
                        "Consensus timeout after {} seconds at round {}",
                        consensus_start.elapsed().as_secs(),
                        round_num
                    ),
                    blocking_conflicts: vec![],
                    respondent_count: 0,
                });
            }

            let round_ctx = RoundExecutionContext {
                topic,
                task_context: context,
                pool,
                timeout_deadline: consensus_start + timeout_duration,
            };
            match self
                .execute_round(&mut session, round_num, &round_ctx, &register_agents)
                .await?
            {
                RoundOutcomeResult::Converged(result) => return Ok(result),
                RoundOutcomeResult::Blocked(result) => return Ok(result),
                RoundOutcomeResult::Continue => {}
            }
        }

        self.finalize_without_full_consensus(&session)
    }

    async fn determine_start_round(&self, mission_id: &str) -> usize {
        if let Some(projection) = self.load_projection(mission_id).await {
            if projection.has_incomplete_round() {
                info!(
                    pending_votes = projection.pending_vote_count(),
                    "Resuming from incomplete consensus round"
                );
            }
            (projection.rounds.saturating_add(1).max(1)) as usize
        } else {
            1
        }
    }

    async fn execute_round(
        &self,
        session: &mut ConsensusSession,
        round_num: usize,
        ctx: &RoundExecutionContext<'_>,
        register_agents: &impl Fn(&[AgentRequest]) -> Vec<AgentId>,
    ) -> Result<RoundOutcomeResult> {
        debug!(
            round = round_num,
            participants = session.participants.len(),
            "Consensus round"
        );

        let mission_id = &ctx.task_context.mission_id;
        let round_u32 = round_num as u32;
        let proposal_hash = self.compute_proposal_hash(ctx.topic, round_u32);

        self.emit_event(DomainEvent::consensus_round_started(
            mission_id,
            round_u32,
            &proposal_hash,
            session
                .participants
                .iter()
                .map(|id| id.to_string())
                .collect(),
        ))
        .await;

        self.broadcast_consensus_request(round_u32, &proposal_hash, ctx.topic)
            .await;

        let scored_proposals = self
            .collect_scored_proposals(
                round_num,
                &session.previous_synthesis,
                &session.participants,
                ctx,
            )
            .await?;

        self.emit_vote_events(mission_id, round_u32, &scored_proposals)
            .await;

        if scored_proposals.len() < self.config.min_participants {
            self.emit_event(DomainEvent::consensus_round_completed(
                mission_id,
                round_u32,
                RoundOutcome::Failed,
                0.0,
            ))
            .await;
            return Ok(RoundOutcomeResult::Blocked(ConsensusResult::NoConsensus {
                summary: format!(
                    "Insufficient participants: {} < {}",
                    scored_proposals.len(),
                    self.config.min_participants
                ),
                blocking_conflicts: vec![],
                respondent_count: scored_proposals.len(),
            }));
        }

        // Check timeout before expensive synthesis operation
        if Self::is_timed_out(ctx.timeout_deadline) {
            warn!(
                round = round_num,
                "Timeout reached before synthesis, aborting round"
            );
            self.emit_event(DomainEvent::consensus_round_completed(
                mission_id,
                round_u32,
                RoundOutcome::Failed,
                0.0,
            ))
            .await;
            return Ok(RoundOutcomeResult::Blocked(ConsensusResult::NoConsensus {
                summary: format!(
                    "Consensus timeout reached during round {} (before synthesis)",
                    round_num
                ),
                blocking_conflicts: vec![],
                respondent_count: 0,
            }));
        }

        let synthesis_output = self
            .synthesize_with_scoring(&scored_proposals, ctx.topic, round_num)
            .await?;

        self.emit_conflict_events(mission_id, round_u32, &synthesis_output.conflicts)
            .await;

        // Process agent requests and run micro-round for newly joined agents
        let new_agent_proposals = self
            .process_agent_requests(
                session,
                &synthesis_output.agent_needed,
                round_num,
                ctx,
                register_agents,
            )
            .await?;

        // Merge proposals from micro-round with existing proposals
        let mut all_proposals = scored_proposals;
        all_proposals.extend(new_agent_proposals);
        let respondent_count = all_proposals.len();

        let converged = synthesis_output.consensus == ConsensusStatus::Yes;

        session.rounds.push(ConsensusRound {
            number: round_num,
            proposals: all_proposals,
            synthesis: serde_json::to_string_pretty(&synthesis_output)
                .inspect_err(|e| debug!(error = %e, "Failed to serialize synthesis"))
                .unwrap_or_default(),
            converged,
            conflicts: synthesis_output.conflicts.clone(),
            agent_requests: synthesis_output.agent_needed.clone(),
        });

        if converged {
            self.emit_event(DomainEvent::consensus_round_completed(
                mission_id,
                round_u32,
                RoundOutcome::Approved,
                1.0,
            ))
            .await;
            self.update_agent_history(&session.rounds);
            return Ok(RoundOutcomeResult::Converged(ConsensusResult::Agreed {
                plan: synthesis_output.plan,
                tasks: synthesis_output.tasks,
                rounds: round_num,
                respondent_count,
            }));
        }

        if let Some(result) = self
            .check_blocking_conflicts(mission_id, round_u32, &synthesis_output.conflicts, session)
            .await
        {
            return Ok(RoundOutcomeResult::Blocked(result));
        }

        self.emit_event(DomainEvent::consensus_round_completed(
            mission_id,
            round_u32,
            RoundOutcome::NeedsMoreRounds,
            0.5,
        ))
        .await;

        session.previous_synthesis = serde_json::to_string(&synthesis_output)
            .inspect_err(|e| debug!(error = %e, "Failed to serialize synthesis for next round"))
            .unwrap_or_default();

        Ok(RoundOutcomeResult::Continue)
    }

    async fn emit_vote_events(&self, mission_id: &str, round: u32, proposals: &[ScoredProposal]) {
        for sp in proposals {
            let decision = if sp.score.weighted_score() >= scoring::APPROVE_THRESHOLD {
                VoteDecision::Approve
            } else if sp.score.weighted_score() >= scoring::APPROVE_WITH_CHANGES_THRESHOLD {
                VoteDecision::ApproveWithChanges
            } else {
                VoteDecision::Reject
            };

            self.emit_event(DomainEvent::consensus_vote_received(
                mission_id,
                round,
                &sp.proposal.agent_id,
                decision,
                sp.score.weighted_score(),
            ))
            .await;

            if sp.proposal.role.is_module() {
                self.emit_event(DomainEvent::consensus_module_vote(
                    mission_id,
                    round,
                    &sp.proposal.agent_id,
                    &sp.proposal.role.id,
                    decision.as_str(),
                    sp.score.confidence,
                ))
                .await;
            }

            let rationale = sp
                .proposal
                .content
                .lines()
                .next()
                .unwrap_or("No rationale")
                .to_string();
            self.broadcast_consensus_vote(&sp.proposal.agent_id, round, decision, &rationale)
                .await;
        }
    }

    async fn emit_conflict_events(&self, mission_id: &str, round: u32, conflicts: &[Conflict]) {
        for conflict in conflicts {
            self.emit_event(DomainEvent::consensus_conflict_detected(
                mission_id,
                round,
                &conflict.id,
                conflict.agents.clone(),
                conflict.severity,
            ))
            .await;

            self.broadcast_conflict_alert(conflict, round).await;

            if let Some(ref resolution) = conflict.resolution {
                self.emit_event(DomainEvent::consensus_conflict_resolved(
                    mission_id,
                    &conflict.id,
                    resolution.strategy.as_str(),
                ))
                .await;
            }
        }
    }

    async fn process_agent_requests(
        &self,
        session: &mut ConsensusSession,
        requests: &[AgentRequest],
        round_num: usize,
        ctx: &RoundExecutionContext<'_>,
        register_agents: &impl Fn(&[AgentRequest]) -> Vec<AgentId>,
    ) -> Result<Vec<ScoredProposal>> {
        if requests.is_empty() {
            return Ok(vec![]);
        }

        info!(
            count = requests.len(),
            round = round_num,
            "Requested additional agents"
        );

        let registered_ids = register_agents(requests);
        for agent_id in &registered_ids {
            session.add_participant(agent_id.clone());
        }

        let registered_requests: Vec<_> = requests
            .iter()
            .filter(|req| {
                let role_id = AgentId::module(&req.module);
                registered_ids.contains(&role_id)
            })
            .cloned()
            .collect();

        for req in &registered_requests {
            let role_id = super::traits::AgentRole::module(&req.module).id;
            self.emit_event(DomainEvent::agent_spawned(
                &ctx.task_context.mission_id,
                &role_id,
                &req.module,
                None,
            ))
            .await;
        }

        self.inject_context_to_new_agents(
            &registered_requests,
            &session.rounds,
            ctx.pool,
            &ctx.task_context.working_dir,
        )
        .await?;

        // Micro-round: collect proposals from newly joined agents
        if registered_ids.is_empty() {
            return Ok(vec![]);
        }

        info!(
            agents = ?registered_ids,
            "Running micro-round for newly joined agents"
        );

        let previous_synthesis = session
            .rounds
            .last()
            .map(|r| r.synthesis.clone())
            .unwrap_or_default();

        let new_proposals = self
            .collect_scored_proposals(round_num, &previous_synthesis, &registered_ids, ctx)
            .await?;

        info!(
            count = new_proposals.len(),
            "Collected proposals from new agents in micro-round"
        );

        Ok(new_proposals)
    }

    async fn check_blocking_conflicts(
        &self,
        mission_id: &str,
        round: u32,
        conflicts: &[Conflict],
        session: &ConsensusSession,
    ) -> Option<ConsensusResult> {
        let blocking: Vec<_> = conflicts
            .iter()
            .filter(|c| c.severity == ConflictSeverity::Blocking && c.resolution.is_none())
            .cloned()
            .collect();

        if blocking.is_empty() {
            return None;
        }

        self.emit_event(DomainEvent::consensus_round_completed(
            mission_id,
            round,
            RoundOutcome::Failed,
            0.0,
        ))
        .await;

        warn!(
            count = blocking.len(),
            "Blocking conflicts prevent consensus"
        );

        Some(ConsensusResult::NoConsensus {
            summary: format!("Blocked by {} unresolved conflicts", blocking.len()),
            blocking_conflicts: blocking,
            respondent_count: session.rounds.last().map_or(0, |r| r.proposals.len()),
        })
    }

    fn finalize_without_full_consensus(
        &self,
        session: &ConsensusSession,
    ) -> Result<ConsensusResult> {
        self.update_agent_history(&session.rounds);

        let Some(last) = session.rounds.last() else {
            return Ok(ConsensusResult::NoConsensus {
                summary: "No rounds completed".into(),
                blocking_conflicts: vec![],
                respondent_count: 0,
            });
        };

        let respondent_count = last.proposals.len();
        let output: ConsensusSynthesisOutput = serde_json::from_str(&last.synthesis)
            .inspect_err(|e| debug!(error = %e, "Failed to parse last synthesis"))
            .unwrap_or_else(|_| ConsensusSynthesisOutput {
                consensus: ConsensusStatus::Partial,
                plan: last.synthesis.clone(),
                tasks: vec![],
                conflicts: last.conflicts.clone(),
                dissents: vec![],
                agent_needed: vec![],
            });

        if output.consensus == ConsensusStatus::Partial {
            Ok(ConsensusResult::PartialAgreement {
                plan: output.plan,
                dissents: output.dissents,
                unresolved_conflicts: output.conflicts,
                respondent_count,
            })
        } else {
            Ok(ConsensusResult::NoConsensus {
                summary: "Max rounds reached without consensus".into(),
                blocking_conflicts: output.conflicts,
                respondent_count,
            })
        }
    }

    async fn collect_scored_proposals(
        &self,
        round: usize,
        previous_synthesis: &str,
        participants: &[AgentId],
        ctx: &RoundExecutionContext<'_>,
    ) -> Result<Vec<ScoredProposal>> {
        // Check timeout before starting parallel collection
        if Self::is_timed_out(ctx.timeout_deadline) {
            warn!(round = round, "Timeout reached before collecting proposals");
            return Ok(Vec::new());
        }

        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_llm_calls));
        let per_agent_timeout = Duration::from_secs(self.config.per_agent_timeout_secs);
        let prompt =
            self.build_proposal_prompt(ctx.topic, ctx.task_context, round, previous_synthesis);
        let mission_id = ctx.task_context.mission_id.clone();
        let working_dir = ctx.task_context.working_dir.clone();
        let topic = ctx.topic.to_string();
        let task_context = ctx.task_context.clone();

        let futures: Vec<_> = participants
            .iter()
            .filter_map(|agent_id| {
                let agent = ctx
                    .pool
                    .select_by_qualified_id(agent_id.as_str())
                    .or_else(|| ctx.pool.select_by_id(agent_id.as_str()));

                let agent = match agent {
                    Some(a) => a,
                    None => {
                        warn!(agent_id = %agent_id, "Agent not found in pool, skipping");
                        return None;
                    }
                };

                let sem = Arc::clone(&semaphore);
                let prompt = prompt.clone();
                let agent_id = agent_id.clone();
                let working_dir = working_dir.clone();
                let task_context = task_context.clone();

                Some(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return None,
                    };

                    let task = AgentTask {
                        id: format!("consensus-r{}-{}", round, agent_id),
                        description: prompt,
                        context: task_context,
                        priority: TaskPriority::High,
                        role: Some(agent.role().clone()),
                    };

                    let result = match tokio::time::timeout(
                        per_agent_timeout,
                        agent.execute(&task, &working_dir),
                    )
                    .await
                    {
                        Ok(Ok(result)) => result,
                        Ok(Err(e)) => {
                            debug!(agent = agent.id(), error = %e, "Agent proposal failed");
                            return None;
                        }
                        Err(_) => {
                            warn!(agent = agent.id(), "Agent proposal timed out");
                            return None;
                        }
                    };

                    Some((agent, result))
                })
            })
            .collect();

        let results = join_all(futures).await;

        let mut proposals = Vec::with_capacity(results.len());
        for (agent, result) in results.into_iter().flatten() {
            let score = self.calculate_proposal_score(agent.id(), agent.role(), &result, &topic);

            let proposal_hash = self.compute_proposal_hash(&result.output, round as u32);
            let affected_modules = self.extract_affected_modules(&result.output);

            self.emit_event(DomainEvent::consensus_proposal_submitted(
                &mission_id,
                round as u32,
                agent.id(),
                &proposal_hash,
                affected_modules,
            ))
            .await;

            proposals.push(ScoredProposal {
                proposal: AgentProposal {
                    agent_id: agent.id().to_string(),
                    role: agent.role().clone(),
                    content: result.output,
                },
                score,
            });
        }

        info!(
            round = round,
            collected = proposals.len(),
            total_participants = participants.len(),
            "Parallel proposal collection completed"
        );

        Ok(proposals)
    }

    /// Collect proposals with cross-visibility context.
    ///
    /// In cross-visibility mode, all agents see each other's proposals and can
    /// refine their approaches based on what others propose. This enables semantic
    /// convergence on API contracts and shared types.
    pub async fn collect_with_cross_visibility(
        &self,
        participants: &[AgentId],
        topic: &str,
        context: &TaskContext,
        pool: &AgentPool,
        max_rounds: usize,
    ) -> Result<Vec<EnhancedProposal>> {
        let mut all_proposals: std::collections::HashMap<String, EnhancedProposal> =
            std::collections::HashMap::new();

        for round in 0..max_rounds {
            debug!(round = round, "Cross-visibility collection round");

            // Build context with peer proposals
            let peer_context = self.build_peer_context(&all_proposals);

            // Collect proposals from all participants with peer visibility
            let round_proposals = self
                .collect_enhanced_proposals(
                    participants,
                    topic,
                    context,
                    pool,
                    &peer_context,
                    round,
                )
                .await?;

            // Update proposals (agents can refine based on what they saw)
            for proposal in round_proposals {
                all_proposals.insert(proposal.agent_id.clone(), proposal);
            }

            // Check for convergence using semantic analysis
            if self.check_proposal_convergence(&all_proposals) {
                info!(round = round, "Cross-visibility proposals converged");
                break;
            }
        }

        Ok(all_proposals.into_values().collect())
    }

    async fn collect_enhanced_proposals(
        &self,
        participants: &[AgentId],
        topic: &str,
        context: &TaskContext,
        pool: &AgentPool,
        peer_context: &str,
        round: usize,
    ) -> Result<Vec<EnhancedProposal>> {
        let semaphore = Arc::new(Semaphore::new(self.config.max_concurrent_llm_calls));
        let per_agent_timeout = Duration::from_secs(self.config.per_agent_timeout_secs);
        let working_dir = context.working_dir.clone();
        let topic = topic.to_string();
        let peer_context = peer_context.to_string();
        let context = context.clone();

        let futures: Vec<_> = participants
            .iter()
            .filter_map(|agent_id| {
                let agent = pool
                    .select_by_qualified_id(agent_id.as_str())
                    .or_else(|| pool.select_by_id(agent_id.as_str()));

                let agent = match agent {
                    Some(a) => a,
                    None => {
                        warn!(agent_id = %agent_id, "Agent not found for enhanced proposal");
                        return None;
                    }
                };

                let sem = Arc::clone(&semaphore);
                let agent_id = agent_id.clone();
                let working_dir = working_dir.clone();
                let context = context.clone();
                let topic = topic.clone();
                let peer_context = peer_context.clone();

                Some(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => return None,
                    };

                    let prompt = Self::build_cross_visibility_prompt_static(
                        &topic,
                        &context,
                        &peer_context,
                        round,
                    );

                    let task = AgentTask {
                        id: format!("cross-vis-r{}-{}", round, agent_id),
                        description: prompt,
                        context,
                        priority: TaskPriority::High,
                        role: Some(agent.role().clone()),
                    };

                    let result = match tokio::time::timeout(
                        per_agent_timeout,
                        agent.execute(&task, &working_dir),
                    )
                    .await
                    {
                        Ok(Ok(result)) => result,
                        Ok(Err(e)) => {
                            debug!(agent = agent.id(), error = %e, "Enhanced proposal failed");
                            return None;
                        }
                        Err(_) => {
                            warn!(agent = agent.id(), "Enhanced proposal timed out");
                            return None;
                        }
                    };

                    let enhanced = Self::parse_enhanced_proposal_static(
                        agent.id(),
                        agent.role().module_id().unwrap_or("unknown"),
                        &result.output,
                    );

                    Some(enhanced)
                })
            })
            .collect();

        let results = join_all(futures).await;
        Ok(results.into_iter().flatten().collect())
    }

    fn build_cross_visibility_prompt_static(
        topic: &str,
        context: &TaskContext,
        peer_context: &str,
        round: usize,
    ) -> String {
        let mut prompt = format!(
            "# Planning Task (Round {})\n\n## Topic\n{}\n\n",
            round + 1,
            topic
        );

        if !context.key_findings.is_empty() {
            prompt.push_str("## Key Findings\n");
            prompt.push_str(&context.key_findings.join("\n"));
            prompt.push_str("\n\n");
        }

        if !peer_context.is_empty() {
            prompt.push_str(peer_context);
            prompt.push('\n');
        }

        prompt.push_str("## Your Response\nProvide your proposal in the following JSON format:\n");
        prompt.push_str(r#"```json
{
  "summary": "Brief summary of your approach",
  "approach": "Detailed approach description",
  "affected_files": ["file1.rs", "file2.rs"],
  "api_changes": [{"name": "function_name", "change_type": "add|remove|modify", "signature": "fn signature()"}],
  "type_changes": [{"type_name": "TypeName", "change_type": "add_field|change_type", "details": "description"}],
  "dependencies_used": ["module_a", "module_b"],
  "dependencies_provided": ["api_x", "api_y"]
}
```"#);

        prompt
    }

    fn build_peer_context(
        &self,
        proposals: &std::collections::HashMap<String, EnhancedProposal>,
    ) -> String {
        if proposals.is_empty() {
            return String::new();
        }

        let mut context = String::from("## Current Peer Proposals\n\n");
        for proposal in proposals.values() {
            context.push_str(&format!(
                "**{}** ({}): {}\n",
                proposal.agent_id,
                proposal.module_id,
                proposal.brief_summary()
            ));

            if !proposal.api_changes.is_empty() {
                let api_names: Vec<_> = proposal
                    .api_changes
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect();
                context.push_str(&format!("  - API changes: {}\n", api_names.join(", ")));
            }

            if !proposal.dependencies_provided.is_empty() {
                context.push_str(&format!(
                    "  - Provides: {}\n",
                    proposal.dependencies_provided.join(", ")
                ));
            }

            context.push('\n');
        }
        context
    }

    /// Parse LLM response into structured EnhancedProposal.
    ///
    /// Extracts JSON block and parses API changes, type changes, and dependencies
    /// for semantic convergence checking. Falls back to summary-only on parse failure.
    fn parse_enhanced_proposal_static(
        agent_id: &str,
        module_id: &str,
        response: &str,
    ) -> EnhancedProposal {
        let json_str = Self::extract_json_block(response);

        #[derive(Deserialize)]
        struct ProposalJson {
            #[serde(default)]
            summary: String,
            #[serde(default)]
            approach: String,
            #[serde(default)]
            affected_files: Vec<String>,
            #[serde(default)]
            api_changes: Vec<ProposedApiChange>,
            #[serde(default)]
            type_changes: Vec<ProposedTypeChange>,
            #[serde(default)]
            dependencies_used: Vec<String>,
            #[serde(default)]
            dependencies_provided: Vec<String>,
        }

        match serde_json::from_str::<ProposalJson>(json_str) {
            Ok(parsed) => EnhancedProposal {
                agent_id: agent_id.to_string(),
                module_id: module_id.to_string(),
                summary: parsed.summary,
                approach: parsed.approach,
                affected_files: parsed.affected_files,
                api_changes: parsed.api_changes,
                type_changes: parsed.type_changes,
                dependencies_used: parsed.dependencies_used,
                dependencies_provided: parsed.dependencies_provided,
            },
            Err(e) => {
                warn!(
                    agent = %agent_id,
                    module = %module_id,
                    error = %e,
                    json_excerpt = %json_str.chars().take(100).collect::<String>(),
                    "Failed to parse enhanced proposal JSON, using fallback"
                );
                // Fallback: use raw response as summary (no API/type change tracking)
                EnhancedProposal::new(agent_id, module_id)
                    .with_summary(response.lines().next().unwrap_or("No summary"))
                    .with_approach(response.to_string())
            }
        }
    }

    fn check_proposal_convergence(
        &self,
        proposals: &std::collections::HashMap<String, EnhancedProposal>,
    ) -> bool {
        if proposals.len() <= 1 {
            return true;
        }

        // Check for API naming conflicts
        let mut api_names: std::collections::HashMap<String, Vec<&str>> =
            std::collections::HashMap::new();
        for proposal in proposals.values() {
            for api in &proposal.api_changes {
                api_names
                    .entry(api.name.clone())
                    .or_default()
                    .push(&proposal.agent_id);
            }
        }

        // If same API is being changed by multiple agents differently, not converged
        for agents in api_names.values() {
            if agents.len() > 1 {
                // Multiple agents changing same API - check if they agree
                let signatures: std::collections::HashSet<_> = proposals
                    .values()
                    .filter(|p| agents.contains(&p.agent_id.as_str()))
                    .flat_map(|p| p.api_changes.iter())
                    .filter_map(|a| a.signature.as_ref())
                    .collect();

                if signatures.len() > 1 {
                    return false; // Conflicting signatures
                }
            }
        }

        // Check dependency satisfaction
        let mut all_provided: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut all_used: std::collections::HashSet<&str> = std::collections::HashSet::new();

        for proposal in proposals.values() {
            all_provided.extend(proposal.dependencies_provided.iter().map(String::as_str));
            all_used.extend(proposal.dependencies_used.iter().map(String::as_str));
        }

        // At least 80% of used dependencies should be provided
        if !all_used.is_empty() {
            let satisfied = all_used.intersection(&all_provided).count();
            let ratio = satisfied as f64 / all_used.len() as f64;
            if ratio < 0.8 {
                return false;
            }
        }

        true
    }

    fn calculate_proposal_score(
        &self,
        agent_id: &str,
        role: &AgentRole,
        result: &AgentTaskResult,
        topic: &str,
    ) -> ProposalScore {
        let module_relevance = if let Some(module_id) = role.module_id() {
            if topic.to_lowercase().contains(&module_id.to_lowercase()) {
                scoring::MODULE_MATCH_SCORE
            } else {
                scoring::MODULE_PARTIAL_SCORE
            }
        } else if role.is_core() {
            scoring::CORE_ROLE_SCORE
        } else {
            scoring::DEFAULT_ROLE_SCORE
        };

        let historical_accuracy = match self.agent_history.read() {
            Ok(history) => history.get(agent_id).map_or(
                scoring::DEFAULT_ACCURACY,
                AgentPerformanceHistory::accuracy_rate,
            ),
            Err(_) => {
                debug!(agent_id = %agent_id, "Failed to read agent history, using default accuracy");
                scoring::DEFAULT_ACCURACY
            }
        };

        // Assess evidence quality (strength + confidence)
        let evidence = self.assess_evidence_quality(&result.output);

        ProposalScore {
            agent_id: agent_id.to_string(),
            module_relevance,
            historical_accuracy,
            evidence_strength: evidence.strength,
            confidence: evidence.confidence,
        }
    }

    fn assess_evidence_quality(&self, content: &str) -> EvidenceQuality {
        // Pre-compute shared detection results
        let has_file_ref = Self::has_file_reference(content);
        let has_line_ref = Self::has_line_reference(content);
        let has_code = content.contains("```");
        let has_error_code = Self::has_error_code(content);
        let has_identifier = Self::has_identifier_pattern(content);
        let line_count = content.lines().count();

        // Strength calculation
        let mut strength = scoring::BASE_EVIDENCE_SCORE;
        if has_file_ref {
            strength += scoring::FILE_REFERENCE_BONUS;
        }
        if has_code {
            strength += scoring::CODE_SNIPPET_BONUS;
        }
        if has_line_ref {
            strength += scoring::LINE_REFERENCE_BONUS;
        }
        if has_error_code {
            strength += scoring::ERROR_CODE_BONUS;
        }
        if has_identifier {
            strength += scoring::IDENTIFIER_PATTERN_BONUS;
        }

        // Confidence calculation
        let mut confidence = scoring::BASE_EVIDENCE_SCORE;
        if line_count > 20 {
            confidence += scoring::CONSISTENCY_BONUS;
        } else if line_count > 10 {
            confidence += scoring::IMPLEMENTATION_DETAIL_BONUS;
        }

        let list_items = content
            .lines()
            .filter(|l| {
                let t = l.trim();
                t.starts_with("- ")
                    || t.starts_with("* ")
                    || t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('.')
            })
            .count();
        if list_items >= 3 {
            confidence += scoring::IMPLEMENTATION_DETAIL_BONUS;
        }

        let file_refs = content
            .lines()
            .filter(|l| Self::has_file_extension(l) || Self::has_line_reference(l))
            .count();
        if file_refs >= 3 {
            confidence += scoring::TOPIC_MATCH_BONUS;
        } else if file_refs >= 1 {
            confidence += scoring::REFERENCE_BONUS;
        }

        EvidenceQuality {
            strength: strength.min(1.0),
            confidence: confidence.clamp(scoring::MIN_EVIDENCE_SCORE, 1.0),
        }
    }

    fn has_file_reference(content: &str) -> bool {
        content.contains("src/")
            || content.contains("lib/")
            || content.contains("test/")
            || content.contains("./")
            || Self::has_file_extension(content)
    }

    fn has_file_extension(content: &str) -> bool {
        const EXTENSIONS: &[&str] = &[
            ".rs", ".py", ".go", ".java", ".ts", ".js", ".tsx", ".jsx", ".rb", ".php", ".c",
            ".cpp", ".h", ".hpp", ".cs", ".kt", ".swift", ".scala", ".ex", ".exs", ".clj", ".zig",
            ".toml", ".yaml", ".yml", ".json", ".xml", ".html", ".css", ".sql",
        ];
        EXTENSIONS.iter().any(|ext| content.contains(ext))
    }

    fn has_line_reference(content: &str) -> bool {
        let chars: Vec<char> = content.chars().collect();
        chars.iter().enumerate().any(|(i, &c)| {
            if c == ':' && i > 0 && i + 1 < chars.len() {
                let start_before = i.saturating_sub(4);
                let has_ext_before = chars[start_before..i].contains(&'.');
                let has_digit_after = chars.get(i + 1).is_some_and(|ch| ch.is_ascii_digit());
                has_ext_before && has_digit_after
            } else {
                false
            }
        })
    }

    fn has_error_code(content: &str) -> bool {
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_uppercase() {
                let mut code = String::new();
                code.push(c);
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphanumeric() && code.len() < 10 {
                        code.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
                let letters = code.chars().take_while(|c| c.is_ascii_uppercase()).count();
                let digits = code
                    .chars()
                    .skip(letters)
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                if (1..=3).contains(&letters) && (2..=5).contains(&digits) {
                    return true;
                }
            }
        }
        false
    }

    fn has_identifier_pattern(content: &str) -> bool {
        content.split_whitespace().any(|word| {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if clean.len() < 3 {
                return false;
            }
            let has_camel = clean.chars().skip(1).any(|c| c.is_ascii_uppercase());
            let has_snake = clean.contains('_');
            has_camel || has_snake
        })
    }

    async fn synthesize_with_scoring(
        &self,
        proposals: &[ScoredProposal],
        topic: &str,
        round: usize,
    ) -> Result<ConsensusSynthesisOutput> {
        // Sort proposals by score for weighted consideration
        let mut sorted: Vec<_> = proposals.to_vec();
        sorted.sort_by(|a, b| {
            b.score
                .weighted_score()
                .partial_cmp(&a.score.weighted_score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let proposals_text: Vec<String> = sorted
            .iter()
            .map(|sp| {
                format!(
                    "### {} ({}) [Score: {:.2}]\n{}\n\nScoring: module={:.2}, history={:.2}, evidence={:.2}, confidence={:.2}",
                    sp.proposal.agent_id,
                    sp.proposal.role,
                    sp.score.weighted_score(),
                    sp.proposal.content,
                    sp.score.module_relevance,
                    sp.score.historical_accuracy,
                    sp.score.evidence_strength,
                    sp.score.confidence,
                )
            })
            .collect();

        let prompt = format!(
            r#"## Consensus Synthesis (Round {round})

Topic: {topic}

## Scored Agent Proposals (sorted by weighted score)
{proposals}

## Instructions
Synthesize proposals using evidence-weighted voting:
1. Higher-scored proposals carry more weight
2. Identify and resolve conflicts explicitly
3. Request additional module agents if expertise is missing

IMPORTANT: Output MUST be valid JSON following this exact structure:

```json
{{
  "consensus": "yes" | "partial" | "no",
  "plan": "unified implementation plan as string",
  "tasks": [
    {{
      "id": "task-1",
      "description": "task description",
      "assigned_module": "module-name or workspace::domain::module for cross-workspace",
      "dependencies": ["task-id-1"],
      "priority": "normal" | "high" | "critical" | "low",
      "estimated_complexity": "trivial" | "low" | "medium" | "high" | "complex",
      "files_affected": ["src/path/file.rs"]
    }}
  ],
  "conflicts": [
    {{
      "id": "conflict-1",
      "agents": ["agent-1", "agent-2"],
      "topic": "what they disagree on",
      "positions": ["agent-1's position", "agent-2's position"],
      "severity": "minor" | "moderate" | "major" | "blocking",
      "resolution": {{
        "strategy": "highest_score" | "module_expert_priority" | "compromise" | "escalate",
        "chosen_position": "the chosen approach",
        "rationale": "why this was chosen"
      }}
    }}
  ],
  "dissents": ["unresolved disagreement 1"],
  "agent_needed": [
    {{
      "module": "module-name",
      "reason": "why this module expert is needed"
    }}
  ]
}}
```

Respond with ONLY the JSON block, no other text."#,
            proposals = proposals_text.join("\n\n"),
        );

        let working_dir = std::env::current_dir().unwrap_or_default();
        let response = self
            .aggregator
            .run_with_profile(&prompt, &working_dir, super::PermissionProfile::ReadOnly)
            .await?;

        // Parse JSON from response
        self.parse_synthesis_json(&response)
    }

    fn parse_synthesis_json(&self, response: &str) -> Result<ConsensusSynthesisOutput> {
        let json_str = Self::extract_json_block(response);

        serde_json::from_str::<ConsensusSynthesisOutput>(json_str).map_err(|e| {
            error!(
                error = %e,
                json_excerpt = %json_str.chars().take(200).collect::<String>(),
                "Failed to parse synthesis JSON"
            );
            PilotError::AgentExecution(format!("Invalid synthesis JSON: {e}"))
        })
    }

    fn find_json_bounds(s: &str) -> Option<(usize, usize)> {
        let start = s.find('{')?;
        let mut depth = 0;
        for (i, c) in s[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start, start + i + 1));
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn extract_json_block(response: &str) -> &str {
        // Try ```json``` code block first
        if let Some(start) = response.find("```json") {
            let content = &response[start + 7..];
            return content
                .find("```")
                .map(|end| content[..end].trim())
                .unwrap_or_else(|| content.trim());
        }

        // Try raw JSON object
        if let Some((start, end)) = Self::find_json_bounds(response) {
            return &response[start..end];
        }

        response.trim()
    }

    fn build_proposal_prompt(
        &self,
        topic: &str,
        context: &TaskContext,
        round: usize,
        previous_synthesis: &str,
    ) -> String {
        let mut prompt = format!("## Consensus Round {round}\n\nTopic: {topic}\n");

        if !context.key_findings.is_empty() {
            prompt.push_str("\n## Context\n");
            for finding in &context.key_findings {
                prompt.push_str(&format!("- {finding}\n"));
            }
        }

        if !previous_synthesis.is_empty() {
            prompt.push_str(&format!(
                "\n## Previous Round Synthesis\n{previous_synthesis}\n"
            ));
            prompt.push_str(
                "\nBuild on or refine the previous synthesis from your module perspective.\n",
            );
        } else {
            prompt.push_str("\nPropose an implementation approach from your module perspective.\n");
        }

        prompt.push_str(
            r#"
Provide:
1. Your proposed approach
2. Files/components affected in your module
3. Dependencies on other modules
4. Concerns or risks
5. Your confidence level (high/medium/low)

If you need expertise from a module not currently represented, include:
AGENT_NEEDED: module="module-name" reason="why this expert is needed"
"#,
        );

        prompt
    }

    async fn inject_context_to_new_agents(
        &self,
        requests: &[AgentRequest],
        previous_rounds: &[ConsensusRound],
        pool: &AgentPool,
        working_dir: &Path,
    ) -> Result<()> {
        if previous_rounds.is_empty() {
            return Ok(());
        }

        let context_summary = self.summarize_rounds(previous_rounds);
        let open_questions = self.extract_open_questions(previous_rounds);

        for request in requests {
            let role_id = super::traits::AgentRole::module(&request.module).id;
            if let Some(agent) = pool.select_by_id(&role_id) {
                let onboarding_prompt = format!(
                    r"You are joining an ongoing consensus discussion as a {} module expert.

## Why You Were Requested
{}

## Previous Agreement Summary
{}

## Current Open Questions
{}

Please review this context and be ready to provide your module perspective in the next round.",
                    request.module, request.reason, context_summary, open_questions,
                );

                let task = AgentTask {
                    id: format!("onboarding-{}", role_id),
                    description: onboarding_prompt,
                    context: TaskContext::default(),
                    priority: TaskPriority::High,
                    role: Some(agent.role().clone()),
                };

                // Onboarding is best-effort but we log failures for debugging
                match agent.execute(&task, working_dir).await {
                    Ok(_) => {
                        debug!(module = %request.module, "Successfully onboarded new agent with context");
                    }
                    Err(e) => {
                        warn!(
                            module = %request.module,
                            error = %e,
                            "Failed to onboard new agent - agent may participate without full context"
                        );
                    }
                }
            } else {
                warn!(module = %request.module, "Could not find registered agent for context injection");
            }
        }

        Ok(())
    }

    fn summarize_rounds(&self, rounds: &[ConsensusRound]) -> String {
        let mut summary = String::new();

        for round in rounds {
            summary.push_str(&format!("\n### Round {}\n", round.number));

            // Top proposals by score
            let mut sorted: Vec<_> = round.proposals.iter().collect();
            sorted.sort_by(|a, b| {
                b.score
                    .weighted_score()
                    .partial_cmp(&a.score.weighted_score())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            for sp in sorted.iter().take(3) {
                summary.push_str(&format!(
                    "- {} (score: {:.2}): {}\n",
                    sp.proposal.agent_id,
                    sp.score.weighted_score(),
                    sp.proposal.content.lines().next().unwrap_or("")
                ));
            }

            if !round.conflicts.is_empty() {
                summary.push_str("\nConflicts:\n");
                for conflict in &round.conflicts {
                    summary.push_str(&format!("- {} ({:?})\n", conflict.topic, conflict.severity));
                }
            }
        }

        summary
    }

    fn extract_open_questions(&self, rounds: &[ConsensusRound]) -> String {
        let mut questions = Vec::new();

        if let Some(last) = rounds.last() {
            // Unresolved conflicts become questions
            for conflict in &last.conflicts {
                if conflict.resolution.is_none() {
                    questions.push(format!("- {}: {:?}", conflict.topic, conflict.positions));
                }
            }
        }

        if questions.is_empty() {
            "No specific open questions.".to_string()
        } else {
            questions.join("\n")
        }
    }

    fn update_agent_history(&self, rounds: &[ConsensusRound]) {
        // Update history based on which proposals were accepted
        let Ok(mut history) = self.agent_history.write() else {
            warn!("Failed to acquire write lock on agent history - history update skipped");
            return;
        };

        for round in rounds {
            for sp in &round.proposals {
                let entry = history.entry(sp.proposal.agent_id.clone()).or_default();
                entry.total_proposals += 1;

                if (round.converged && sp.score.weighted_score() > scoring::CONVERGENCE_THRESHOLD)
                    || (!round.converged
                        && sp.score.weighted_score() > scoring::PARTIAL_CONVERGENCE_THRESHOLD)
                {
                    entry.accepted_proposals += 1;
                }

                // Track module contributions
                if sp.proposal.role.is_module() {
                    *entry
                        .module_contributions
                        .entry(sp.proposal.role.id.clone())
                        .or_insert(0) += 1;
                }
            }
        }

        // Prune history if it exceeds threshold to prevent unbounded memory growth
        // Note: Check is performed again inside prune_agent_history to prevent race conditions
        if history.len() > limits::HISTORY_PRUNE_THRESHOLD {
            self.prune_agent_history(&mut history);
        }
    }

    fn prune_agent_history(&self, history: &mut HashMap<String, AgentPerformanceHistory>) {
        // Keep only the most active agents (by total proposals) up to MAX_AGENT_HISTORY_ENTRIES
        // Double-check size to prevent race condition where multiple threads could trigger pruning
        if history.len() <= limits::MAX_AGENT_HISTORY_ENTRIES {
            return;
        }

        let mut entries: Vec<_> = history
            .iter()
            .map(|(k, v)| (k.clone(), v.total_proposals))
            .collect();

        // Sort by total proposals descending
        entries.sort_by(|a, b| b.1.cmp(&a.1));

        // Collect keys to remove (those beyond the limit)
        let keys_to_remove: Vec<String> = entries
            .into_iter()
            .skip(limits::MAX_AGENT_HISTORY_ENTRIES)
            .map(|(k, _)| k)
            .collect();

        for key in keys_to_remove {
            history.remove(&key);
        }

        info!(
            remaining = history.len(),
            pruned_to = limits::MAX_AGENT_HISTORY_ENTRIES,
            "Pruned agent history to prevent unbounded growth"
        );
    }

    /// Parse AGENT_NEEDED requests from synthesis output.
    pub fn parse_agent_requests(synthesis: &str) -> Vec<AgentRequest> {
        synthesis
            .lines()
            .filter(|line| {
                let trimmed = line.trim().to_uppercase();
                trimmed.starts_with("AGENT_NEEDED:") || trimmed.starts_with("AGENT_NEEDED ")
            })
            .filter_map(|line| {
                let module = extract_field(line, "module:")?;
                let reason = extract_field(line, "reason:")
                    .unwrap_or_else(|| "Requested during consensus".to_string());
                Some(AgentRequest { module, reason })
            })
            .collect()
    }

    /// Check if consensus requests additional agents.
    pub fn needs_additional_agents(synthesis: &str) -> bool {
        let upper = synthesis.to_uppercase();
        upper.contains("AGENT_NEEDED:")
            || upper.contains("AGENT_NEEDED ")
            || upper.contains("NEED MODULE EXPERT")
            || upper.contains("REQUIRE SPECIALIST")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proposal_score_weights() {
        let score = ProposalScore {
            agent_id: "test".into(),
            module_relevance: 1.0,
            historical_accuracy: 1.0,
            evidence_strength: 1.0,
            confidence: 1.0,
        };

        let total = score.weighted_score();
        assert!((total - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_proposal_score_weighted() {
        let score = ProposalScore {
            agent_id: "test".into(),
            module_relevance: 0.9,
            historical_accuracy: 0.7,
            evidence_strength: 0.8,
            confidence: 0.6,
        };

        let expected = 0.9 * 0.35 + 0.7 * 0.25 + 0.8 * 0.25 + 0.6 * 0.15;
        assert!((score.weighted_score() - expected).abs() < 0.001);
    }

    #[test]
    fn test_extract_field() {
        let line = r#"task: "implement auth" | module: "auth" | deps: "db" | priority: "high""#;
        assert_eq!(extract_field(line, "task:"), Some("implement auth".into()));
        assert_eq!(extract_field(line, "module:"), Some("auth".into()));
        assert_eq!(extract_field(line, "deps:"), Some("db".into()));
        assert_eq!(extract_field(line, "priority:"), Some("high".into()));
    }

    #[test]
    fn test_parse_agent_requests() {
        let synthesis = r#"
CONSENSUS: PARTIAL

We need more expertise.
AGENT_NEEDED: module="payments" reason="Payment integration required"
AGENT_NEEDED: module="security" reason="Auth review needed"
"#;

        let requests = ConsensusEngine::parse_agent_requests(synthesis);
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].module, "payments");
        assert_eq!(requests[1].module, "security");
    }

    #[test]
    fn test_conflict_severity() {
        let conflict = Conflict {
            id: "c1".into(),
            agents: vec!["a1".into(), "a2".into()],
            topic: "API design".into(),
            positions: vec!["REST".into(), "GraphQL".into()],
            severity: ConflictSeverity::Major,
            resolution: None,
        };

        assert_eq!(conflict.severity, ConflictSeverity::Major);
    }

    #[test]
    fn test_consensus_task_serialization() {
        let task = ConsensusTask {
            id: "task-1".into(),
            description: "Implement auth".into(),
            assigned_module: Some("auth".into()),
            dependencies: vec!["task-0".into()],
            priority: TaskPriority::High,
            estimated_complexity: TaskComplexity::Medium,
            files_affected: vec!["src/auth.rs".into()],
            priority_score: 0.8,
        };

        let json = serde_json::to_string(&task).unwrap();
        let parsed: ConsensusTask = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, "task-1");
        assert_eq!(parsed.estimated_complexity, TaskComplexity::Medium);
    }

    #[test]
    fn test_synthesis_output_parsing() {
        let json = r#"{
            "consensus": "yes",
            "plan": "Implement the feature",
            "tasks": [
                {
                    "id": "task-1",
                    "description": "Add endpoint",
                    "assigned_module": "api",
                    "dependencies": [],
                    "priority": "high",
                    "estimated_complexity": "medium",
                    "files_affected": ["src/api.rs"]
                }
            ],
            "conflicts": [],
            "dissents": [],
            "agent_needed": []
        }"#;

        let output: ConsensusSynthesisOutput = serde_json::from_str(json).unwrap();
        assert_eq!(output.consensus, ConsensusStatus::Yes);
        assert_eq!(output.tasks.len(), 1);
        assert_eq!(output.tasks[0].estimated_complexity, TaskComplexity::Medium);
    }

    #[test]
    fn test_agent_performance_history() {
        let mut history = AgentPerformanceHistory::default();
        assert_eq!(history.accuracy_rate(), 0.5); // Default for new agents

        history.total_proposals = 10;
        history.accepted_proposals = 8;
        assert!((history.accuracy_rate() - 0.8).abs() < 0.001);

        history.module_contributions.insert("auth".into(), 5);
        history.module_contributions.insert("api".into(), 5);
        assert!((history.module_strength("auth") - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_needs_additional_agents() {
        assert!(ConsensusEngine::needs_additional_agents(
            "AGENT_NEEDED: module=\"x\""
        ));
        assert!(ConsensusEngine::needs_additional_agents(
            "Need module expert"
        ));
        assert!(ConsensusEngine::needs_additional_agents(
            "require specialist"
        ));
        assert!(!ConsensusEngine::needs_additional_agents("All good"));
    }

    #[test]
    fn test_consensus_task_workspace_extraction() {
        // Unqualified module - workspace() should return None
        let task1 = ConsensusTask::new("t1", "Task 1").with_module("auth");
        assert!(task1.workspace().is_none());
        assert!(!task1.is_qualified());
        assert_eq!(task1.module_name(), Some("auth"));

        // Qualified module - workspace() should return workspace
        let task2 = ConsensusTask::new("t2", "Task 2").with_qualified_module("project-a", "auth");
        assert_eq!(task2.workspace(), Some("project-a"));
        assert!(task2.is_qualified());
        assert_eq!(task2.module_name(), Some("auth"));

        // Full qualification with domain
        let task3 = ConsensusTask::new("t3", "Task 3").with_full_qualification(
            "project-a",
            "security",
            "auth",
        );
        assert_eq!(task3.workspace(), Some("project-a"));
        assert!(task3.is_qualified());
        assert_eq!(task3.module_name(), Some("auth"));
        assert!(task3.is_in_workspace("project-a"));
        assert!(!task3.is_in_workspace("project-b"));

        // Parse qualified should work
        let parsed = task3.parse_qualified();
        assert!(parsed.is_some());
        let qm = parsed.unwrap();
        assert_eq!(qm.workspace, "project-a");
        assert_eq!(qm.domain, Some("security".to_string()));
        assert_eq!(qm.module, "auth");
    }

    #[test]
    fn test_conflict_from_description_severity() {
        // Critical/Blocking severity
        let c1 = Conflict::from_description("c1", "Critical API breaking change in auth module");
        assert_eq!(c1.severity, ConflictSeverity::Blocking);

        // Major severity
        let c2 = Conflict::from_description("c2", "Type mismatch between User and UserDTO");
        assert_eq!(c2.severity, ConflictSeverity::Major);

        // Moderate severity
        let c3 = Conflict::from_description("c3", "API naming conflict in endpoints");
        assert_eq!(c3.severity, ConflictSeverity::Moderate);

        // Minor severity (default)
        let c4 = Conflict::from_description("c4", "Style preference in naming");
        assert_eq!(c4.severity, ConflictSeverity::Minor);
    }

    #[test]
    fn test_conflict_extract_agents_between_pattern() {
        let c = Conflict::from_description("c1", "API conflict between auth and api modules");
        assert_eq!(c.agents.len(), 2);
        assert!(c.agents.contains(&"auth".to_string()));
        assert!(c.agents.contains(&"api".to_string()));
    }

    #[test]
    fn test_conflict_extract_agents_vs_pattern() {
        let c = Conflict::from_description("c1", "auth vs api naming conflict");
        assert_eq!(c.agents.len(), 2);
        assert!(c.agents.contains(&"auth".to_string()));
        assert!(c.agents.contains(&"api".to_string()));
    }

    #[test]
    fn test_conflict_from_descriptions_batch() {
        let descriptions = vec![
            "Critical breaking change".to_string(),
            "Minor style issue".to_string(),
            "Type mismatch in UserDTO".to_string(),
        ];

        let conflicts = Conflict::from_descriptions(&descriptions);
        assert_eq!(conflicts.len(), 3);
        assert_eq!(conflicts[0].id, "conflict-0");
        assert_eq!(conflicts[0].severity, ConflictSeverity::Blocking);
        assert_eq!(conflicts[1].severity, ConflictSeverity::Minor);
        assert_eq!(conflicts[2].severity, ConflictSeverity::Major);
    }
}
