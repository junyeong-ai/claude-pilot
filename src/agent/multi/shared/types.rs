//! Shared types for multi-agent orchestration.
//!
//! This module contains foundation types used across the multi-agent system,
//! including agent identification, consensus outcomes, tier results, and
//! cross-workspace coordination types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn research(instance: usize) -> Self {
        Self(format!("research-{instance}"))
    }

    pub fn planning(instance: usize) -> Self {
        Self(format!("planning-{instance}"))
    }

    pub fn coder(instance: usize) -> Self {
        Self(format!("coder-{instance}"))
    }

    pub fn verifier(instance: usize) -> Self {
        Self(format!("verifier-{instance}"))
    }

    pub fn reviewer(instance: usize) -> Self {
        Self(format!("reviewer-{instance}"))
    }

    pub fn architect(instance: usize) -> Self {
        Self(format!("architect-{instance}"))
    }

    pub fn module(module_id: &str) -> Self {
        Self(format!("module-{module_id}"))
    }

    pub fn module_with_instance(name: &str, instance: usize) -> Self {
        Self(format!("module-{}-{}", name.to_lowercase(), instance))
    }

    pub fn architecture(instance: usize) -> Self {
        Self(format!("architecture-{instance}"))
    }

    pub fn core(role: &str, instance: usize) -> Self {
        Self(format!("{role}-{instance}"))
    }

    pub fn group_coordinator(group_id: &str) -> Self {
        Self(format!("group-{group_id}"))
    }

    pub fn domain_coordinator(domain_id: &str) -> Self {
        Self(format!("domain-{domain_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoteDecision {
    Approve,
    ApproveWithChanges,
    Reject,
    Abstain,
}

impl VoteDecision {
    pub fn is_positive(&self) -> bool {
        matches!(self, Self::Approve | Self::ApproveWithChanges)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::ApproveWithChanges => "approve_with_changes",
            Self::Reject => "reject",
            Self::Abstain => "abstain",
        }
    }
}

impl fmt::Display for VoteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSeverity {
    Minor,
    Moderate,
    Major,
    Blocking,
}

impl ConflictSeverity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Moderate => "moderate",
            Self::Major => "major",
            Self::Blocking => "blocking",
        }
    }

    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Blocking)
    }
}

impl fmt::Display for ConflictSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

impl TaskPriority {
    pub fn weight(&self) -> f64 {
        match self {
            Self::Low => 0.25,
            Self::Normal => 0.5,
            Self::High => 0.75,
            Self::Critical => 1.0,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Trivial,
    #[default]
    Low,
    Medium,
    High,
    Complex,
}

impl TaskComplexity {
    pub fn weight(&self) -> f64 {
        match self {
            Self::Trivial => 0.2,
            Self::Low => 0.4,
            Self::Medium => 0.6,
            Self::High => 0.8,
            Self::Complex => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierLevel {
    /// Single module scope
    Module,
    /// Multiple modules in same group
    Group,
    /// Multiple groups in same domain
    Domain,
    /// Entire workspace
    Workspace,
    /// Multiple workspaces (cross-project coordination)
    CrossWorkspace,
}

impl TierLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Group => "group",
            Self::Domain => "domain",
            Self::Workspace => "workspace",
            Self::CrossWorkspace => "cross_workspace",
        }
    }

    pub fn order(&self) -> u8 {
        match self {
            Self::Module => 0,
            Self::Group => 1,
            Self::Domain => 2,
            Self::Workspace => 3,
            Self::CrossWorkspace => 4,
        }
    }

    pub fn parent(&self) -> Option<Self> {
        match self {
            Self::Module => Some(Self::Group),
            Self::Group => Some(Self::Domain),
            Self::Domain => Some(Self::Workspace),
            Self::Workspace => Some(Self::CrossWorkspace),
            Self::CrossWorkspace => None,
        }
    }

    /// Returns the escalation order from lowest to highest tier.
    pub fn escalation_order() -> &'static [TierLevel] {
        &[
            TierLevel::Module,
            TierLevel::Group,
            TierLevel::Domain,
            TierLevel::Workspace,
            TierLevel::CrossWorkspace,
        ]
    }

    /// Returns the next tier level for escalation.
    pub fn next(&self) -> Option<TierLevel> {
        self.parent()
    }
}

impl fmt::Display for TierLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundOutcome {
    Approved,
    NeedsMoreRounds,
    Failed,
    Escalated,
}

impl RoundOutcome {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Approved | Self::Failed | Self::Escalated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleCategory {
    Core,
    Module,
    Reviewer,
    Architecture,
    Advisor,
}

impl fmt::Display for RoleCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core => write!(f, "core"),
            Self::Module => write!(f, "module"),
            Self::Reviewer => write!(f, "reviewer"),
            Self::Architecture => write!(f, "architecture"),
            Self::Advisor => write!(f, "advisor"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    #[default]
    ReadOnly,
    FileModify,
    VerifyExecute,
}

impl PermissionProfile {
    pub fn can_modify_files(&self) -> bool {
        matches!(self, Self::FileModify | Self::VerifyExecute)
    }

