//! Proposal generation, cross-visibility collection, and convergence checking.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use super::super::pool::AgentPool;
use super::super::shared::AgentId;
use super::super::traits::{AgentTask, TaskContext, TaskPriority};
use super::types::{EnhancedProposal, ProposedApiChange, ProposedTypeChange};
use super::ConsensusEngine;
use crate::error::Result;

impl ConsensusEngine {
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

    pub(super) fn build_cross_visibility_prompt_static(
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
    pub(super) fn parse_enhanced_proposal_static(
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

    pub(super) fn check_proposal_convergence(
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
}
