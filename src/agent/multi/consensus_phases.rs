//! 3-Phase Consensus Architecture
//!
//! Phase 1 (Direction Setting): Expert analysis -> GlobalConstraints
//! - Actor: TaskAgent aggregator (single expert)
//! - Output: Tech decisions, API contracts, runtime constraints
//!
//! Phase 2 (Planning Consensus): Planning agents -> Agreed plan + tasks
//! - Actor: PLANNING AGENTS ONLY (filtered via role_filter: "planning")
//! - Process: Hierarchical consensus with cross-visibility
//! - Output: HierarchicalConsensusResult { plan, tasks }
//!
//! Phase 3 (Implementation): Coder agents -> Execute tasks
//! - Actor: CODER AGENTS ONLY (routed via to_agent_task())
//! - Input: Vec<ConsensusTask> from Phase 2

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::PermissionProfile;
use super::adaptive_consensus::ConsensusMission;
use super::shared::{
    ApiContract, GlobalConstraints, QualifiedModule, RuntimeConstraint, TechDecision,
};
use super::workspace_registry::WorkspaceRegistry;
use crate::agent::TaskAgent;
use crate::error::Result;

// ============================================================================
// Phase 1: Direction Setting
// ============================================================================

/// Executes Phase 1: Direction Setting (Top-Down).
///
/// Establishes global constraints before module planners begin detailed planning.
pub struct DirectionSettingPhase {
    aggregator: Arc<TaskAgent>,
    workspace_registry: Option<Arc<WorkspaceRegistry>>,
}

impl DirectionSettingPhase {
    pub fn new(aggregator: Arc<TaskAgent>) -> Self {
        Self {
            aggregator,
            workspace_registry: None,
        }
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceRegistry>) -> Self {
        self.workspace_registry = Some(registry);
        self
    }

    /// Execute Phase 1 to establish global constraints.
    pub async fn execute(
        &self,
        mission: &ConsensusMission,
        involved_modules: &[QualifiedModule],
    ) -> Result<GlobalConstraints> {
        info!(
            mission_id = %mission.id,
            modules = involved_modules.len(),
            "Phase 1: Analyzing mission for global constraints"
        );

        // 1. Analyze mission for global decisions
        let analysis_prompt = self.build_analysis_prompt(mission, involved_modules);
        let working_dir = std::env::current_dir().unwrap_or_default();

        let response = self
            .aggregator
            .run_with_profile(&analysis_prompt, &working_dir, PermissionProfile::ReadOnly)
            .await?;

        // 2. Parse constraints from response
        let mut constraints = self.parse_constraints(&response)?;

        // 3. Refine per workspace if registry available
        if let Some(registry) = &self.workspace_registry {
            self.refine_per_workspace(&mut constraints, mission, registry)
                .await?;
        }

        info!(
            tech_decisions = constraints.tech_decisions.len(),
            api_contracts = constraints.api_contracts.len(),
            "Phase 1 complete: constraints established"
        );

        Ok(constraints)
    }

    fn build_analysis_prompt(
        &self,
        mission: &ConsensusMission,
        involved_modules: &[QualifiedModule],
    ) -> String {
        let modules_str: Vec<String> = involved_modules
            .iter()
            .map(|m| m.to_qualified_string())
            .collect();

        format!(
            r#"## Phase 1: Direction Setting Analysis

Analyze this mission that spans multiple modules/workspaces:

**Mission**: {}

**Involved Modules**: {}

**Task**: Identify global constraints that MUST be consistent across all modules.

Identify:
1. **Technology Decisions**: Choices that must be uniform (e.g., "Use JWT for auth", "REST API v2")
2. **API Contracts**: Interfaces that will be shared between modules
3. **Type Ownership**: Which module owns which shared types

Output JSON:
```json
{{
  "tech_decisions": [
    {{"topic": "...", "decision": "...", "rationale": "..."}}
  ],
  "api_contracts": [
    {{"name": "...", "provider": "workspace::module", "consumers": ["ws::mod"], "specification": "..."}}
  ],
  "type_ownership": [
    {{"type_name": "...", "owner": "workspace::module"}}
  ]
}}
```

Respond with ONLY the JSON block."#,
            mission.description,
            modules_str.join(", ")
        )
    }