    pub fn can_execute(&self) -> bool {
        matches!(self, Self::VerifyExecute)
    }
}

/// Outcome of a consensus session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsensusOutcome {
    Converged,
    PartialConvergence,
    Escalated,
    Timeout,
    Failed,
}

impl ConsensusOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Converged => "converged",
            Self::PartialConvergence => "partial",
            Self::Escalated => "escalated",
            Self::Timeout => "timeout",
            Self::Failed => "failed",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Converged | Self::PartialConvergence)
    }

    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::PartialConvergence)
    }
}

impl fmt::Display for ConsensusOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Result of a single tier consensus unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierResult {
    pub tier_level: TierLevel,
    pub unit_id: String,
    pub converged: bool,
    pub synthesis: String,
    pub respondent_count: usize,
    pub conflicts: Vec<String>,
    pub timed_out: bool,
    /// Number of consensus rounds executed within this unit.
    pub rounds: usize,
}

impl TierResult {
    pub fn success(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        synthesis: impl Into<String>,
        respondent_count: usize,
        rounds: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: true,
            synthesis: synthesis.into(),
            respondent_count,
            conflicts: Vec::new(),
            timed_out: false,
            rounds,
        }
    }

    pub fn failure(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        synthesis: impl Into<String>,
        respondent_count: usize,
        conflicts: Vec<String>,
        rounds: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: false,
            synthesis: synthesis.into(),
            respondent_count,
            conflicts,
            timed_out: false,
            rounds,
        }
    }

    pub fn timeout(
        tier_level: TierLevel,
        unit_id: impl Into<String>,
        respondent_count: usize,
    ) -> Self {
        Self {
            tier_level,
            unit_id: unit_id.into(),
            converged: false,
            synthesis: "Timed out - workspace may be unavailable".to_string(),
            respondent_count,
            conflicts: vec!["timeout".to_string()],
            timed_out: true,
            rounds: 0,
        }
    }
}

/// Status of a consensus session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Checkpointed,
    Completed,
    Escalated,
    Failed,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Checkpointed => "checkpointed",
            Self::Completed => "completed",
            Self::Escalated => "escalated",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Escalated | Self::Failed)
    }
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ============================================================================
// Cross-Workspace Types
// ============================================================================

/// Qualified module identifier for cross-workspace consensus.
/// Format: "workspace::domain::module" (e.g., "project-a::auth::api")
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedModule {
    pub workspace: String,
    pub domain: Option<String>,
    pub module: String,
}

impl QualifiedModule {
    pub fn new(workspace: impl Into<String>, module: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            domain: None,
            module: module.into(),
        }
    }

    pub fn with_domain(
        workspace: impl Into<String>,
        domain: impl Into<String>,
        module: impl Into<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            domain: Some(domain.into()),
            module: module.into(),
        }
    }

    pub fn from_qualified_string(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split("::").collect();
        match parts.len() {
            2 => Some(Self::new(parts[0], parts[1])),
            3 => Some(Self::with_domain(parts[0], parts[1], parts[2])),
            _ => None,
        }
    }

    pub fn to_qualified_string(&self) -> String {
        match &self.domain {
            Some(d) => format!("{}::{}::{}", self.workspace, d, self.module),
            None => format!("{}::{}", self.workspace, self.module),
        }
    }
}

impl fmt::Display for QualifiedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_qualified_string())
    }
}

// ============================================================================
// Consensus Phase Types
// ============================================================================

