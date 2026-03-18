//! Convergent verification loop for the Coordinator.
//!
//! Implements the N-consecutive-clean-rounds verification pattern.
//! NON-NEGOTIABLE: requires N consecutive clean rounds for success.

use std::collections::HashMap;
use std::path::Path;

use tracing::{debug, info, warn};

use super::super::traits::{
    AgentConvergenceResult, AgentRole, AgentTask, AgentTaskResult, TaskContext, TaskPriority,
    VerificationVerdict,
};
use super::types::HealthAdaptation;
use super::Coordinator;
use crate::error::Result;

impl Coordinator {
    pub(super) async fn run_convergent_verification(
        &self,
        context: &mut TaskContext,
        working_dir: &Path,
        results: &mut Vec<AgentTaskResult>,
    ) -> Result<AgentConvergenceResult> {
        let required_clean_rounds = self.config.convergent.required_clean_rounds as usize;
        let max_rounds = self.config.convergent.max_rounds as usize;
        let max_fix_attempts_per_issue = self.config.convergent.max_fix_attempts_per_issue as usize;

        info!(
            "Starting convergent verification (requires {} consecutive clean rounds)",
            required_clean_rounds
        );

        let mut consecutive_clean = 0;
        let mut total_rounds = 0;
        let mut all_issues_found = Vec::new();
        let mut all_issues_fixed = Vec::new();
        let mut fix_attempts: HashMap<String, usize> = HashMap::new();

        while consecutive_clean < required_clean_rounds && total_rounds < max_rounds {
            total_rounds += 1;
            info!(
                round = total_rounds,
                consecutive_clean = consecutive_clean,
                "Verification round"
            );

            if total_rounds % 3 == 0 {
                match self.check_health_adaptation() {
                    HealthAdaptation::Escalate { reason } => {
                        warn!(reason = %reason, "Health escalation during verification, breaking loop");
                        break;
                    }
                    HealthAdaptation::Throttle { delay_ms } => {
                        debug!(
                            delay_ms = delay_ms,
                            "Throttling verification due to degraded health"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }
                    HealthAdaptation::ForceSequential => {
                        info!("Forcing sequential execution due to critical health");
                    }
                    HealthAdaptation::Continue => {}
                }
            }

            let qa_results = self.run_code_review(context, working_dir).await?;
            let qa_issues: Vec<String> = qa_results
                .iter()
                .filter(|r| !r.is_success())
                .flat_map(|r| r.findings.clone())
                .collect();
            results.extend(qa_results);

            let arch_issues: Vec<String> = if let Some(arch_result) =
                self.run_architecture_review(context, working_dir).await?
            {
                let issues = if arch_result.is_success() {
                    vec![]
                } else {
                    arch_result.findings.clone()
                };
                results.push(arch_result);
                issues
            } else {
                vec![]
            };

            let verify_result = self.run_verification_phase(context, working_dir).await?;
            let verify_issues: Vec<String> = if verify_result.is_success() {
                vec![]
            } else {
                verify_result.findings.clone()
            };
            results.push(verify_result.clone());

            let round_issues: Vec<String> = qa_issues
                .into_iter()
                .chain(arch_issues)
                .chain(verify_issues)
                .filter(|issue| !issue.is_empty())
                .collect();

            if round_issues.is_empty() {
                consecutive_clean += 1;
                info!(
                    round = total_rounds,
                    consecutive_clean = consecutive_clean,
                    "Clean round"
                );
            } else {
                consecutive_clean = 0;
                info!(
                    round = total_rounds,
                    issues = round_issues.len(),
                    "Issues found, resetting clean counter"
                );

                for issue in &round_issues {
                    let issue_key = Self::normalize_issue_key(issue);
                    let attempts = fix_attempts.entry(issue_key.clone()).or_insert(0);
                    *attempts += 1;

                    if *attempts > max_fix_attempts_per_issue {
                        warn!(issue = %issue, attempts = *attempts, "Max fix attempts exceeded for issue");
                        continue;
                    }

                    let mut fix_task = AgentTask {
                        id: format!("fix-r{}-{}", total_rounds, attempts),
                        description: format!("Fix issue: {}", issue),
                        context: context.clone(),
                        priority: TaskPriority::High,
                        role: Some(AgentRole::core_coder()),
                    };

                    self.enrich_task_and_emit(&mut fix_task).await;

                    match self.pool.execute(&fix_task, working_dir).await {
                        Ok(fix_result) => {
                            if fix_result.is_success() {
                                all_issues_fixed.push(issue.clone());
                                context.key_findings.extend(fix_result.findings.clone());
                            }
                            results.push(fix_result);
                        }
                        Err(e) => {
                            warn!(error = %e, "Fix attempt failed");
                        }
                    }
                }

                all_issues_found.extend(round_issues);
            }
        }

        let converged = consecutive_clean >= required_clean_rounds;
        let final_verdict = if converged {
            VerificationVerdict::Passed
        } else if consecutive_clean > 0 {
            VerificationVerdict::PartialPass
        } else if total_rounds >= max_rounds {
            VerificationVerdict::Timeout
        } else {
            VerificationVerdict::Failed
        };

        info!(
            converged = converged,
            clean_rounds = consecutive_clean,
            total_rounds = total_rounds,
            verdict = ?final_verdict,
            "Convergent verification complete"
        );

        Ok(AgentConvergenceResult {
            converged,
            clean_rounds: consecutive_clean,
            total_rounds,
            issues_found: all_issues_found,
            issues_fixed: all_issues_fixed,
            final_verdict,
        })
    }
}
