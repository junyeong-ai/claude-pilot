//! Meta-strategy orchestrator for convergence.

use std::path::Path;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::domain::{AttemptedStrategy, EscalationContext, HumanAction, StrategyOutcome};
use crate::error::Result;
use crate::verification::Issue;

use super::{
    AttemptResult, ConvergenceContext, ConvergenceLevel, ConvergenceStrategy,
    MetaConvergenceResult, MetaConvergenceState, PartialConvergencePolicy,
};

/// Configuration for meta-convergence.
#[derive(Debug, Clone)]
pub struct MetaConvergenceConfig {
    pub required_clean_rounds: u32,
    pub max_rounds_per_level: u32,
    pub stagnation_threshold: u32,
    pub total_timeout: Duration,
    pub partial_policy: PartialConvergencePolicy,
}

impl Default for MetaConvergenceConfig {
    fn default() -> Self {
        Self {
            required_clean_rounds: 2,
            max_rounds_per_level: 5,
            stagnation_threshold: 3,
            total_timeout: Duration::from_secs(3600),
            partial_policy: PartialConvergencePolicy::default(),
        }
    }
}

/// Orchestrates convergence across multiple strategy levels.
pub struct MetaConvergenceOrchestrator {
    strategies: Vec<Box<dyn ConvergenceStrategy>>,
    config: MetaConvergenceConfig,
}

impl MetaConvergenceOrchestrator {
    pub fn new(config: MetaConvergenceConfig) -> Self {
        Self {
            strategies: Vec::new(),
            config,
        }
    }

    pub fn register_strategy(&mut self, strategy: Box<dyn ConvergenceStrategy>) {
        self.strategies.push(strategy);
        self.strategies.sort_by_key(|s| s.level());
    }

    /// Run convergence with automatic level escalation.
    pub async fn converge(
        &self,
        mission_id: &str,
        working_dir: &Path,
        initial_issues: Vec<Issue>,
    ) -> Result<MetaConvergenceResult> {
        let start = Instant::now();
        let mut state = MetaConvergenceState::new(self.config.stagnation_threshold);
        state.issues = initial_issues;

        let context = ConvergenceContext {
            mission_id: mission_id.to_string(),
            working_dir: working_dir.to_path_buf(),
            max_rounds_per_level: self.config.max_rounds_per_level,
            required_clean_rounds: self.config.required_clean_rounds,
        };

        // Early-abort: Check for obviously unfixable issues before wasting rounds.
        // These issues cannot be resolved through code fixes and need human intervention.
        if let Some(reason) = self.should_early_escalate(&state.issues) {
            info!(reason = %reason, "Early escalation: unfixable issue detected");
            return self.build_human_escalation(state, &context);
        }

        loop {
            // Check timeout
            if start.elapsed() > self.config.total_timeout {
                warn!(
                    elapsed_secs = start.elapsed().as_secs(),
                    "Convergence timed out"
                );
                return Ok(MetaConvergenceResult::Timeout {
                    best_round: state.progress.best_round(),
                    state,
                });
            }

            // Get strategy for current level
            let strategy = match self.get_strategy(state.current_level) {
                Some(s) => s,
                None => {
                    // No strategy available - try next level
                    if state.escalate_level().is_none() {
                        return self.build_human_escalation(state, &context);
                    }
                    continue;
                }
            };

            info!(
                level = state.current_level.name(),
                round = state.total_rounds + 1,
                "Attempting convergence"
            );

            // Attempt convergence
            let attempt = strategy.attempt(&state, &context).await?;
            state.record_round(attempt.new_issues.clone());

            match attempt.result {
                AttemptResult::Converged => {
                    if state.is_converged(self.config.required_clean_rounds) {
                        info!(
                            rounds = state.total_rounds,
                            level = state.current_level.name(),
                            "Convergence achieved"
                        );
                        return Ok(MetaConvergenceResult::Converged {
                            rounds: state.total_rounds,
                            final_level: state.current_level,
                        });
                    }
                }
                AttemptResult::Progress => {
                    debug!("Making progress, continuing at current level");
                }
                AttemptResult::Stagnating | AttemptResult::Degrading => {
                    // Check if we should escalate
                    if state.progress.should_escalate_level() {
                        info!(
                            from = state.current_level.name(),
                            "Escalating convergence level"
                        );

                        // Try reduced scope before human escalation
                        if state.current_level == ConvergenceLevel::ReducedScope
                            && let Some(result) = self.try_partial_convergence(&state)
                        {
                            return Ok(result);
                        }

                        if state.escalate_level().is_none() {
                            return self.build_human_escalation(state, &context);
                        }
                    }
                }
            }
        }
    }

    fn get_strategy(&self, level: ConvergenceLevel) -> Option<&dyn ConvergenceStrategy> {
        self.strategies
            .iter()
            .find(|s| s.level() == level)
            .map(|s| s.as_ref())
    }

