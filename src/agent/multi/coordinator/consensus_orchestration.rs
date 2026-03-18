//! Consensus orchestration logic for the Coordinator.
//!
//! Handles adaptive consensus execution including two-phase orchestration,
//! cross-workspace consensus, and hierarchical result processing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::super::adaptive_consensus::{
    ConsensusMission, ConsensusOutcome, HierarchicalConsensusResult,
};
use super::super::consensus::ConsensusTask;
use super::super::shared::QualifiedModule;
use super::super::traits::TaskContext;
use super::Coordinator;
use crate::agent::multi::scope::AgentScope;
use crate::error::{PilotError, Result};
use crate::state::DomainEvent;
use crate::workspace::Workspace;

impl Coordinator {
    /// Run adaptive consensus using the AdaptiveConsensusExecutor.
    ///
    /// When a two-phase orchestrator is configured, this method first runs
    /// the direction-setting phase to establish global constraints before
    /// executing the synthesis phase with cross-visibility.
    pub(super) async fn run_adaptive_consensus(
        &self,
        description: &str,
        context: &TaskContext,
        _working_dir: &Path,
    ) -> Result<HierarchicalConsensusResult> {
        let executor = self
            .adaptive_executor
            .as_ref()
            .ok_or_else(|| PilotError::AgentExecution("Adaptive executor not configured".into()))?;

        let workspace = self
            .workspace
            .as_ref()
            .ok_or_else(|| PilotError::AgentExecution("Workspace not configured".into()))?;

        let affected_files: Vec<PathBuf> =
            context.related_files.iter().map(PathBuf::from).collect();

        let mission = ConsensusMission::new(description, affected_files.clone(), context.clone());

        let scope = AgentScope::Workspace {
            workspace: workspace.name.clone(),
        };

        if let Some(ref orchestrator) = self.two_phase_orchestrator {
            let qualified_modules = self.derive_qualified_modules(&affected_files, workspace);

            let constraints = match orchestrator
                .execute_phase1(&mission, &qualified_modules)
                .await
            {
                Ok(c) => {
                    info!(
                        tech_decisions = c.tech_decisions.len(),
                        api_contracts = c.api_contracts.len(),
                        "Direction phase completed with constraints"
                    );
                    Some(c)
                }
                Err(e) => {
                    warn!(error = %e, "Direction phase failed, proceeding without constraints");
                    None
                }
            };

            if let Some(c) = constraints {
                let constraint_strings: Vec<String> = c
                    .tech_decisions
                    .iter()
                    .map(|td| format!("[Tech] {}: {}", td.topic, td.decision))
                    .chain(c.api_contracts.iter().map(|ac| {
                        format!(
                            "[API] {}: provided by {}",
                            ac.name,
                            ac.provider.to_qualified_string()
                        )
                    }))
                    .chain(
                        c.runtime_constraints
                            .iter()
                            .map(|rc| format!("[Runtime] {}", rc.description)),
                    )
                    .collect();

                info!(
                    constraints = constraint_strings.len(),
                    "Executing Phase 2 synthesis with constraints"
                );

                if self.requires_cross_workspace(&affected_files) {
                    let workspace_ids = self
                        .workspace_registry
                        .workspaces_for_files(&affected_files);
                    let mut workspaces_map: HashMap<String, &Workspace> = HashMap::new();

                    let unavailable: Vec<_> = self
                        .workspace_registry
                        .unavailable_workspaces()
                        .into_iter()
                        .map(|ws| ws.id)
                        .collect();

                    workspaces_map.insert(workspace.name.clone(), workspace);

                    for ws_id in &workspace_ids {
                        if unavailable.contains(ws_id) {
                            warn!(
                                workspace = %ws_id,
                                "Workspace unavailable, proceeding with degraded mode"
                            );
                            continue;
                        }
                        if let Some(ws) = self.additional_workspaces.get(ws_id) {
                            workspaces_map.insert(ws_id.clone(), ws.as_ref());
                        }
                    }

                    if workspaces_map.len() > 1 {
                        info!(
                            workspace_count = workspaces_map.len(),
                            "Using cross-workspace consensus execution"
                        );
                        return executor
                            .execute_cross_workspace(
                                &mission,
                                &workspaces_map,
                                &self.pool,
                                constraint_strings,
                            )
                            .await;
                    }
                }

                return executor
                    .execute_with_constraints(
                        &mission,
                        &scope,
                        workspace,
                        &self.pool,
                        constraint_strings,
                    )
                    .await;
            }
        }

        executor
            .execute(&mission, &scope, workspace, &self.pool)
            .await
    }

    fn derive_qualified_modules(
        &self,
        affected_files: &[PathBuf],
        workspace: &Workspace,
    ) -> Vec<QualifiedModule> {
        let mut modules = Vec::new();
        let workspace_name = &workspace.name;

        for file in affected_files {
            for module in workspace.modules() {
                let file_str = file.to_string_lossy();
                if module.paths.iter().any(|p| file_str.starts_with(p)) {
                    let qualified = QualifiedModule::new(workspace_name, &module.name);
                    let qualified_str = qualified.to_qualified_string();
                    if !modules
                        .iter()
                        .any(|m: &QualifiedModule| m.to_qualified_string() == qualified_str)
                    {
                        modules.push(qualified);
                    }
                    break;
                }
            }
        }

        modules
    }

    pub(super) fn process_hierarchical_result(
        &self,
        result: &HierarchicalConsensusResult,
        context: &mut TaskContext,
    ) -> Vec<ConsensusTask> {
        match result.outcome {
            ConsensusOutcome::Converged => {
                if let Some(ref plan) = result.plan {
                    context.key_findings.push(format!(
                        "Consensus converged in {} rounds: {}",
                        result.total_rounds,
                        plan.lines().next().unwrap_or("Plan agreed")
                    ));
                }
                self.prepare_tasks(result.tasks.clone())
            }
            ConsensusOutcome::PartialConvergence => {
                if let Some(ref plan) = result.plan {
                    context
                        .key_findings
                        .push(format!("Partial consensus: {}", plan));
                }
                if result.tasks.is_empty() {
                    self.extract_implementation_tasks(context)
                } else {
                    self.prepare_tasks(result.tasks.clone())
                }
            }
            ConsensusOutcome::Escalated | ConsensusOutcome::Timeout | ConsensusOutcome::Failed => {
                self.extract_implementation_tasks(context)
            }
        }
    }

    pub(super) async fn emit_hierarchical_consensus_event(
        &self,
        mission_id: &str,
        result: &HierarchicalConsensusResult,
    ) {
        let respondent_count = result
            .tier_results
            .values()
            .map(|r| r.respondent_count)
            .sum::<usize>();

        self.emit_critical_event(DomainEvent::consensus_completed(
            mission_id,
            result.total_rounds as u32,
            respondent_count,
            result.outcome,
        ))
        .await;
    }
}
