//! Semantic convergence checking for multi-agent consensus.
//!
//! Replaces Jaccard-based file similarity with semantic analysis of:
//! - API changes (renames, signature modifications)
//! - Type changes (field additions, type modifications)
//! - Dependency relationships (provider/consumer contracts)
//!
//! This enables detection of conflicts between modules that touch
//! different files but have incompatible API/type changes.

use std::collections::{HashMap, HashSet};

use tracing::warn;

use super::shared::{
    ApiChange, ApiChangeType, ConflictSeverity, ConvergenceCheckResult, SemanticConflict,
    SemanticConflictType, TypeChange,
};

// ============================================================================
// Convergence Configuration
// ============================================================================

const CONVERGENCE_THRESHOLD: f64 = 0.8;
const HIGH_AGREEMENT_THRESHOLD: f64 = 0.9;

// ============================================================================
// Proposal for Convergence Checking
// ============================================================================

/// Proposal with semantic information for convergence checking.
#[derive(Debug, Clone)]
pub struct SemanticProposal {
    pub agent_id: String,
    pub module_id: String,
    pub summary: String,
    pub api_changes: Vec<ApiChange>,
    pub type_changes: Vec<TypeChange>,
    pub dependencies_used: Vec<String>,
    pub dependencies_provided: Vec<String>,
}

impl SemanticProposal {
    pub fn new(agent_id: impl Into<String>, module_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            module_id: module_id.into(),
            summary: String::new(),
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

    pub fn with_api_changes(mut self, changes: Vec<ApiChange>) -> Self {
        self.api_changes = changes;
        self
    }

    pub fn with_type_changes(mut self, changes: Vec<TypeChange>) -> Self {
        self.type_changes = changes;
        self
    }

    pub fn with_dependencies(mut self, used: Vec<String>, provided: Vec<String>) -> Self {
        self.dependencies_used = used;
        self.dependencies_provided = provided;
        self
    }
}

// ============================================================================
// Semantic Convergence Checker
// ============================================================================

/// Checks semantic convergence of proposals.
pub struct SemanticConvergenceChecker {
    convergence_threshold: f64,
}

impl Default for SemanticConvergenceChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticConvergenceChecker {
    pub fn new() -> Self {
        Self {
            convergence_threshold: CONVERGENCE_THRESHOLD,
        }
    }

    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.convergence_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Check convergence of proposals.
    pub fn check_convergence(&self, proposals: &[SemanticProposal]) -> ConvergenceCheckResult {
        if proposals.is_empty() {
            return ConvergenceCheckResult::Converged {
                score: 1.0,
                minor_conflicts: vec![],
            };
        }

        if proposals.len() == 1 {
            return ConvergenceCheckResult::Converged {
                score: 1.0,
                minor_conflicts: vec![],
            };
        }

        // 1. Detect conflicts
        let conflicts = self.detect_all_conflicts(proposals);

        // 2. Check for blocking conflicts
        let blocking: Vec<_> = conflicts
            .iter()
            .filter(|c| c.severity == ConflictSeverity::Blocking)
            .cloned()
            .collect();

        if !blocking.is_empty() {
            return ConvergenceCheckResult::Blocked {
                conflicts: blocking,
            };
        }

        // 3. Calculate agreement score on shared concerns
        let agreement_score = self.calculate_agreement_score(proposals);

        // 4. Determine convergence result
        if agreement_score >= self.convergence_threshold {
            let minor_conflicts: Vec<_> = conflicts
                .into_iter()
                .filter(|c| c.severity != ConflictSeverity::Blocking)
                .collect();

            ConvergenceCheckResult::Converged {
                score: agreement_score,
                minor_conflicts,
            }
        } else {
            ConvergenceCheckResult::NeedsMoreRounds {
                current_score: agreement_score,
                conflicts,
            }
        }
    }

    /// Detect all conflicts between proposals.
    fn detect_all_conflicts(&self, proposals: &[SemanticProposal]) -> Vec<SemanticConflict> {
        let mut conflicts = Vec::new();

        // API conflicts
        conflicts.extend(self.detect_api_conflicts(proposals));

        // Type conflicts
        conflicts.extend(self.detect_type_conflicts(proposals));

        // Dependency conflicts
        conflicts.extend(self.detect_dependency_conflicts(proposals));

        conflicts
    }

