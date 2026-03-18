
use chrono::Utc;
use tracing::{debug, info, warn};

use super::Orchestrator;
use crate::error::{PilotError, Result};
use crate::mission::Mission;
use crate::notification::{EventType, MissionEvent};
use crate::planning::{ChunkedPlanner, ComplexityEstimator, ComplexityTier, PlanningOrchestrator};
use crate::state::{DomainEvent, MissionState};

impl Orchestrator {
    /// Determines the complexity tier for mission routing.
    /// Returns Trivial, Simple, or Complex tier based on LLM assessment.
    /// Uses Planning model for higher quality strategic decisions.
    ///
    /// Note: --direct flag is handled before this method is called,
    /// bypassing complexity evaluation entirely.
    pub(super) async fn evaluate_complexity_tier(
        &self,
        mission: &Mission,
        evidence: Option<&crate::planning::Evidence>,
    ) -> ComplexityTier {
        // Force full planning if explicitly using isolated mode
        if mission.flags.isolated {
            return ComplexityTier::Complex;
        }

        // Use PlanAgent (Planning model) for complexity assessment
        // Complexity evaluation is a strategic decision that benefits from higher quality reasoning
        let plan_agent = match crate::planning::PlanAgent::new(&self.paths.root, &self.config.agent)
        {
            Ok(agent) => agent,
            Err(e) => {
                warn!(error = %e, "Failed to create planning agent for complexity, defaulting to Complex");
                return ComplexityTier::Complex;
            }
        };

        let mut estimator = ComplexityEstimator::new(
            &self.paths.root,
            &self.config.complexity,
            plan_agent,
            self.config.task_scoring.clone(),
        );
        estimator.load_examples().await;
        let gate = estimator
            .evaluate_with_evidence(&mission.description, evidence)
            .await;

        debug!(
            mission_id = %mission.id,
            tier = ?gate.tier,
            reasoning = %gate.reasoning,
            evidence_based = evidence.is_some(),
            "Complexity tier evaluated"
        );

        gate.tier
    }