    fn parse_constraints(&self, response: &str) -> Result<GlobalConstraints> {
        let mut constraints = GlobalConstraints::new();

        // Extract JSON from response
        let json_str = Self::extract_json(response);

        if let Ok(parsed) = serde_json::from_str::<ConstraintAnalysis>(json_str) {
            for td in parsed.tech_decisions {
                constraints.add_tech_decision(TechDecision {
                    topic: td.topic,
                    decision: td.decision,
                    rationale: td.rationale,
                    affected_modules: Vec::new(),
                });
            }

            for ac in parsed.api_contracts {
                if let Some(provider) = QualifiedModule::from_qualified_string(&ac.provider) {
                    let consumers: Vec<QualifiedModule> = ac
                        .consumers
                        .iter()
                        .filter_map(|s| QualifiedModule::from_qualified_string(s))
                        .collect();

                    constraints.add_api_contract(
                        ApiContract::new(ac.name, provider)
                            .with_consumers(consumers)
                            .with_specification(ac.specification),
                    );
                }
            }
        } else {
            debug!("Failed to parse constraints JSON, using empty constraints");
        }

        Ok(constraints)
    }

    fn extract_json(response: &str) -> &str {
        if let Some(start) = response.find("```json") {
            let content = &response[start + 7..];
            return content
                .find("```")
                .map(|end| content[..end].trim())
                .unwrap_or(content.trim());
        }

        if let Some(start) = response.find('{')
            && let Some(end) = response.rfind('}')
        {
            return &response[start..=end];
        }

        response.trim()
    }