    /// Detect API naming/signature conflicts.
    fn detect_api_conflicts(&self, proposals: &[SemanticProposal]) -> Vec<SemanticConflict> {
        let mut conflicts = Vec::new();

        // Group API changes by original name to detect rename conflicts
        let mut api_changes_by_name: HashMap<String, Vec<(&str, &ApiChange)>> = HashMap::new();

        for proposal in proposals {
            for change in &proposal.api_changes {
                let key = match &change.change_type {
                    ApiChangeType::Rename { old_name } => old_name.clone(),
                    _ => change.api_name.clone(),
                };
                api_changes_by_name
                    .entry(key)
                    .or_default()
                    .push((&proposal.agent_id, change));
            }
        }

        // Check for conflicting renames
        for (original_name, changes) in &api_changes_by_name {
            let renames: Vec<_> = changes
                .iter()
                .filter(|(_, c)| matches!(c.change_type, ApiChangeType::Rename { .. }))
                .collect();

            if renames.len() > 1 {
                // Multiple agents renaming the same API
                let proposed_names: HashSet<_> = renames.iter().map(|(_, c)| &c.api_name).collect();

                if proposed_names.len() > 1 {
                    // Conflicting rename proposals
                    let agents_and_names: Vec<(String, String)> = renames
                        .iter()
                        .map(|(agent, c)| (agent.to_string(), c.api_name.clone()))
                        .collect();

                    conflicts.push(SemanticConflict::api_rename(
                        original_name,
                        agents_and_names,
                    ));
                }
            }

            // Check for conflicting signature modifications
            let modifications: Vec<_> = changes
                .iter()
                .filter(|(_, c)| c.change_type == ApiChangeType::ModifySignature)
                .collect();

            if modifications.len() > 1 {
                let signatures: HashSet<_> = modifications
                    .iter()
                    .filter_map(|(_, c)| c.new_signature.as_ref())
                    .collect();

                if signatures.len() > 1 {
                    let agents: Vec<String> =
                        modifications.iter().map(|(a, _)| a.to_string()).collect();

                    conflicts.push(SemanticConflict {
                        conflict_type: SemanticConflictType::ApiRename {
                            old_name: original_name.clone(),
                            proposed_names: signatures.iter().map(|s| s.to_string()).collect(),
                        },
                        agents_involved: agents,
                        description: format!(
                            "Conflicting signature changes for API '{}'",
                            original_name
                        ),
                        severity: ConflictSeverity::Major,
                    });
                }
            }
        }

        conflicts
    }

    /// Detect type change conflicts.
    fn detect_type_conflicts(&self, proposals: &[SemanticProposal]) -> Vec<SemanticConflict> {
        let mut conflicts = Vec::new();

        // Group type changes by type name
        let mut type_changes_by_name: HashMap<String, Vec<(&str, &TypeChange)>> = HashMap::new();

        for proposal in proposals {
            for change in &proposal.type_changes {
                type_changes_by_name
                    .entry(change.type_name.clone())
                    .or_default()
                    .push((&proposal.agent_id, change));
            }
        }

        // Check for conflicting changes to the same type
        for (type_name, changes) in &type_changes_by_name {
            if changes.len() > 1 {
                // Multiple agents modifying the same type - check if compatible
                let change_details: HashSet<_> = changes.iter().map(|(_, c)| &c.details).collect();

                if change_details.len() > 1 {
                    // Conflicting type modifications
                    let agent_changes: Vec<(String, String)> = changes
                        .iter()
                        .map(|(a, c)| (a.to_string(), c.details.clone()))
                        .collect();

                    conflicts.push(SemanticConflict::type_change(type_name, agent_changes));
                }
            }
        }

        conflicts
    }

    /// Detect dependency mismatch conflicts.
    fn detect_dependency_conflicts(&self, proposals: &[SemanticProposal]) -> Vec<SemanticConflict> {
        let mut conflicts = Vec::new();

        // Build provider map
        let mut providers: HashMap<String, &str> = HashMap::new();
        for proposal in proposals {
            for dep in &proposal.dependencies_provided {
                providers.insert(dep.clone(), &proposal.module_id);
            }
        }

        // Check that all used dependencies have providers
        for proposal in proposals {
            for dep in &proposal.dependencies_used {
                if !providers.contains_key(dep) {
                    // Check if any proposal provides something similar
                    let similar = providers.keys().any(|k| {
                        k.to_lowercase().contains(&dep.to_lowercase())
                            || dep.to_lowercase().contains(&k.to_lowercase())
                    });

                    if !similar {
                        warn!(
                            consumer = %proposal.module_id,
                            dependency = %dep,
                            "Dependency not provided by any module"
                        );
                        conflicts.push(SemanticConflict::dependency_mismatch(
                            format!("missing:{}", dep),
                            &proposal.module_id,
                        ));
                    }
                }
            }
        }

        conflicts
    }