/// Quorum requirement for consensus units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Quorum {
    /// Simple majority (>50%)
    #[default]
    Majority,
    /// Supermajority (≥2/3)
    Supermajority,
    /// All participants must agree
    Unanimous,
    /// Unanimous with fallback to supermajority
    UnanimousWithFallback,
}

impl Quorum {
    pub fn threshold(&self, total: usize) -> usize {
        match self {
            Self::Majority => total / 2 + 1,
            Self::Supermajority => (total * 2).div_ceil(3),
            Self::Unanimous | Self::UnanimousWithFallback => total,
        }
    }

    pub fn is_met(&self, votes: usize, total: usize) -> bool {
        votes >= self.threshold(total)
    }
}

impl From<crate::config::QuorumType> for Quorum {
    fn from(qt: crate::config::QuorumType) -> Self {
        match qt {
            crate::config::QuorumType::Majority => Quorum::Majority,
            crate::config::QuorumType::Supermajority => Quorum::Supermajority,
            crate::config::QuorumType::Unanimous => Quorum::Unanimous,
            crate::config::QuorumType::UnanimousWithFallback => Quorum::UnanimousWithFallback,
        }
    }
}

// ============================================================================
// API and Type Change Tracking
// ============================================================================

/// Represents a change to an API (function, method, endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiChange {
    pub api_name: String,
    pub change_type: ApiChangeType,
    pub old_signature: Option<String>,
    pub new_signature: Option<String>,
    pub module: Option<QualifiedModule>,
}

impl ApiChange {
    pub fn add(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::Add,
            old_signature: None,
            new_signature: Some(signature.into()),
            module: None,
        }
    }

    pub fn remove(name: impl Into<String>, signature: impl Into<String>) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::Remove,
            old_signature: Some(signature.into()),
            new_signature: None,
            module: None,
        }
    }

    pub fn rename(old_name: impl Into<String>, new_name: impl Into<String>) -> Self {
        Self {
            api_name: new_name.into(),
            change_type: ApiChangeType::Rename {
                old_name: old_name.into(),
            },
            old_signature: None,
            new_signature: None,
            module: None,
        }
    }

    pub fn modify(
        name: impl Into<String>,
        old_sig: impl Into<String>,
        new_sig: impl Into<String>,
    ) -> Self {
        Self {
            api_name: name.into(),
            change_type: ApiChangeType::ModifySignature,
            old_signature: Some(old_sig.into()),
            new_signature: Some(new_sig.into()),
            module: None,
        }
    }

    pub fn with_module(mut self, module: QualifiedModule) -> Self {
        self.module = Some(module);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApiChangeType {
    Add,
    Remove,
    Rename { old_name: String },
    ModifySignature,
}

/// Represents a change to a type (struct, enum, interface).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeChange {
    pub type_name: String,
    pub change_type: TypeChangeType,
    pub details: String,
    pub module: Option<QualifiedModule>,
}

impl TypeChange {
    pub fn add_field(type_name: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::AddField,
            details: field.into(),
            module: None,
        }
    }

    pub fn remove_field(type_name: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::RemoveField,
            details: field.into(),
            module: None,
        }
    }

    pub fn change_field_type(type_name: impl Into<String>, change: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            change_type: TypeChangeType::ChangeFieldType,
            details: change.into(),
            module: None,
        }
    }

    pub fn with_module(mut self, module: QualifiedModule) -> Self {
        self.module = Some(module);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TypeChangeType {
    AddField,
    RemoveField,
    ChangeFieldType,
    RenameField { old_name: String },
    AddVariant,
    RemoveVariant,
}

// ============================================================================
// Enhanced TierResult
// ============================================================================

/// Extended tier result with semantic information for upper tiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedTierResult {
    pub base: TierResult,
    pub tasks: Vec<TierTask>,
    pub api_changes: Vec<ApiChange>,
    pub type_changes: Vec<TypeChange>,
    pub constraints_applied: Vec<String>,
}

impl EnhancedTierResult {
    pub fn from_base(base: TierResult) -> Self {
        Self {
            base,
            tasks: Vec::new(),
            api_changes: Vec::new(),
            type_changes: Vec::new(),
            constraints_applied: Vec::new(),
        }
    }

    pub fn with_tasks(mut self, tasks: Vec<TierTask>) -> Self {
        self.tasks = tasks;
        self
    }

    pub fn with_api_changes(mut self, changes: Vec<ApiChange>) -> Self {
        self.api_changes = changes;
        self
    }

