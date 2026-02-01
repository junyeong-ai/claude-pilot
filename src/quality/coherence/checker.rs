use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use claude_agent::{Agent as SdkAgent, PermissionMode, ToolAccess};
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::config::{AgentConfig, ModelAgentType};
use crate::error::{PilotError, Result};
use crate::quality::{
    CoherenceCheckType, CoherenceConfig, CoherenceIssue, CoherenceResult, FileChange, Severity,
};
use crate::symora::SymoraClient;

/// Checks coherence across tasks to ensure they work together correctly.
/// Uses LLM for semantic validation of mission requirements.
pub struct CoherenceChecker {
    config: CoherenceConfig,
    symora: Option<SymoraClient>,
    working_dir: PathBuf,
    agent_config: AgentConfig,
}

impl CoherenceChecker {
    pub fn new(working_dir: &Path, config: CoherenceConfig, agent_config: AgentConfig) -> Self {
        Self {
            config,
            symora: None,
            working_dir: working_dir.to_path_buf(),
            agent_config,
        }
    }

    pub fn with_symora(
        working_dir: &Path,
        config: CoherenceConfig,
        agent_config: AgentConfig,
        timeout_secs: u64,
    ) -> Self {
        Self {
            config,
            symora: Some(SymoraClient::new(working_dir, timeout_secs)),
            working_dir: working_dir.to_path_buf(),
            agent_config,
        }
    }

    pub fn set_symora(&mut self, client: SymoraClient) {
        self.symora = Some(client);
    }

    /// Run all coherence checks on completed tasks.
    pub async fn check_all(
        &self,
        task_results: &[CoherenceTaskResult],
        mission_requirements: &[String],
    ) -> Result<CoherenceReport> {
        info!(
            tasks = task_results.len(),
            requirements = mission_requirements.len(),
            "Starting coherence checks"
        );

        let contract_consistency = self.check_contract_consistency(task_results).await?;
        let integration_soundness = self.check_integration_soundness(task_results).await?;
        let mission_completion = self
            .check_mission_completion(task_results, mission_requirements)
            .await?;

        let overall_passed = contract_consistency.passed
            && integration_soundness.passed
            && mission_completion.passed;

        Ok(CoherenceReport {
            contract_consistency,
            integration_soundness,
            mission_completion,
            overall_passed,
        })
    }