    async fn refine_per_workspace(
        &self,
        constraints: &mut GlobalConstraints,
        mission: &ConsensusMission,
        registry: &WorkspaceRegistry,
    ) -> Result<()> {
        for ws_info in registry.available_workspaces() {
            let ws_name = &ws_info.id;
            let refinement_prompt = format!(
                r#"## Workspace Refinement: {}

Given global constraints:
- Tech decisions: {:?}
- API contracts: {:?}

Mission: {}

Provide workspace-specific refinements (1-3 bullet points):
"#,
                ws_name,
                constraints
                    .tech_decisions
                    .iter()
                    .map(|t| &t.decision)
                    .collect::<Vec<_>>(),
                constraints
                    .api_contracts
                    .iter()
                    .map(|a| &a.name)
                    .collect::<Vec<_>>(),
                mission.description
            );

            let working_dir = std::env::current_dir().unwrap_or_default();
            if let Ok(response) = self
                .aggregator
                .run_with_profile(
                    &refinement_prompt,
                    &working_dir,
                    PermissionProfile::ReadOnly,
                )
                .await
            {
                for line in response.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('-') || trimmed.starts_with('•') {
                        let refinement = trimmed.trim_start_matches(['-', '•', ' ']);
                        if !refinement.is_empty() {
                            constraints.add_workspace_refinement(ws_name, refinement);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct ConstraintAnalysis {
    #[serde(default)]
    tech_decisions: Vec<TechDecisionRaw>,
    #[serde(default)]
    api_contracts: Vec<ApiContractRaw>,
    #[serde(default)]
    #[allow(dead_code)]
    type_ownership: Vec<TypeOwnershipRaw>,
}

#[derive(Debug, Deserialize)]
struct TechDecisionRaw {
    topic: String,
    decision: String,
    #[serde(default)]
    rationale: String,
}

#[derive(Debug, Deserialize)]
struct ApiContractRaw {
    name: String,
    provider: String,
    #[serde(default)]
    consumers: Vec<String>,
    #[serde(default)]
    specification: String,
}

#[derive(Debug, Deserialize)]
struct TypeOwnershipRaw {
    #[allow(dead_code)]
    type_name: String,
    #[allow(dead_code)]
    owner: String,
}

// ============================================================================
// Phase 2: Synthesis with Backtracking
// ============================================================================

/// Context passed during Phase 2 synthesis rounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisContext {
    pub constraints: GlobalConstraints,
    pub round_number: usize,
    pub peer_proposals: HashMap<String, ProposalSummary>,
    pub detected_conflicts: Vec<String>,
    pub backtrack_count: usize,
}

impl SynthesisContext {
    pub fn new(constraints: GlobalConstraints) -> Self {
        Self {
            constraints,
            round_number: 0,
            peer_proposals: HashMap::new(),
            detected_conflicts: Vec::new(),
            backtrack_count: 0,
        }
    }

    pub fn next_round(&self) -> Self {
        Self {
            constraints: self.constraints.clone(),
            round_number: self.round_number + 1,
            peer_proposals: self.peer_proposals.clone(),
            detected_conflicts: Vec::new(),
            backtrack_count: self.backtrack_count,
        }
    }

    pub fn with_peer_proposal(&mut self, agent_id: impl Into<String>, summary: ProposalSummary) {
        self.peer_proposals.insert(agent_id.into(), summary);
    }

    pub fn with_conflict(&mut self, description: impl Into<String>) {
        self.detected_conflicts.push(description.into());
    }

    pub fn increment_backtrack(&mut self) {
        self.backtrack_count += 1;
    }
}

/// Summary of a proposal for cross-visibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalSummary {
    pub agent_id: String,
    pub module: QualifiedModule,
    pub approach: String,
    pub api_changes: Vec<String>,
    pub dependencies_used: Vec<String>,
    pub dependencies_provided: Vec<String>,
}

impl ProposalSummary {
    pub fn new(agent_id: impl Into<String>, module: QualifiedModule) -> Self {
        Self {
            agent_id: agent_id.into(),
            module,
            approach: String::new(),
            api_changes: Vec::new(),
            dependencies_used: Vec::new(),
            dependencies_provided: Vec::new(),
        }
    }

    pub fn with_approach(mut self, approach: impl Into<String>) -> Self {
        self.approach = approach.into();
        self
    }

    pub fn with_api_changes(mut self, changes: Vec<String>) -> Self {
        self.api_changes = changes;
        self
    }
}

// ============================================================================
// Two-Phase Orchestrator
// ============================================================================

/// Orchestrates two-phase consensus execution.
pub struct TwoPhaseOrchestrator {
    direction_phase: DirectionSettingPhase,
    max_backtracks: usize,
}

impl TwoPhaseOrchestrator {
    pub fn new(aggregator: Arc<TaskAgent>) -> Self {
        Self {
            direction_phase: DirectionSettingPhase::new(aggregator),
            max_backtracks: 3,
        }
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceRegistry>) -> Self {
        self.direction_phase = self.direction_phase.with_workspace_registry(registry);
        self
    }

    pub fn with_max_backtracks(mut self, max: usize) -> Self {
        self.max_backtracks = max;
        self
    }

    /// Execute Phase 1 (Direction Setting).
    pub async fn execute_phase1(
        &self,
        mission: &ConsensusMission,
        involved_modules: &[QualifiedModule],
    ) -> Result<GlobalConstraints> {
        self.direction_phase
            .execute(mission, involved_modules)
            .await
    }

    /// Create initial synthesis context for Phase 2.
    pub fn create_synthesis_context(&self, constraints: GlobalConstraints) -> SynthesisContext {
        SynthesisContext::new(constraints)
    }

    /// Check if backtracking is allowed.
    pub fn can_backtrack(&self, context: &SynthesisContext) -> bool {
        context.backtrack_count < self.max_backtracks
    }

    /// Generate constraint from conflict for backtracking.
    pub fn conflict_to_constraint(
        &self,
        conflict_description: &str,
        affected_modules: Vec<QualifiedModule>,
    ) -> RuntimeConstraint {
        RuntimeConstraint {
            source: "backtrack_resolution".to_string(),
            constraint_type: super::shared::ConstraintType::ApiNaming,
            description: conflict_description.to_string(),
            affected_modules,
        }
    }
}

// ============================================================================
// Peer Summary Builder
// ============================================================================

/// Builds peer summaries for cross-tier visibility.
pub struct PeerSummaryBuilder;

impl PeerSummaryBuilder {
    /// Build context string from peer summaries for upper tier.
    pub fn build_peer_context(peers: &[ProposalSummary]) -> String {
        if peers.is_empty() {
            return String::new();
        }

        let mut context = String::from("## Peer Unit Summaries\n\n");

        for peer in peers {
            context.push_str(&format!(
                "### {} ({})\n**Approach**: {}\n",
                peer.agent_id,
                peer.module.to_qualified_string(),
                peer.approach
            ));

            if !peer.api_changes.is_empty() {
                context.push_str("**API Changes**: ");
                context.push_str(&peer.api_changes.join(", "));
                context.push('\n');
            }

            if !peer.dependencies_provided.is_empty() {
                context.push_str("**Provides**: ");
                context.push_str(&peer.dependencies_provided.join(", "));
                context.push('\n');
            }

            if !peer.dependencies_used.is_empty() {
                context.push_str("**Uses**: ");
                context.push_str(&peer.dependencies_used.join(", "));
                context.push('\n');
            }

            context.push('\n');
        }

        context
    }

    /// Build constraints context string.
    pub fn build_constraints_context(constraints: &GlobalConstraints) -> String {
        let mut context = String::from("## Active Constraints\n\n");

        if !constraints.tech_decisions.is_empty() {
            context.push_str("### Technology Decisions\n");
            for td in &constraints.tech_decisions {
                context.push_str(&format!("- **{}**: {}\n", td.topic, td.decision));
            }
            context.push('\n');
        }

        if !constraints.api_contracts.is_empty() {
            context.push_str("### API Contracts\n");
            for ac in &constraints.api_contracts {
                context.push_str(&format!(
                    "- **{}**: provided by {}\n",
                    ac.name,
                    ac.provider.to_qualified_string()
                ));
            }
            context.push('\n');
        }

        if !constraints.runtime_constraints.is_empty() {
            context.push_str("### Runtime Constraints (from backtracking)\n");
            for rc in &constraints.runtime_constraints {
                context.push_str(&format!("- {}\n", rc.description));
            }
            context.push('\n');
        }

        context
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synthesis_context_rounds() {
        let constraints = GlobalConstraints::new();
        let ctx = SynthesisContext::new(constraints);
        assert_eq!(ctx.round_number, 0);

        let ctx2 = ctx.next_round();
        assert_eq!(ctx2.round_number, 1);
    }

    #[test]
    fn test_synthesis_context_peer_proposals() {
        let mut ctx = SynthesisContext::new(GlobalConstraints::new());
        let module = QualifiedModule::new("ws", "mod");
        let summary = ProposalSummary::new("agent-1", module).with_approach("Use JWT");

        ctx.with_peer_proposal("agent-1", summary);
        assert!(ctx.peer_proposals.contains_key("agent-1"));
    }

    #[test]
    fn test_proposal_summary() {
        let module = QualifiedModule::new("project-a", "auth");
        let summary = ProposalSummary::new("planner-1", module)
            .with_approach("Implement JWT validation")
            .with_api_changes(vec!["AuthService.validate()".to_string()]);

        assert_eq!(summary.approach, "Implement JWT validation");
        assert_eq!(summary.api_changes.len(), 1);
    }

    #[test]
    fn test_peer_summary_builder() {
        let module = QualifiedModule::new("ws", "auth");
        let summary = ProposalSummary::new("agent-1", module)
            .with_approach("JWT auth")
            .with_api_changes(vec!["validate()".to_string()]);

        let context = PeerSummaryBuilder::build_peer_context(&[summary]);
        assert!(context.contains("agent-1"));
        assert!(context.contains("JWT auth"));
        assert!(context.contains("validate()"));
    }

    #[test]
    fn test_constraints_context_builder() {
        let mut constraints = GlobalConstraints::new();
        constraints.add_tech_decision(TechDecision::new("auth", "Use JWT"));

        let context = PeerSummaryBuilder::build_constraints_context(&constraints);
        assert!(context.contains("Technology Decisions"));
        assert!(context.contains("Use JWT"));
    }

    #[test]
    fn test_backtrack_limit() {
        let orchestrator = TwoPhaseOrchestrator::new(Arc::new(crate::agent::TaskAgent::new(
            crate::config::AgentConfig::default(),
        )))
        .with_max_backtracks(2);

        let mut ctx = SynthesisContext::new(GlobalConstraints::new());
        assert!(orchestrator.can_backtrack(&ctx));

        ctx.increment_backtrack();
        assert!(orchestrator.can_backtrack(&ctx));

        ctx.increment_backtrack();
        assert!(!orchestrator.can_backtrack(&ctx));
    }
}
