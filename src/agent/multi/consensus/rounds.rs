//! Round execution methods for the consensus engine.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::super::pool::AgentPool;
use super::super::shared::AgentId;
use crate::domain::{RoundOutcome, VoteDecision};
use super::super::traits::{AgentTask, TaskContext, TaskPriority};
use super::conflict::Conflict;
use super::types::{
    AgentProposal, AgentRequest, ConsensusResult, ScoredProposalRound, ConsensusSession,
    ConsensusSynthesisOutput, RoundExecutionContext, RoundOutcomeResult,
    ScoredProposal,
};
use super::ConsensusEngine;
use crate::error::Result;
use crate::state::DomainEvent;

impl ConsensusEngine {
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
        let timeout_duration = std::time::Duration::from_secs(self.config.scoring.consensus_timeout_secs);

        for round_num in start_round..=self.config.max_rounds {
            // Check timeout before each round
            if consensus_start.elapsed() >= timeout_duration {
                warn!(
                    elapsed_secs = consensus_start.elapsed().as_secs(),
                    max_secs = self.config.scoring.consensus_timeout_secs,
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
            .synthesize_with_scoring(
                &scored_proposals,
                ctx.topic,
                round_num,
                &ctx.task_context.working_dir,
            )
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

        let converged = synthesis_output.is_agreed();

        session.rounds.push(ScoredProposalRound {
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
            let decision = if sp.score.weighted_score() >= self.config.scoring.approve_threshold {
                VoteDecision::Approve
            } else if sp.score.weighted_score() >= self.config.scoring.approve_with_changes_threshold {
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
                    decision,
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
                    resolution.strategy,
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
            let role_id = super::super::traits::AgentRole::module(&req.module).id;
            self.emit_event(DomainEvent::agent_spawned(
                &ctx.task_context.mission_id,
                &role_id,
                crate::agent::multi::identity::RoleType::Module(req.module.clone()),
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
            .filter(|c| c.severity == crate::domain::Severity::Critical && c.resolution.is_none())
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

    pub(super) fn finalize_without_full_consensus(
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
                consensus: "partial_agreement".to_string(),
                plan: last.synthesis.clone(),
                tasks: vec![],
                conflicts: last.conflicts.clone(),
                dissents: vec![],
                agent_needed: vec![],
            });

        if output.is_partial_agreement() {
            Ok(ConsensusResult::PartialAgreement {
                plan: output.plan,
                tasks: output.tasks,
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

    pub(super) async fn collect_scored_proposals(
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
}