    pub fn with_type_changes(mut self, changes: Vec<TypeChange>) -> Self {
        self.type_changes = changes;
        self
    }

    pub fn with_constraints(mut self, constraints: Vec<String>) -> Self {
        self.constraints_applied = constraints;
        self
    }

    /// Check if this tier result converged successfully.
    pub fn converged(&self) -> bool {
        self.base.converged
    }

    /// Get the tier level.
    pub fn tier_level(&self) -> TierLevel {
        self.base.tier_level
    }

    /// Get the unit ID.
    pub fn unit_id(&self) -> &str {
        &self.base.unit_id
    }

    /// Get all workspaces involved in the tasks.
    pub fn involved_workspaces(&self) -> Vec<&str> {
        let mut workspaces: Vec<&str> = self.tasks.iter().map(|t| t.workspace()).collect();
        workspaces.sort();
        workspaces.dedup();
        workspaces
    }

    /// Check if tasks span multiple workspaces.
    pub fn is_cross_workspace(&self) -> bool {
        self.involved_workspaces().len() > 1
    }

    /// Get tasks grouped by workspace.
    pub fn tasks_by_workspace(&self) -> HashMap<&str, Vec<&TierTask>> {
        let mut grouped: HashMap<&str, Vec<&TierTask>> = HashMap::new();
        for task in &self.tasks {
            grouped.entry(task.workspace()).or_default().push(task);
        }
        grouped
    }

    /// Check if there are any API changes.
    pub fn has_api_changes(&self) -> bool {
        !self.api_changes.is_empty()
    }

    /// Check if there are any type changes.
    pub fn has_type_changes(&self) -> bool {
        !self.type_changes.is_empty()
    }

    /// Get a summary for upper tier context building.
    pub fn summary_for_upper_tier(&self) -> String {
        format!(
            "Unit '{}' ({:?}): {} tasks, {} API changes, {} type changes. Converged: {}",
            self.base.unit_id,
            self.base.tier_level,
            self.tasks.len(),
            self.api_changes.len(),
            self.type_changes.len(),
            self.base.converged
        )
    }
}

/// Task extracted from a consensus tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierTask {
    pub id: String,
    pub description: String,
    pub assigned_module: QualifiedModule,
    pub dependencies: Vec<String>,
    pub priority: TaskPriority,
    pub complexity: TaskComplexity,
    pub files_affected: Vec<String>,
}

impl TierTask {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        module: QualifiedModule,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            assigned_module: module,
            dependencies: Vec::new(),
            priority: TaskPriority::Normal,
            complexity: TaskComplexity::Medium,
            files_affected: Vec::new(),
        }
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn with_priority(mut self, priority: TaskPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_complexity(mut self, complexity: TaskComplexity) -> Self {
        self.complexity = complexity;
        self
    }

    pub fn with_files(mut self, files: Vec<String>) -> Self {
        self.files_affected = files;
        self
    }

    /// Try to create a TierTask from a qualified module string.
    ///
    /// Returns `None` if the string is not in qualified format (workspace::module or workspace::domain::module).
    ///
    /// # Example
    /// ```ignore
    /// let task = TierTask::try_from_qualified(
    ///     "task-1",
    ///     "Implement auth",
    ///     "project-a::auth::login"
    /// );
    /// assert!(task.is_some());
    /// ```
    pub fn try_from_qualified(
        id: impl Into<String>,
        description: impl Into<String>,
        qualified_module: &str,
    ) -> Option<Self> {
        QualifiedModule::from_qualified_string(qualified_module)
            .map(|module| Self::new(id, description, module))
    }

    /// Get the workspace from the assigned module.
    pub fn workspace(&self) -> &str {
        &self.assigned_module.workspace
    }

    /// Get the domain from the assigned module (if present).
    pub fn domain(&self) -> Option<&str> {
        self.assigned_module.domain.as_deref()
    }

    /// Get the full qualified path as a string.
    pub fn qualified_path(&self) -> String {
        self.assigned_module.to_string()
    }
}

// ============================================================================
// Global Constraints (for Phase 1 Direction Setting)
// ============================================================================

/// Global constraints established during Phase 1 direction setting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConstraints {
    pub tech_decisions: Vec<TechDecision>,
    pub api_contracts: Vec<ApiContract>,
    pub workspace_refinements: HashMap<String, Vec<String>>,
    pub runtime_constraints: Vec<RuntimeConstraint>,
}