    /// Calculate agreement score on shared concerns.
    fn calculate_agreement_score(&self, proposals: &[SemanticProposal]) -> f64 {
        if proposals.len() <= 1 {
            return 1.0;
        }

        let mut scores = Vec::new();

        // Score based on API change consistency
        scores.push(self.api_consistency_score(proposals));

        // Score based on type change consistency
        scores.push(self.type_consistency_score(proposals));

        // Score based on dependency alignment
        scores.push(self.dependency_alignment_score(proposals));

        // Average all scores
        if scores.is_empty() {
            1.0
        } else {
            scores.iter().sum::<f64>() / scores.len() as f64
        }
    }

    fn api_consistency_score(&self, proposals: &[SemanticProposal]) -> f64 {
        // Group API changes by name
        let mut api_changes: HashMap<String, Vec<&ApiChange>> = HashMap::new();
        for proposal in proposals {
            for change in &proposal.api_changes {
                api_changes
                    .entry(change.api_name.clone())
                    .or_default()
                    .push(change);
            }
        }

        if api_changes.is_empty() {
            return HIGH_AGREEMENT_THRESHOLD;
        }

        let mut consistent_count = 0;
        let total = api_changes.len();

        for changes in api_changes.values() {
            // All changes to the same API should have same type
            let change_types: HashSet<_> = changes.iter().map(|c| &c.change_type).collect();
            if change_types.len() == 1 {
                consistent_count += 1;
            }
        }

        consistent_count as f64 / total as f64
    }

    fn type_consistency_score(&self, proposals: &[SemanticProposal]) -> f64 {
        let mut type_changes: HashMap<String, Vec<&TypeChange>> = HashMap::new();
        for proposal in proposals {
            for change in &proposal.type_changes {
                type_changes
                    .entry(change.type_name.clone())
                    .or_default()
                    .push(change);
            }
        }

        if type_changes.is_empty() {
            return HIGH_AGREEMENT_THRESHOLD;
        }

        let mut consistent_count = 0;
        let total = type_changes.len();

        for changes in type_changes.values() {
            let change_types: HashSet<_> = changes.iter().map(|c| &c.change_type).collect();
            if change_types.len() == 1 {
                consistent_count += 1;
            }
        }

        consistent_count as f64 / total as f64
    }

    fn dependency_alignment_score(&self, proposals: &[SemanticProposal]) -> f64 {
        // Build sets of provided and used dependencies
        let mut all_provided: HashSet<&str> = HashSet::new();
        let mut all_used: HashSet<&str> = HashSet::new();

        for proposal in proposals {
            all_provided.extend(proposal.dependencies_provided.iter().map(String::as_str));
            all_used.extend(proposal.dependencies_used.iter().map(String::as_str));
        }

        if all_used.is_empty() {
            return HIGH_AGREEMENT_THRESHOLD;
        }

        // Score based on how many used dependencies are provided
        let satisfied = all_used.intersection(&all_provided).count();
        satisfied as f64 / all_used.len() as f64
    }
}

// ============================================================================
// Cross-Visibility Context
// ============================================================================

/// Context for cross-visibility during consensus rounds.
#[derive(Debug, Clone)]
pub struct CrossVisibilityContext {
    pub round_number: usize,
    pub peer_proposals: HashMap<String, SemanticProposal>,
    pub detected_conflicts: Vec<SemanticConflict>,
}

impl CrossVisibilityContext {
    pub fn new(round: usize) -> Self {
        Self {
            round_number: round,
            peer_proposals: HashMap::new(),
            detected_conflicts: Vec::new(),
        }
    }

    pub fn add_proposal(&mut self, proposal: SemanticProposal) {
        self.peer_proposals
            .insert(proposal.agent_id.clone(), proposal);
    }

    pub fn add_conflict(&mut self, conflict: SemanticConflict) {
        self.detected_conflicts.push(conflict);
    }

