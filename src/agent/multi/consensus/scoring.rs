//! Evidence evaluation, scoring, and agent history tracking.

use std::collections::HashMap;

use tracing::info;

use super::super::traits::{AgentRole, AgentTaskResult};
use super::types::{
    AgentPerformanceHistory, AgentProposalScore, EvidenceQuality, ScoredProposalRound,
};
use super::ConsensusEngine;

impl ConsensusEngine {
    pub(super) fn calculate_proposal_score(
        &self,
        agent_id: &str,
        role: &AgentRole,
        result: &AgentTaskResult,
        topic: &str,
    ) -> AgentProposalScore {
        let sc = &self.config.scoring;
        let module_relevance = if let Some(module_id) = role.module_id() {
            if topic.to_lowercase().contains(&module_id.to_lowercase()) {
                sc.module_match_score
            } else {
                sc.module_partial_score
            }
        } else if role.is_core() {
            sc.core_role_score
        } else {
            sc.default_role_score
        };

        let historical_accuracy = self
            .agent_history
            .read()
            .get(agent_id)
            .map_or(sc.default_accuracy, AgentPerformanceHistory::accuracy_rate);

        let evidence = self.assess_evidence_quality(&result.output);

        AgentProposalScore {
            agent_id: agent_id.to_string(),
            module_relevance,
            historical_accuracy,
            evidence_strength: evidence.strength,
            confidence: evidence.confidence,
        }
    }

    pub(super) fn assess_evidence_quality(&self, content: &str) -> EvidenceQuality {
        let sc = &self.config.scoring;

        let has_file_ref = Self::has_file_reference(content);
        let has_line_ref = Self::has_line_reference(content);
        let has_code = content.contains("```");
        let has_error_code = Self::has_error_code(content);
        let has_identifier = Self::has_identifier_pattern(content);
        let line_count = content.lines().count();

        let mut strength = sc.base_evidence_score;
        if has_file_ref {
            strength += sc.file_reference_bonus;
        }
        if has_code {
            strength += sc.code_snippet_bonus;
        }
        if has_line_ref {
            strength += sc.line_reference_bonus;
        }
        if has_error_code {
            strength += sc.error_code_bonus;
        }
        if has_identifier {
            strength += sc.identifier_pattern_bonus;
        }

        let mut confidence = sc.base_evidence_score;
        if line_count > 20 {
            confidence += sc.consistency_bonus;
        } else if line_count > 10 {
            confidence += sc.implementation_detail_bonus;
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
            confidence += sc.implementation_detail_bonus;
        }

        let file_refs = content
            .lines()
            .filter(|l| Self::has_file_extension(l) || Self::has_line_reference(l))
            .count();
        if file_refs >= 3 {
            confidence += sc.topic_match_bonus;
        } else if file_refs >= 1 {
            confidence += sc.reference_bonus;
        }

        EvidenceQuality {
            strength: strength.min(1.0),
            confidence: confidence.clamp(sc.min_evidence_score, 1.0),
        }
    }

    pub(super) fn has_file_reference(content: &str) -> bool {
        content.contains("src/")
            || content.contains("lib/")
            || content.contains("test/")
            || content.contains("./")
            || Self::has_file_extension(content)
    }

    pub(super) fn has_file_extension(content: &str) -> bool {
        const EXTENSIONS: &[&str] = &[
            ".rs", ".py", ".go", ".java", ".ts", ".js", ".tsx", ".jsx", ".rb", ".php", ".c",
            ".cpp", ".h", ".hpp", ".cs", ".kt", ".swift", ".scala", ".ex", ".exs", ".clj", ".zig",
            ".toml", ".yaml", ".yml", ".json", ".xml", ".html", ".css", ".sql",
        ];
        EXTENSIONS.iter().any(|ext| content.contains(ext))
    }

    pub(super) fn has_line_reference(content: &str) -> bool {
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

    pub(super) fn has_error_code(content: &str) -> bool {
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_ascii_uppercase() {
                let mut code = String::new();
                code.push(c);
                while let Some(&next) = chars.peek() {
                    if next.is_ascii_alphanumeric() && code.len() < 10 {
                        if let Some(ch) = chars.next() {
                            code.push(ch);
                        }
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

    pub(super) fn has_identifier_pattern(content: &str) -> bool {
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

    pub(super) fn update_agent_history(&self, rounds: &[ScoredProposalRound]) {
        let sc = &self.config.scoring;
        let mut history = self.agent_history.write();

        for round in rounds {
            for sp in &round.proposals {
                let entry = history.entry(sp.proposal.agent_id.clone()).or_default();
                entry.total_proposals += 1;

                if (round.converged
                    && sp.score.weighted_score() > sc.convergence_threshold)
                    || (!round.converged
                        && sp.score.weighted_score() > sc.partial_convergence_threshold)
                {
                    entry.accepted_proposals += 1;
                }

                if sp.proposal.role.is_module() {
                    *entry
                        .module_contributions
                        .entry(sp.proposal.role.id.clone())
                        .or_insert(0) += 1;
                }
            }
        }

        if history.len() > sc.history_prune_threshold {
            Self::prune_agent_history(&mut history, sc.max_agent_history_entries);
        }
    }

    fn prune_agent_history(
        history: &mut HashMap<String, AgentPerformanceHistory>,
        max_entries: usize,
    ) {
        if history.len() <= max_entries {
            return;
        }

        let mut entries: Vec<_> = history
            .iter()
            .map(|(k, v)| (k.clone(), v.total_proposals))
            .collect();

        entries.sort_by(|a, b| b.1.cmp(&a.1));

        let keys_to_remove: Vec<String> = entries
            .into_iter()
            .skip(max_entries)
            .map(|(k, _)| k)
            .collect();

        for key in keys_to_remove {
            history.remove(&key);
        }

        info!(
            remaining = history.len(),
            pruned_to = max_entries,
            "Pruned agent history"
        );
    }
}