    /// Check contract consistency between tasks.
    ///
    /// Uses Symora to verify that:
    /// 1. Types exported by one task match imports in dependent tasks
    /// 2. Function signatures are consistent across task boundaries
    /// 3. No conflicting type definitions exist
    async fn check_contract_consistency(
        &self,
        task_results: &[CoherenceTaskResult],
    ) -> Result<CoherenceResult> {
        let mut issues = Vec::new();
        let mut affected_tasks = Vec::new();

        // Collect all modified files grouped by task
        let mut task_files: HashMap<String, Vec<String>> = HashMap::new();
        for result in task_results {
            let files: Vec<String> = result
                .file_changes
                .iter()
                .map(|f| f.path.to_string_lossy().to_string())
                .collect();
            task_files.insert(result.task_id.clone(), files);
        }

        // Check for conflicting type definitions if Symora is available
        if let Some(ref symora) = self.symora {
            // Find symbols defined in modified files
            let mut symbol_definitions: HashMap<String, Vec<(String, String)>> = HashMap::new(); // symbol -> [(task_id, file)]

            for result in task_results {
                for change in &result.file_changes {
                    let file_path = change.path.to_string_lossy().to_string();

                    if let Ok(symbols) = symora.find_symbols(&file_path, None, None).await {
                        for sym in symbols.symbols {
                            symbol_definitions
                                .entry(sym.name.clone())
                                .or_default()
                                .push((result.task_id.clone(), file_path.clone()));
                        }
                    }
                }
            }

            // Check for symbols defined in multiple files by different tasks
            for (symbol, locations) in &symbol_definitions {
                let unique_tasks: HashSet<&String> = locations.iter().map(|(t, _)| t).collect();
                if unique_tasks.len() > 1 {
                    let task_list: Vec<String> =
                        unique_tasks.iter().map(|s| (*s).clone()).collect();
                    issues.push(CoherenceIssue {
                        check_type: CoherenceCheckType::ContractConsistency,
                        description: format!(
                            "Symbol '{}' defined in multiple files by different tasks: {:?}",
                            symbol,
                            locations.iter().map(|(_, f)| f).collect::<Vec<_>>()
                        ),
                        task_a: task_list.first().cloned(),
                        task_b: task_list.get(1).cloned(),
                        severity: Severity::Major,
                        suggested_resolution: Some(format!(
                            "Consolidate '{}' definition or ensure tasks coordinate",
                            symbol
                        )),
                    });
                    affected_tasks.extend(task_list);
                }
            }

            // Check for broken references across task boundaries
            for result in task_results {
                for change in &result.file_changes {
                    let file_path = change.path.to_string_lossy().to_string();

                    // Get diagnostics to find potential issues.
                    // Trust LSP diagnostics directly without keyword filtering.
                    // LSP already classifies diagnostics - no need for language-specific keywords.
                    if let Ok(diags) = symora.diagnostics(&file_path, true, false).await {
                        for diag in diags.diagnostics {
                            // Report all error-level diagnostics as potential contract issues.
                            // The LSP determines severity; we just surface them.
                            issues.push(CoherenceIssue {
                                check_type: CoherenceCheckType::ContractConsistency,
                                description: format!(
                                    "Diagnostic in '{}': {}",
                                    file_path, diag.message
                                ),
                                task_a: Some(result.task_id.clone()),
                                task_b: None,
                                severity: Severity::Major,
                                suggested_resolution: diag.suggestions.first().cloned(),
                            });
                            affected_tasks.push(result.task_id.clone());
                        }
                    }
                }
            }
        } else {
            // Fallback: Basic file-based contract checking without Symora.
            // Without semantic analysis, any file modified by multiple tasks
            // is flagged as a potential contract issue (LLM can assess actual impact).
            debug!("Symora not available, using basic contract checking");

            let mut file_tasks: HashMap<String, Vec<String>> = HashMap::new();
            for result in task_results {
                for change in &result.file_changes {
                    let path = change.path.to_string_lossy().to_string();
                    file_tasks
                        .entry(path)
                        .or_default()
                        .push(result.task_id.clone());
                }
            }

            for (file, tasks) in &file_tasks {
                if tasks.len() > 1 {
                    issues.push(CoherenceIssue {
                        check_type: CoherenceCheckType::ContractConsistency,
                        description: format!(
                            "File '{}' modified by multiple tasks: {:?}",
                            file, tasks
                        ),
                        task_a: tasks.first().cloned(),
                        task_b: tasks.get(1).cloned(),
                        severity: Severity::Minor,
                        suggested_resolution: Some("Review changes for compatibility".to_string()),
                    });
                    affected_tasks.extend(tasks.clone());
                }
            }
        }

        affected_tasks.sort();
        affected_tasks.dedup();

        let score = if issues.is_empty() {
            1.0
        } else {
            let critical_count = issues
                .iter()
                .filter(|i| matches!(i.severity, Severity::Critical))
                .count();
            let major_count = issues
                .iter()
                .filter(|i| matches!(i.severity, Severity::Major))
                .count();
            let critical_weight = self.config.thresholds.critical_severity_weight;
            let major_weight = self.config.thresholds.major_severity_weight;
            1.0 - ((critical_count as f32 * critical_weight) + (major_count as f32 * major_weight))
                .min(1.0)
        };

        let passed = score >= self.config.thresholds.contract_consistency;

        debug!(
            issues = issues.len(),
            score = score,
            "Contract consistency check"
        );

        Ok(CoherenceResult {
            check_type: CoherenceCheckType::ContractConsistency,
            score,
            passed,
            issues,
            affected_tasks,
        })
    }

    /// Check that tasks integrate correctly without conflicts.
    async fn check_integration_soundness(
        &self,
        task_results: &[CoherenceTaskResult],
    ) -> Result<CoherenceResult> {
        let mut issues = Vec::new();
        let mut affected_tasks = Vec::new();

        // Check for file conflicts
        let mut file_modifications: HashMap<String, Vec<String>> = HashMap::new();
        for result in task_results {
            for change in &result.file_changes {
                file_modifications
                    .entry(change.path.to_string_lossy().to_string())
                    .or_default()
                    .push(result.task_id.clone());
            }
        }

        // Files modified by multiple tasks may indicate integration issues.
        // Use configurable threshold for multi-task modification warning.
        let threshold = self.config.thresholds.multi_task_modification_threshold;
        for (file, tasks) in &file_modifications {
            if tasks.len() > threshold {
                issues.push(CoherenceIssue {
                    check_type: CoherenceCheckType::IntegrationSoundness,
                    description: format!(
                        "File '{}' modified by {} tasks, may indicate poor separation of concerns",
                        file,
                        tasks.len()
                    ),
                    task_a: tasks.first().cloned(),
                    task_b: tasks.last().cloned(),
                    severity: Severity::Minor,
                    suggested_resolution: Some(
                        "Consider refactoring to reduce coupling between tasks".to_string(),
                    ),
                });
                affected_tasks.extend(tasks.clone());
            }
        }

        affected_tasks.sort();
        affected_tasks.dedup();

        let score = if issues.is_empty() {
            1.0
        } else {
            let major_count = issues
                .iter()
                .filter(|i| matches!(i.severity, Severity::Major | Severity::Critical))
                .count();
            1.0 - (major_count as f32 * 0.2).min(1.0)
        };

        let passed = score >= self.config.thresholds.integration_soundness;

        Ok(CoherenceResult {
            check_type: CoherenceCheckType::IntegrationSoundness,
            score,
            passed,
            issues,
            affected_tasks,
        })
    }