impl GlobalConstraints {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_tech_decision(&mut self, decision: TechDecision) {
        self.tech_decisions.push(decision);
    }

    pub fn add_api_contract(&mut self, contract: ApiContract) {
        self.api_contracts.push(contract);
    }

    pub fn add_workspace_refinement(
        &mut self,
        workspace: impl Into<String>,
        refinement: impl Into<String>,
    ) {
        self.workspace_refinements
            .entry(workspace.into())
            .or_default()
            .push(refinement.into());
    }

    pub fn add_runtime_constraint(&mut self, constraint: RuntimeConstraint) {
        self.runtime_constraints.push(constraint);
    }

    pub fn get_refinements(&self, workspace: &str) -> Vec<&str> {
        self.workspace_refinements
            .get(workspace)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }
}

/// Technology decision that applies to all modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechDecision {
    pub topic: String,
    pub decision: String,
    pub rationale: String,
    pub affected_modules: Vec<QualifiedModule>,
}

impl TechDecision {
    pub fn new(topic: impl Into<String>, decision: impl Into<String>) -> Self {
        Self {
            topic: topic.into(),
            decision: decision.into(),
            rationale: String::new(),
            affected_modules: Vec::new(),
        }
    }

    pub fn with_rationale(mut self, rationale: impl Into<String>) -> Self {
        self.rationale = rationale.into();
        self
    }

    pub fn with_affected_modules(mut self, modules: Vec<QualifiedModule>) -> Self {
        self.affected_modules = modules;
        self
    }
}

/// API contract that must be satisfied across modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiContract {
    pub name: String,
    pub provider: QualifiedModule,
    pub consumers: Vec<QualifiedModule>,
    pub specification: String,
}

impl ApiContract {
    pub fn new(name: impl Into<String>, provider: QualifiedModule) -> Self {
        Self {
            name: name.into(),
            provider,
            consumers: Vec::new(),
            specification: String::new(),
        }
    }

    pub fn with_consumers(mut self, consumers: Vec<QualifiedModule>) -> Self {
        self.consumers = consumers;
        self
    }

    pub fn with_specification(mut self, spec: impl Into<String>) -> Self {
        self.specification = spec.into();
        self
    }
}

/// Runtime constraint generated during backtracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeConstraint {
    pub source: String,
    pub constraint_type: ConstraintType,
    pub description: String,
    pub affected_modules: Vec<QualifiedModule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintType {
    ApiNaming,
    TypeDefinition,
    DependencyOrder,
    TechnologyChoice,
}

// ============================================================================
// Semantic Conflict Types
// ============================================================================

/// Conflict detected through semantic analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticConflict {
    pub conflict_type: SemanticConflictType,
    pub agents_involved: Vec<String>,
    pub description: String,
    pub severity: ConflictSeverity,
}

impl SemanticConflict {
    pub fn api_rename(
        old_name: impl Into<String>,
        new_names: Vec<(String, String)>, // (agent, proposed_name)
    ) -> Self {
        let agents: Vec<String> = new_names.iter().map(|(a, _)| a.clone()).collect();
        let names: Vec<&str> = new_names.iter().map(|(_, n)| n.as_str()).collect();
        Self {
            conflict_type: SemanticConflictType::ApiRename {
                old_name: old_name.into(),
                proposed_names: names.iter().map(|s| s.to_string()).collect(),
            },
            agents_involved: agents,
            description: format!("Conflicting API rename proposals: {:?}", names),
            severity: ConflictSeverity::Major,
        }
    }

    pub fn type_change(type_name: impl Into<String>, changes: Vec<(String, String)>) -> Self {
        let agents: Vec<String> = changes.iter().map(|(a, _)| a.clone()).collect();
        Self {
            conflict_type: SemanticConflictType::TypeChange {
                type_name: type_name.into(),
                changes: changes.into_iter().map(|(_, c)| c).collect(),
            },
            agents_involved: agents,
            description: "Conflicting type modifications".to_string(),
            severity: ConflictSeverity::Major,
        }
    }

    pub fn dependency_mismatch(provider: impl Into<String>, consumer: impl Into<String>) -> Self {
        Self {
            conflict_type: SemanticConflictType::DependencyMismatch {
                provider: provider.into(),
                consumer: consumer.into(),
            },
            agents_involved: Vec::new(),
            description: "Provider/consumer dependency mismatch".to_string(),
            severity: ConflictSeverity::Blocking,
        }
    }

