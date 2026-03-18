//! Core engine methods: synthesis, prompt building, messaging, and utility functions.

use std::path::Path;

use tracing::{debug, error, warn};

use super::super::messaging::{AgentMessage, MessagePayload};
use super::super::pool::AgentPool;
use crate::domain::VoteDecision;
use super::super::traits::{AgentTask, TaskContext, TaskPriority, extract_field};
use super::conflict::Conflict;
use super::types::{
    AgentRequest, ScoredProposalRound, ConsensusSynthesisOutput, ScoredProposal,
};
use super::ConsensusEngine;
use crate::error::{PilotError, Result};
use crate::state::{ConsensusProjection, DomainEvent};

impl ConsensusEngine {
    #[inline]
    pub(super) fn is_timed_out(deadline: std::time::Instant) -> bool {
        std::time::Instant::now() >= deadline
    }

    pub(super) async fn broadcast_consensus_request(&self, round: u32, proposal_hash: &str, proposal: &str) {
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

    pub(super) async fn broadcast_consensus_vote(
        &self,
        agent_id: &str,
        round: u32,
        decision: VoteDecision,
        rationale: &str,
    ) {
        let Some(bus) = &self.message_bus else { return };

        let message = AgentMessage::consensus_vote(
            agent_id,
            "consensus-engine",
            round,
            decision,
            rationale.to_string(),
        );

        if let Err(e) = bus.send(message).await {
            debug!(error = %e, "Failed to broadcast consensus vote");
        }
    }

    pub(super) async fn broadcast_conflict_alert(&self, conflict: &Conflict, round: u32) {
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

    pub(super) fn compute_proposal_hash(&self, content: &str, round: u32) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        hasher.update(round.to_le_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub(super) fn extract_affected_modules(&self, content: &str) -> Vec<String> {
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

    pub(super) async fn emit_event(&self, event: DomainEvent) {
        if let Some(ref store) = self.event_store
            && let Err(e) = store.append(event).await
        {
            warn!(error = %e, "Failed to emit consensus event");
        }
    }

    pub(super) async fn load_projection(&self, mission_id: &str) -> Option<ConsensusProjection> {
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

    pub(super) async fn synthesize_with_scoring(
        &self,
        proposals: &[ScoredProposal],
        topic: &str,
        round: usize,
        working_dir: &Path,
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

        let response = self
            .aggregator
            .run_with_profile(&prompt, working_dir, super::super::shared::PermissionProfile::ReadOnly)
            .await?;

        // Parse JSON from response
        self.parse_synthesis_json(&response)
    }

    pub(super) fn parse_synthesis_json(&self, response: &str) -> Result<ConsensusSynthesisOutput> {
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

    pub(super) fn find_json_bounds(s: &str) -> Option<(usize, usize)> {
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

    pub(super) fn extract_json_block(response: &str) -> &str {
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

    pub(super) fn build_proposal_prompt(
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

    pub(super) async fn inject_context_to_new_agents(
        &self,
        requests: &[AgentRequest],
        previous_rounds: &[ScoredProposalRound],
        pool: &AgentPool,
        working_dir: &Path,
    ) -> Result<()> {
        if previous_rounds.is_empty() {
            return Ok(());
        }

        let context_summary = self.summarize_rounds(previous_rounds);
        let open_questions = self.extract_open_questions(previous_rounds);

        for request in requests {
            let role_id = super::super::traits::AgentRole::module(&request.module).id;
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

    pub(super) fn summarize_rounds(&self, rounds: &[ScoredProposalRound]) -> String {
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

    pub(super) fn extract_open_questions(&self, rounds: &[ScoredProposalRound]) -> String {
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
