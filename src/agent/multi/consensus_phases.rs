//! Direction-Setting Consensus Phase
//!
//! Phase 1 (Direction Setting): Expert analysis -> GlobalConstraints
//! - Actor: TaskAgent aggregator (single expert)
//! - Output: Tech decisions, API contracts, runtime constraints

use std::sync::Arc;

use serde::Deserialize;
use tracing::{debug, info};

use super::shared::PermissionProfile;
use super::adaptive_consensus::ConsensusMission;
use super::shared::{
    ApiContract, GlobalConstraints, QualifiedModule, TechDecision,
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
pub(crate) struct DirectionSettingPhase {
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
        let working_dir = &mission.context.working_dir;

        let response = self
            .aggregator
            .run_with_profile(&analysis_prompt, working_dir, PermissionProfile::ReadOnly)
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
                if let Ok(provider) = ac.provider.parse::<QualifiedModule>() {
                    let consumers: Vec<QualifiedModule> = ac
                        .consumers
                        .iter()
                        .filter_map(|s| s.parse().ok())
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

            if let Ok(response) = self
                .aggregator
                .run_with_profile(
                    &refinement_prompt,
                    &mission.context.working_dir,
                    PermissionProfile::ReadOnly,
                )
                .await
            {
                for line in response.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('-') || trimmed.starts_with('\u{2022}') {
                        let refinement = trimmed.trim_start_matches(['-', '\u{2022}', ' ']);
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

// ============================================================================
// Direction-Setting Orchestrator
// ============================================================================

/// Orchestrates direction-setting phase for consensus execution.
pub(crate) struct TwoPhaseOrchestrator {
    direction_phase: DirectionSettingPhase,
}

impl TwoPhaseOrchestrator {
    pub fn new(aggregator: Arc<TaskAgent>) -> Self {
        Self {
            direction_phase: DirectionSettingPhase::new(aggregator),
        }
    }

    pub fn with_workspace_registry(mut self, registry: Arc<WorkspaceRegistry>) -> Self {
        self.direction_phase = self.direction_phase.with_workspace_registry(registry);
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
}