    /// Build context string for agent prompt.
    pub fn to_prompt_context(&self) -> String {
        let mut context = format!("## Round {} Context\n\n", self.round_number);

        if !self.peer_proposals.is_empty() {
            context.push_str("### Peer Proposals\n\n");
            for proposal in self.peer_proposals.values() {
                context.push_str(&format!(
                    "**{}** ({}): {}\n",
                    proposal.agent_id,
                    proposal.module_id,
                    proposal.summary.lines().next().unwrap_or("No summary")
                ));

                if !proposal.api_changes.is_empty() {
                    context.push_str(&format!(
                        "  - API changes: {}\n",
                        proposal
                            .api_changes
                            .iter()
                            .map(|c| c.api_name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }

                if !proposal.dependencies_provided.is_empty() {
                    context.push_str(&format!(
                        "  - Provides: {}\n",
                        proposal.dependencies_provided.join(", ")
                    ));
                }

                context.push('\n');
            }
        }

        if !self.detected_conflicts.is_empty() {
            context.push_str("### Detected Conflicts (Please Address)\n\n");
            for conflict in &self.detected_conflicts {
                context.push_str(&format!(
                    "- **{:?}**: {}\n",
                    conflict.severity, conflict.description
                ));
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
    use crate::agent::multi::shared::TypeChangeType;

    #[test]
    fn test_single_proposal_converges() {
        let checker = SemanticConvergenceChecker::new();
        let proposal =
            SemanticProposal::new("agent-1", "auth").with_summary("Implement JWT validation");

        let result = checker.check_convergence(&[proposal]);
        assert!(result.is_converged());
    }

    #[test]
    fn test_compatible_proposals_converge() {
        let checker = SemanticConvergenceChecker::new();

        // p1 adds an API and explicitly provides it as a dependency
        let p1 = SemanticProposal::new("agent-1", "auth")
            .with_summary("Add JWT validation")
            .with_api_changes(vec![ApiChange::add("validate", "fn validate(token: &str)")])
            .with_dependencies(vec![], vec!["auth.validate".to_string()]);

        // p2 uses the dependency that p1 provides
        let p2 = SemanticProposal::new("agent-2", "api")
            .with_summary("Call auth validation")
            .with_dependencies(vec!["auth.validate".to_string()], vec![]);

        let result = checker.check_convergence(&[p1, p2]);
        assert!(result.is_converged());
    }

    #[test]
    fn test_conflicting_rename_detected() {
        let checker = SemanticConvergenceChecker::new();

        let p1 = SemanticProposal::new("agent-1", "auth")
            .with_api_changes(vec![ApiChange::rename("validate", "verify")]);

        let p2 = SemanticProposal::new("agent-2", "auth")
            .with_api_changes(vec![ApiChange::rename("validate", "check")]);

        let result = checker.check_convergence(&[p1, p2]);

        // Should detect conflict but not necessarily block
        let conflicts = result.conflicts();
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_type_conflict_detected() {
        let checker = SemanticConvergenceChecker::new();

        let p1 = SemanticProposal::new("agent-1", "types").with_type_changes(vec![TypeChange {
            type_name: "User".to_string(),
            change_type: TypeChangeType::AddField,
            details: "email: String".to_string(),
            module: None,
        }]);

        let p2 = SemanticProposal::new("agent-2", "types").with_type_changes(vec![TypeChange {
            type_name: "User".to_string(),
            change_type: TypeChangeType::AddField,
            details: "user_email: Option<String>".to_string(),
            module: None,
        }]);

        let result = checker.check_convergence(&[p1, p2]);
        let conflicts = result.conflicts();
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_dependency_mismatch_detected() {
        let checker = SemanticConvergenceChecker::new();

        // Consumer uses dependency not provided by anyone
        let p1 = SemanticProposal::new("agent-1", "api")
            .with_dependencies(vec!["payment.process".to_string()], vec![]);

        let p2 = SemanticProposal::new("agent-2", "auth")
            .with_dependencies(vec![], vec!["auth.validate".to_string()]);

        let result = checker.check_convergence(&[p1, p2]);
        let conflicts = result.conflicts();
        assert!(conflicts.iter().any(|c| matches!(
            c.conflict_type,
            SemanticConflictType::DependencyMismatch { .. }
        )));
    }

    #[test]
    fn test_cross_visibility_context() {
        let mut ctx = CrossVisibilityContext::new(1);

        let proposal = SemanticProposal::new("agent-1", "auth").with_summary("JWT implementation");

        ctx.add_proposal(proposal);

        let prompt = ctx.to_prompt_context();
        assert!(prompt.contains("Round 1"));
        assert!(prompt.contains("agent-1"));
        assert!(prompt.contains("JWT implementation"));
    }

    #[test]
    fn test_agreement_scoring() {
        let checker = SemanticConvergenceChecker::new();

        // All proposals agree on API changes
        let p1 = SemanticProposal::new("agent-1", "mod-a")
            .with_api_changes(vec![ApiChange::add("foo", "fn foo()")]);

        let p2 = SemanticProposal::new("agent-2", "mod-b")
            .with_api_changes(vec![ApiChange::add("bar", "fn bar()")]);

        let result = checker.check_convergence(&[p1, p2]);
        assert!(result.is_converged());

        if let ConvergenceCheckResult::Converged { score, .. } = result {
            assert!(score >= 0.8);
        }
    }
}