    /// Pre-flight check for context budget.
    /// Prevents API errors by detecting overflow before execution.
    pub(super) async fn check_context_budget(&self, mission: &Mission) -> Result<()> {
        use crate::config::AuthMode;
        use crate::context::{ContextEstimator, LoadingRecommendation};

        let working_dir = self.isolation.working_dir(mission);
        let model_config = self.config.agent.model_config()?;
        let max_context = self.config.context.effective_max_tokens(&model_config);

        // Warn if extended_context is configured but ignored due to OAuth mode
        if self.config.agent.extended_context && self.config.agent.auth_mode == AuthMode::Oauth {
            warn!(
                configured_extended_context = self.config.agent.extended_context,
                auth_mode = ?self.config.agent.auth_mode,
                effective_context_window = max_context,
                "extended_context setting ignored in OAuth mode - using 200k limit"
            );
        }

        let estimator =
            ContextEstimator::with_config(max_context, self.config.context.estimator.clone());
        let estimate = estimator.estimate(&working_dir).await;

        debug!(
            auth_mode = ?self.config.agent.auth_mode,
            extended_context_effective = self.config.agent.effective_extended_context(),
            max_context,
            "Context budget check"
        );

        match estimate.recommendation {
            LoadingRecommendation::Emergency => {
                tracing::error!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    max = estimate.max_context,
                    "Context overflow detected - cannot proceed"
                );
                return Err(PilotError::ContextOverflow {
                    estimated: estimate.total_estimated,
                    max: estimate.max_context,
                });
            }
            LoadingRecommendation::SkipMcpResources => {
                warn!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget tight - MCP resources will be skipped"
                );
            }
            LoadingRecommendation::LoadSelective => {
                info!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget moderate - using selective resource loading"
                );
            }
            LoadingRecommendation::LoadAll => {
                debug!(
                    mission_id = %mission.id,
                    estimated = estimate.total_estimated,
                    available = estimate.available_budget,
                    "Context budget sufficient"
                );
            }
        }

        Ok(())
    }

    /// Executes a mission directly without full planning phase.
    /// Includes proper post-processing: commit, on_complete actions, cleanup, and learning extraction.
    pub(super) async fn execute_direct(&self, mission: &mut Mission) -> Result<()> {
        info!(mission_id = %mission.id, "Using direct execution mode");

        self.transition_state(mission, MissionState::Running, "Starting direct execution")
            .await?;
        mission.started_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, &mission.id))
            .await;

        let working_dir = self.isolation.working_dir(mission);

        let result = self
            .direct_agent
            .execute(&mission.description, &working_dir)
            .await?;

        if result.success {
            info!(
                mission_id = %mission.id,
                convergence_rounds = result.convergence_rounds,
                "Direct execution completed successfully"
            );

            // Transition through Verifying before Completed (state machine requirement)
            self.transition_state(mission, MissionState::Verifying, "Verification passed")
                .await?;

            let commit_message = format!("feat({}): {}", mission.id, mission.description);
            self.isolation.commit(mission, &commit_message).await?;

            self.complete_mission(mission).await?;
        } else {
            // cleanup_on_failure handles state transition and notification
            self.cleanup_on_failure(mission).await;
        }

        Ok(())
    }

    /// Execute mission using multi-agent coordinator.
    ///
    /// This method provides an alternative execution path when multi-agent mode is enabled.
    /// The coordinator orchestrates specialized agents (Research, Planning, Coder, Verifier)
    /// through a 4-phase workflow:
    /// 1. Research: Evidence gathering and codebase analysis
    /// 2. Planning: Implementation plan generation
    /// 3. Implementation: Parallel/sequential task execution
    /// 4. Verification: Quality checks and convergent verification
    pub(super) async fn execute_with_coordinator(&self, mission: &mut Mission) -> Result<()> {
        let coordinator = self.coordinator.as_ref().ok_or_else(|| {
            PilotError::AgentExecution("Multi-agent coordinator not initialized".into())
        })?;

        info!(mission_id = %mission.id, "Using multi-agent coordinator execution");

        self.transition_state(
            mission,
            MissionState::Running,
            "Starting multi-agent execution",
        )
        .await?;
        mission.started_at = Some(Utc::now());
        self.store.save(mission).await?;

        self.notifier
            .notify(&MissionEvent::new(EventType::MissionStarted, &mission.id))
            .await;

        let working_dir = self.isolation.working_dir(mission);

        let result = coordinator
            .execute_mission(&mission.id, &mission.description, &working_dir)
            .await?;

        let success_count = result.results.iter().filter(|r| r.is_success()).count();
        self.emit_event(DomainEvent::multi_agent_phase_completed(
            &mission.id,
            crate::agent::multi::session::SessionPhase::Implementation,
            result.results.len(),
            success_count,
            0,
        ))
        .await;

        // Log execution metrics
        let metrics = result.metrics;
        info!(
            mission_id = %mission.id,
            total_executions = metrics.total_executions,
            success_rate = metrics.success_rate,
            avg_duration_ms = metrics.avg_duration_ms,
            "Multi-agent execution completed"
        );

        if result.success {
            info!(
                mission_id = %mission.id,
                summary = %result.summary,
                phase_count = result.results.len(),
                "Multi-agent mission completed successfully"
            );

            // Transition through Verifying before Completed (state machine requirement)
            self.transition_state(
                mission,
                MissionState::Verifying,
                "Multi-agent verification passed",
            )
            .await?;

            let commit_message = format!("feat({}): {}", mission.id, mission.description);
            self.isolation.commit(mission, &commit_message).await?;

            self.complete_mission(mission).await?;
        } else {
            warn!(
                mission_id = %mission.id,
                summary = %result.summary,
                "Multi-agent mission failed"
            );
            self.cleanup_on_failure(mission).await;
        }

        Ok(())
    }

    /// Plan mission using pre-gathered evidence.
    /// This allows evidence reuse from complexity assessment, avoiding duplicate collection.
    pub(super) async fn plan_mission_with_evidence(
        &self,
        mission: &mut Mission,
        evidence: crate::planning::Evidence,
    ) -> Result<()> {
        // Tier-based quality assessment
        let assessment = evidence.quality_tier(&self.config.quality);
        match assessment.tier {
            crate::planning::QualityTier::Red => {
                warn!(
                    mission_id = %mission.id,
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "RED",
                    "Evidence quality insufficient"
                );
                return Err(PilotError::EvidenceGathering(format!(
                    "Evidence RED tier: {} - cannot proceed safely",
                    assessment.format_for_llm()
                )));
            }
            crate::planning::QualityTier::Yellow => {
                if self.config.quality.require_green_tier {
                    warn!(
                        mission_id = %mission.id,
                        quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                        confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                        tier = "YELLOW",
                        "Evidence quality insufficient - require_green_tier is enabled"
                    );
                    return Err(PilotError::EvidenceGathering(format!(
                        "Evidence YELLOW tier: {} - Green tier evidence quality required",
                        assessment.format_for_llm()
                    )));
                }
                warn!(
                    mission_id = %mission.id,
                    quality_score = %format!("{:.1}%", assessment.quality_score * 100.0),
                    confidence = %format!("{:.1}%", assessment.confidence * 100.0),
                    tier = "YELLOW",
                    "Proceeding with caution"
                );
            }
            crate::planning::QualityTier::Green => {}
        }

        let evidence_tokens = evidence.estimated_tokens(&self.config.quality);
        let chunking_threshold = self.config.evidence.plan_evidence_budget;

        // Chunking decision based on evidence size (deterministic, no LLM needed)
        let needs_chunking =
            self.config.chunked_planning.enabled && evidence_tokens > chunking_threshold;

        if needs_chunking {
            info!(
                mission_id = %mission.id,
                evidence_tokens = evidence_tokens,
                threshold = chunking_threshold,
                "Evidence exceeds threshold, using chunked planning"
            );
        }

        // CRITICAL: Use the mission's working directory (worktree path for isolated execution)
        // This ensures the planner initializes from the correct directory
        let working_dir = self.isolation.working_dir(mission);

        // Create planner with correct working_dir for isolated execution
        let planner = PlanningOrchestrator::new(
            &working_dir,
            self.config.verification.ai_plan_validation,
            &self.config.agent,
            self.config.quality.clone(),
            self.config.evidence.clone(),
            self.config.task_decomposition.clone(),
        )?;

        let result = if needs_chunking {
            let chunked = ChunkedPlanner::new(
                &working_dir,
                self.config.agent.clone(),
                self.config.quality.clone(),
                self.config.evidence.clone(),
                self.config.task_decomposition.clone(),
                self.config.chunked_planning.clone(),
            )?;
            chunked.plan(&mission.description, evidence).await?
        } else {
            planner.plan(&mission.description, evidence).await?
        };

        if !result.validation.passed {
            return Err(PilotError::PlanValidation(result.validation.to_summary()));
        }

        planner.apply_to_mission(mission, &result);
        planner.save_artifacts(&mission.id, &result).await?;

        let model_config = self.config.agent.model_config()?;
        let mut context = self
            .context_manager
            .load_or_init_with_model_config(mission, &model_config)
            .await?;
        self.context_manager
            .initialize_from_planning(&mut context, mission, &result.evidence);
        self.context_manager.save(&context).await?;

        if result.recommended_isolation != mission.isolation {
            info!(
                mission_id = %mission.id,
                current = %mission.isolation,
                recommended = %result.recommended_isolation,
                "Isolation mode recommendation differs"
            );
        }

        self.store.save(mission).await?;

        info!(
            mission_id = %mission.id,
            task_count = mission.tasks.len(),
            phase_count = mission.phases.len(),
            risk = %result.plan.risk_assessment.overall_risk,
            "Mission planned"
        );

        Ok(())
    }
}