    /// Check that all mission requirements are fulfilled using LLM semantic validation.
    /// This replaces naive word overlap with proper semantic understanding.
    async fn check_mission_completion(
        &self,
        task_results: &[CoherenceTaskResult],
        mission_requirements: &[String],
    ) -> Result<CoherenceResult> {
        let mut issues = Vec::new();
        let affected_tasks = Vec::new();

        if mission_requirements.is_empty() {
            return Ok(CoherenceResult {
                check_type: CoherenceCheckType::MissionCompletion,
                score: 1.0,
                passed: true,
                issues: Vec::new(),
                affected_tasks: Vec::new(),
            });
        }

        let task_descriptions: Vec<String> =
            task_results.iter().map(|t| t.description.clone()).collect();

        // Use LLM for semantic validation of requirement fulfillment
        let validation_result = self
            .validate_requirements_with_llm(mission_requirements, &task_descriptions)
            .await;

        let (fulfilled_count, unfulfilled_reqs) = match validation_result {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, "LLM validation failed, using fallback heuristic");
                self.fallback_requirement_check(mission_requirements, &task_descriptions)
            }
        };

        for req in unfulfilled_reqs {
            issues.push(CoherenceIssue {
                check_type: CoherenceCheckType::MissionCompletion,
                description: format!("Requirement not semantically fulfilled: {}", req),
                task_a: None,
                task_b: None,
                severity: Severity::Major,
                suggested_resolution: Some(format!("Add or modify a task to address: {}", req)),
            });
        }

        let score = fulfilled_count as f32 / mission_requirements.len() as f32;
        let passed = score >= self.config.thresholds.mission_completion;

        debug!(
            fulfilled = fulfilled_count,
            total = mission_requirements.len(),
            score = score,
            "Mission completion check"
        );

        Ok(CoherenceResult {
            check_type: CoherenceCheckType::MissionCompletion,
            score,
            passed,
            issues,
            affected_tasks,
        })
    }

    /// Use LLM to semantically validate if tasks fulfill requirements.
    /// Returns (fulfilled_count, unfulfilled_requirements).
    async fn validate_requirements_with_llm(
        &self,
        requirements: &[String],
        task_descriptions: &[String],
    ) -> Result<(usize, Vec<String>)> {
        let tasks_str = task_descriptions
            .iter()
            .enumerate()
            .map(|(i, desc)| format!("{}. {}", i + 1, desc))
            .collect::<Vec<_>>()
            .join("\n");

        let reqs_str = requirements
            .iter()
            .enumerate()
            .map(|(i, req)| format!("{}. {}", i + 1, req))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            r"Evaluate if the completed tasks fulfill the mission requirements.

## Completed Tasks
{tasks}

## Mission Requirements
{reqs}

For each requirement, determine if it is FULFILLED by one or more tasks.
Consider semantic meaning, not just word matching.",
            tasks = tasks_str,
            reqs = reqs_str,
        );

        let model_config = self
            .agent_config
            .model_config_for(ModelAgentType::Validation)?;
        let timeout = Duration::from_secs(self.agent_config.timeout_secs.min(120));

        let plugin_dirs: Vec<std::path::PathBuf> = self
            .agent_config
            .plugin_dirs
            .iter()
            .map(|p| self.working_dir.join(p))
            .collect();

        let agent = SdkAgent::builder()
            .from_claude_code(&self.working_dir)
            .await
            .map_err(|e| PilotError::Planning(format!("Claude Code init failed: {}", e)))?
            .plugin_dirs(plugin_dirs)
            .model(model_config.model_id())
            .tools(ToolAccess::none())
            .timeout(timeout)
            .max_iterations(1)
            .permission_mode(PermissionMode::AcceptEdits)
            .structured_output::<FulfillmentResponse>()
            .build()
            .await
            .map_err(|e| PilotError::Planning(format!("Failed to build agent: {}", e)))?;

        let result = agent
            .execute(&prompt)
            .await
            .map_err(|e| PilotError::Planning(format!("LLM execution failed: {}", e)))?;

        let parsed: FulfillmentResponse = result.extract().map_err(|e| {
            PilotError::Planning(format!("Failed to extract fulfillment response: {}", e))
        })?;

        let mut fulfilled_count = 0;
        let mut unfulfilled = Vec::new();

        for item in parsed.fulfillment {
            let req_idx = item.requirement.saturating_sub(1);
            if item.fulfilled {
                fulfilled_count += 1;
            } else if let Some(req) = requirements.get(req_idx) {
                unfulfilled.push(req.clone());
            }
        }

        Ok((fulfilled_count, unfulfilled))
    }

    /// Fallback word-based check when LLM is unavailable.
    /// Uses stricter threshold (70%) than before (50%).
    fn fallback_requirement_check(
        &self,
        requirements: &[String],
        task_descriptions: &[String],
    ) -> (usize, Vec<String>) {
        let task_words: HashSet<String> = task_descriptions
            .iter()
            .flat_map(|d| {
                d.to_lowercase()
                    .split_whitespace()
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .collect();

        let mut fulfilled = 0;
        let mut unfulfilled = Vec::new();

        for req in requirements {
            let req_lower = req.to_lowercase();
            let req_words: Vec<&str> = req_lower.split_whitespace().collect();
            let overlap = req_words
                .iter()
                .filter(|w| task_words.contains::<str>(w))
                .count();

            // Use configurable threshold (default 70%, stricter than old 50%)
            let threshold =
                (req_words.len() as f32 * self.config.thresholds.word_overlap_fallback) as usize;
            if overlap >= threshold.max(1) {
                fulfilled += 1;
            } else {
                unfulfilled.push(req.clone());
            }
        }

        (fulfilled, unfulfilled)
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FulfillmentResponse {
    #[serde(default)]
    fulfillment: Vec<FulfillmentItem>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FulfillmentItem {
    requirement: usize,
    fulfilled: bool,
}

/// Result of a completed task for coherence checking.
#[derive(Debug, Clone)]
pub struct CoherenceTaskResult {
    pub task_id: String,
    pub description: String,
    pub file_changes: Vec<FileChange>,
    pub success: bool,
}

impl CoherenceTaskResult {
    pub fn new(task_id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            description: description.into(),
            file_changes: Vec::new(),
            success: true,
        }
    }

    pub fn with_file_changes(mut self, changes: Vec<FileChange>) -> Self {
        self.file_changes = changes;
        self
    }
}

/// Full coherence check report.
#[derive(Debug, Clone)]
pub struct CoherenceReport {
    pub contract_consistency: CoherenceResult,
    pub integration_soundness: CoherenceResult,
    pub mission_completion: CoherenceResult,
    pub overall_passed: bool,
}

impl CoherenceReport {
    pub fn all_issues(&self) -> Vec<&CoherenceIssue> {
        let mut issues = Vec::new();
        issues.extend(&self.contract_consistency.issues);
        issues.extend(&self.integration_soundness.issues);
        issues.extend(&self.mission_completion.issues);
        issues
    }

    pub fn summary(&self) -> String {
        format!(
            "Coherence Report: {} | Contract Consistency: {:.0}% | Integration: {:.0}% | Completion: {:.0}%",
            if self.overall_passed {
                "PASSED"
            } else {
                "FAILED"
            },
            self.contract_consistency.score * 100.0,
            self.integration_soundness.score * 100.0,
            self.mission_completion.score * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use std::path::Path;

    fn create_test_checker() -> CoherenceChecker {
        CoherenceChecker::new(
            Path::new("/tmp"),
            CoherenceConfig::default(),
            AgentConfig::default(),
        )
    }

    #[tokio::test]
    async fn test_contract_consistency_empty_tasks() {
        let checker = create_test_checker();

        let result = checker.check_contract_consistency(&[]).await.unwrap();
        assert!(result.passed);
        assert!(result.issues.is_empty());
    }

    #[tokio::test]
    async fn test_mission_completion_fallback() {
        // This test uses the fallback word-based validation
        // since LLM is not available in tests
        let checker = create_test_checker();

        let tasks = vec![
            CoherenceTaskResult::new("task1", "Implement user authentication system"),
            CoherenceTaskResult::new("task2", "Add login page with form"),
        ];

        let requirements = vec!["User authentication".to_string(), "Login page".to_string()];

        let result = checker
            .check_mission_completion(&tasks, &requirements)
            .await
            .unwrap();
        // Fallback should still find matches based on word overlap
        assert!(result.score >= 0.5);
    }

    #[tokio::test]
    async fn test_mission_completion_missing() {
        let checker = create_test_checker();

        let tasks = vec![CoherenceTaskResult::new("task1", "Implement user service")];

        let requirements = vec![
            "User service".to_string(),
            "Admin dashboard".to_string(),
            "Report generation".to_string(),
        ];

        let result = checker
            .check_mission_completion(&tasks, &requirements)
            .await
            .unwrap();
        assert!(!result.passed);
        assert!(result.score < 0.5);
    }
}