    pub fn is_blocking(&self) -> bool {
        self.severity == ConflictSeverity::Blocking
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticConflictType {
    ApiRename {
        old_name: String,
        proposed_names: Vec<String>,
    },
    TypeChange {
        type_name: String,
        changes: Vec<String>,
    },
    DependencyMismatch {
        provider: String,
        consumer: String,
    },
    TechDecisionConflict {
        topic: String,
        decisions: Vec<String>,
    },
}

// ============================================================================
// Convergence Result
// ============================================================================

/// Result of semantic convergence checking.
#[derive(Debug, Clone)]
pub enum ConvergenceCheckResult {
    Converged {
        score: f64,
        minor_conflicts: Vec<SemanticConflict>,
    },
    Blocked {
        conflicts: Vec<SemanticConflict>,
    },
    NeedsMoreRounds {
        current_score: f64,
        conflicts: Vec<SemanticConflict>,
    },
}

impl ConvergenceCheckResult {
    pub fn is_converged(&self) -> bool {
        matches!(self, Self::Converged { .. })
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }

    pub fn conflicts(&self) -> &[SemanticConflict] {
        match self {
            Self::Converged {
                minor_conflicts, ..
            } => minor_conflicts,
            Self::Blocked { conflicts } | Self::NeedsMoreRounds { conflicts, .. } => conflicts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_id_formats() {
        assert_eq!(AgentId::module("auth").as_str(), "module-auth");
        assert_eq!(
            AgentId::module_with_instance("Auth", 0).as_str(),
            "module-auth-0"
        );
        assert_eq!(
            AgentId::module_with_instance("user-service", 1).as_str(),
            "module-user-service-1"
        );
        assert_eq!(AgentId::group_coordinator("core").as_str(), "group-core");
        assert_eq!(
            AgentId::domain_coordinator("security").as_str(),
            "domain-security"
        );
        assert_eq!(AgentId::research(0).as_str(), "research-0");
        assert_eq!(AgentId::reviewer(0).as_str(), "reviewer-0");
        assert_eq!(AgentId::architecture(0).as_str(), "architecture-0");
        assert_eq!(AgentId::core("planning", 0).as_str(), "planning-0");
        let id: String = AgentId::coder(1).into();
        assert_eq!(id, "coder-1");
    }

    #[test]
    fn test_vote_decision() {
        assert!(VoteDecision::Approve.is_positive());
        assert!(VoteDecision::ApproveWithChanges.is_positive());
        assert!(!VoteDecision::Reject.is_positive());
        assert!(!VoteDecision::Abstain.is_positive());
    }

    #[test]
    fn test_tier_level_order() {
        assert!(TierLevel::Module.order() < TierLevel::Group.order());
        assert!(TierLevel::Group.order() < TierLevel::Domain.order());
        assert!(TierLevel::Domain.order() < TierLevel::Workspace.order());
        assert!(TierLevel::Workspace.order() < TierLevel::CrossWorkspace.order());
    }

    #[test]
    fn test_tier_level_parent() {
        assert_eq!(TierLevel::Module.parent(), Some(TierLevel::Group));
        assert_eq!(TierLevel::Group.parent(), Some(TierLevel::Domain));
        assert_eq!(TierLevel::Domain.parent(), Some(TierLevel::Workspace));
        assert_eq!(
            TierLevel::Workspace.parent(),
            Some(TierLevel::CrossWorkspace)
        );
        assert_eq!(TierLevel::CrossWorkspace.parent(), None);
    }

    #[test]
    fn test_tier_level_escalation() {
        let order = TierLevel::escalation_order();
        assert_eq!(order.len(), 5);
        assert_eq!(order[0], TierLevel::Module);
        assert_eq!(order[4], TierLevel::CrossWorkspace);
    }

    #[test]
    fn test_consensus_outcome() {
        assert_eq!(ConsensusOutcome::Converged.as_str(), "converged");
        assert_eq!(ConsensusOutcome::PartialConvergence.as_str(), "partial");
        assert_eq!(ConsensusOutcome::Escalated.as_str(), "escalated");
        assert_eq!(ConsensusOutcome::Timeout.as_str(), "timeout");
        assert_eq!(ConsensusOutcome::Failed.as_str(), "failed");

        assert!(ConsensusOutcome::Converged.is_success());
        assert!(ConsensusOutcome::PartialConvergence.is_success());
        assert!(!ConsensusOutcome::Failed.is_success());

        assert!(ConsensusOutcome::Converged.is_terminal());
        assert!(!ConsensusOutcome::PartialConvergence.is_terminal());
    }

    #[test]
    fn test_tier_result_constructors() {
        let success = TierResult::success(TierLevel::Module, "unit-1", "Synthesis", 3, 2);
        assert!(success.converged);
        assert!(!success.timed_out);
        assert_eq!(success.respondent_count, 3);
        assert_eq!(success.rounds, 2);

        let failure = TierResult::failure(
            TierLevel::Group,
            "unit-2",
            "Failed",
            2,
            vec!["conflict".to_string()],
            1,
        );
        assert!(!failure.converged);
        assert!(!failure.timed_out);
        assert_eq!(failure.respondent_count, 2);
        assert_eq!(failure.conflicts.len(), 1);
        assert_eq!(failure.rounds, 1);

        let timeout = TierResult::timeout(TierLevel::Domain, "unit-3", 5);
        assert!(!timeout.converged);
        assert!(timeout.timed_out);
        assert_eq!(timeout.respondent_count, 5);
    }

    #[test]
    fn test_session_status() {
        assert_eq!(SessionStatus::Running.as_str(), "running");
        assert_eq!(SessionStatus::Completed.as_str(), "completed");

        assert!(!SessionStatus::Running.is_terminal());
        assert!(!SessionStatus::Checkpointed.is_terminal());
        assert!(SessionStatus::Completed.is_terminal());
        assert!(SessionStatus::Escalated.is_terminal());
        assert!(SessionStatus::Failed.is_terminal());
    }

    #[test]
    fn test_qualified_module() {
        let qm = QualifiedModule::new("project-a", "auth");
        assert_eq!(qm.to_qualified_string(), "project-a::auth");

        let qm2 = QualifiedModule::with_domain("project-a", "security", "auth");
        assert_eq!(qm2.to_qualified_string(), "project-a::security::auth");

        let parsed = QualifiedModule::from_qualified_string("ws::mod").unwrap();
        assert_eq!(parsed.workspace, "ws");
        assert_eq!(parsed.module, "mod");
        assert!(parsed.domain.is_none());

        let parsed2 = QualifiedModule::from_qualified_string("ws::dom::mod").unwrap();
        assert_eq!(parsed2.domain.as_deref(), Some("dom"));
    }

    #[test]
    fn test_quorum() {
        assert_eq!(Quorum::Majority.threshold(5), 3);
        assert_eq!(Quorum::Supermajority.threshold(5), 4);
        assert_eq!(Quorum::Unanimous.threshold(5), 5);

        assert!(Quorum::Majority.is_met(3, 5));
        assert!(!Quorum::Majority.is_met(2, 5));
        assert!(Quorum::Supermajority.is_met(4, 5));
    }

    #[test]
    fn test_api_change() {
        let add = ApiChange::add("validate", "fn validate(token: &str) -> bool");
        assert_eq!(add.change_type, ApiChangeType::Add);
        assert!(add.new_signature.is_some());

        let rename = ApiChange::rename("old_name", "new_name");
        assert!(matches!(rename.change_type, ApiChangeType::Rename { .. }));
    }

    #[test]
    fn test_global_constraints() {
        let mut gc = GlobalConstraints::new();
        gc.add_tech_decision(TechDecision::new("auth", "Use JWT"));
        gc.add_workspace_refinement("ws-a", "Frontend uses cookies");

        assert_eq!(gc.tech_decisions.len(), 1);
        assert_eq!(gc.get_refinements("ws-a").len(), 1);
        assert!(gc.get_refinements("ws-b").is_empty());
    }

    #[test]
    fn test_semantic_conflict() {
        let conflict = SemanticConflict::api_rename(
            "validate",
            vec![
                ("agent-1".to_string(), "verify".to_string()),
                ("agent-2".to_string(), "check".to_string()),
            ],
        );
        assert_eq!(conflict.agents_involved.len(), 2);
        assert!(!conflict.is_blocking());

        let blocking = SemanticConflict::dependency_mismatch("provider", "consumer");
        assert!(blocking.is_blocking());
    }

    #[test]
    fn test_convergence_check_result() {
        let converged = ConvergenceCheckResult::Converged {
            score: 0.9,
            minor_conflicts: vec![],
        };
        assert!(converged.is_converged());
        assert!(!converged.is_blocked());

        let blocked = ConvergenceCheckResult::Blocked {
            conflicts: vec![SemanticConflict::dependency_mismatch("a", "b")],
        };
        assert!(!blocked.is_converged());
        assert!(blocked.is_blocked());
        assert_eq!(blocked.conflicts().len(), 1);
    }

    #[test]
    fn test_tier_task_creation() {
        let module = QualifiedModule::with_domain("ws-a", "security", "auth");
        let task = TierTask::new("task-1", "Implement login", module)
            .with_dependencies(vec!["task-0".to_string()])
            .with_priority(TaskPriority::High)
            .with_complexity(TaskComplexity::Medium)
            .with_files(vec!["src/auth/login.rs".to_string()]);

        assert_eq!(task.id, "task-1");
        assert_eq!(task.workspace(), "ws-a");
        assert_eq!(task.domain(), Some("security"));
        assert_eq!(task.qualified_path(), "ws-a::security::auth");
        assert_eq!(task.dependencies.len(), 1);
        assert_eq!(task.priority, TaskPriority::High);
    }

    #[test]
    fn test_tier_task_try_from_qualified() {
        // Valid qualified strings
        let task1 = TierTask::try_from_qualified("t1", "Test", "ws::mod");
        assert!(task1.is_some());
        let task1 = task1.unwrap();
        assert_eq!(task1.workspace(), "ws");
        assert!(task1.domain().is_none());

        let task2 = TierTask::try_from_qualified("t2", "Test", "ws::dom::mod");
        assert!(task2.is_some());
        assert_eq!(task2.unwrap().domain(), Some("dom"));

        // Invalid - not qualified (no ::)
        let task3 = TierTask::try_from_qualified("t3", "Test", "simple-module");
        assert!(task3.is_none());

        // Invalid - too many parts
        let task4 = TierTask::try_from_qualified("t4", "Test", "a::b::c::d");
        assert!(task4.is_none());
    }

    #[test]
    fn test_enhanced_tier_result() {
        let base = TierResult::success(TierLevel::Module, "module-collective", "Synthesis", 5, 2);
        let enhanced = EnhancedTierResult::from_base(base)
            .with_tasks(vec![
                TierTask::new("t1", "Task 1", QualifiedModule::new("ws-a", "auth")),
                TierTask::new("t2", "Task 2", QualifiedModule::new("ws-a", "api")),
                TierTask::new("t3", "Task 3", QualifiedModule::new("ws-b", "backend")),
            ])
            .with_api_changes(vec![ApiChange::add("validate", "fn validate() -> bool")])
            .with_constraints(vec!["Use JWT".to_string()]);

        assert!(enhanced.converged());
        assert_eq!(enhanced.tier_level(), TierLevel::Module);
        assert_eq!(enhanced.unit_id(), "module-collective");
        assert!(enhanced.has_api_changes());
        assert!(!enhanced.has_type_changes());
        assert!(enhanced.is_cross_workspace());

        let workspaces = enhanced.involved_workspaces();
        assert_eq!(workspaces.len(), 2);
        assert!(workspaces.contains(&"ws-a"));
        assert!(workspaces.contains(&"ws-b"));

        let grouped = enhanced.tasks_by_workspace();
        assert_eq!(grouped.get("ws-a").map(|v| v.len()), Some(2));
        assert_eq!(grouped.get("ws-b").map(|v| v.len()), Some(1));
    }

    #[test]
    fn test_enhanced_tier_result_summary() {
        let base = TierResult::success(TierLevel::Domain, "domain-b", "Synthesized", 3, 1);
        let enhanced = EnhancedTierResult::from_base(base).with_tasks(vec![TierTask::new(
            "t1",
            "Task",
            QualifiedModule::new("ws", "mod"),
        )]);

        let summary = enhanced.summary_for_upper_tier();
        assert!(summary.contains("domain-b"));
        assert!(summary.contains("Domain"));
        assert!(summary.contains("1 tasks"));
        assert!(summary.contains("Converged: true"));
    }
}