    fn try_partial_convergence(
        &self,
        state: &MetaConvergenceState,
    ) -> Option<MetaConvergenceResult> {
        let evaluation = self.config.partial_policy.evaluate(&state.issues);

        if evaluation.is_acceptable {
            Some(MetaConvergenceResult::PartiallyConverged {
                rounds: state.total_rounds,
                accepted_issues: evaluation.accepted_issues,
            })
        } else {
            None
        }
    }

    fn build_human_escalation(
        &self,
        state: MetaConvergenceState,
        context: &ConvergenceContext,
    ) -> Result<MetaConvergenceResult> {
        let summary = format!(
            "Convergence failed after {} rounds across {} level transitions",
            state.total_rounds,
            state.level_transitions.len()
        );

        let attempted = state
            .level_transitions
            .iter()
            .map(|t| AttemptedStrategy {
                name: t.from.name().to_string(),
                level: t.from.as_u8(),
                rounds: t.at_round,
                outcome: StrategyOutcome::Stagnated,
            })
            .collect();

        let mut escalation = EscalationContext::new(&context.mission_id, summary)
            .with_strategies(attempted)
            .with_issues(state.issues.clone());

        // Add suggested actions
        if !state.issues.is_empty() {
            let critical: Vec<_> = state
                .issues
                .iter()
                .filter(|i| i.severity == crate::verification::IssueSeverity::Critical)
                .collect();

            if critical.is_empty() {
                escalation.add_action(HumanAction::AcceptPartial {
                    remaining_issues: state.issues.len(),
                });
            }

            for issue in state.issues.iter().take(3) {
                if let Some(file) = &issue.file {
                    escalation.add_action(HumanAction::ManualFix {
                        file: file.into(),
                        hint: issue.message.clone(),
                    });
                }
            }
        }

        escalation.add_action(HumanAction::Cancel);

        Ok(MetaConvergenceResult::NeedsHuman {
            state,
            context: Box::new(escalation),
        })
    }

    /// Check for issues that are obviously unfixable through code changes.
    ///
    /// This is a "golden heuristic" - uses deterministic patterns that work across
    /// all languages/frameworks to save 5-10 convergence rounds on ~30% of failures.
    ///
    /// ## Unfixable Issue Categories
    /// - **External dependencies**: Missing crates, packages, modules not in registry
    /// - **Permission errors**: Cannot write to file, access denied
    /// - **Environment issues**: Missing tools, wrong versions, PATH problems
    /// - **Network issues that aren't transient**: Blocked hosts, invalid certificates
    fn should_early_escalate(&self, issues: &[Issue]) -> Option<String> {
        for issue in issues {
            let msg_lower = issue.message.to_lowercase();

            // External dependency issues - cannot fix without user intervention
            // Works for: Cargo (crate), npm (package), pip (package), go (module)
            if msg_lower.contains("not found in registry")
                || msg_lower.contains("no matching package")
                || msg_lower.contains("could not find crate")
                || msg_lower.contains("cannot find module")
                || msg_lower.contains("no matching version")
            {
                return Some(format!(
                    "External dependency unavailable: {}",
                    truncate_message(&issue.message, 100)
                ));
            }

            // Permission errors - unfixable without user action
            if msg_lower.contains("permission denied")
                || msg_lower.contains("access denied")
                || msg_lower.contains("operation not permitted")
                || msg_lower.contains("read-only file system")
            {
                return Some(format!(
                    "Permission error requires user action: {}",
                    truncate_message(&issue.message, 100)
                ));
            }

            // Missing required tools - user must install
            if msg_lower.contains("command not found")
                || msg_lower.contains("is not recognized as")
                || msg_lower.contains("executable file not found")
                || msg_lower.contains("no such file or directory")
                    && (msg_lower.contains("bin/") || msg_lower.contains("/usr/"))
            {
                return Some(format!(
                    "Required tool not installed: {}",
                    truncate_message(&issue.message, 100)
                ));
            }

            // Certificate/SSL errors - environment config issue
            if msg_lower.contains("certificate verify failed")
                || msg_lower.contains("ssl_error")
                || msg_lower.contains("unable to get local issuer certificate")
            {
                return Some(format!(
                    "SSL/Certificate configuration issue: {}",
                    truncate_message(&issue.message, 100)
                ));
            }

            // Disk space issues
            if msg_lower.contains("no space left on device")
                || msg_lower.contains("disk quota exceeded")
            {
                return Some("Disk space exhausted - requires cleanup".to_string());
            }
        }

        None
    }
}

/// Truncate message for display, preserving start.
fn truncate_message(msg: &str, max_len: usize) -> String {
    if msg.len() <= max_len {
        msg.to_string()
    } else {
        format!("{}...", &msg[..max_len])
    }
}
