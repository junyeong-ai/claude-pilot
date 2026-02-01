use std::collections::{HashMap, HashSet};
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::consensus::{Conflict, ConflictSeverity, ConsensusResult, ConsensusTask};
use crate::error::{PilotError, Result};

/// Escalation levels for conflict resolution, ordered by severity (lower = less severe).
///
/// The escalation process follows this progression:
/// 1. **Retry** (0): Simple retry with different agents
/// 2. **ScopeReduction** (1): Reduce scope by executing non-conflicting tasks only
/// 3. **ArchitecturalMediation** (2): Request architect agent to mediate
/// 4. **SplitAndConquer** (3): Split task into smaller, independent units
/// 5. **HumanEscalation** (4): Request human intervention (terminal level)
///
/// Use `next()` to progress to the next escalation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationLevel {
    Retry = 0,
    ScopeReduction = 1,
    ArchitecturalMediation = 2,
    SplitAndConquer = 3,
    HumanEscalation = 4,
}

impl EscalationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::ScopeReduction => "scope_reduction",
            Self::ArchitecturalMediation => "architectural_mediation",
            Self::SplitAndConquer => "split_and_conquer",
            Self::HumanEscalation => "human_escalation",
        }
    }

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Retry => Some(Self::ScopeReduction),
            Self::ScopeReduction => Some(Self::ArchitecturalMediation),
            Self::ArchitecturalMediation => Some(Self::SplitAndConquer),
            Self::SplitAndConquer => Some(Self::HumanEscalation),
            Self::HumanEscalation => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum EscalationStrategy {
    RetryWithDifferentAgents {
        exclude_agents: Vec<String>,
        modified_context: Option<String>,
    },
    ExecuteNonConflicting {
        safe_tasks: Vec<ConsensusTask>,
        deferred_tasks: Vec<ConsensusTask>,
    },
    ArchitecturalOverride {
        decision_prompt: String,
        affected_modules: Vec<String>,
    },
    SequentializeConflicts {
        task_order: Vec<String>,
        rationale: String,
    },
    RequestHumanDecision {
        question: String,
        options: Vec<EscalationOption>,
        context: ConflictContext,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationOption {
    pub id: String,
    pub description: String,
    pub agent_id: Option<String>,
    pub trade_offs: String,
}

#[derive(Debug, Clone)]
pub struct ConflictContext {
    pub conflict_summary: String,
    pub affected_files: Vec<String>,
    pub agent_positions: HashMap<String, String>,
    pub attempted_resolutions: Vec<AttemptedResolution>,
}

#[derive(Debug, Clone)]
pub struct AttemptedResolution {
    pub level: EscalationLevel,
    pub strategy: String,
    pub outcome: String,
}

#[derive(Debug, Default)]
pub struct EscalationHistory {
    mission_attempts: RwLock<HashMap<String, Vec<EscalationLevel>>>,
}

impl EscalationHistory {
    pub fn record(&self, mission_id: &str, level: EscalationLevel) {
        self.mission_attempts
            .write()
            .entry(mission_id.to_string())
            .or_default()
            .push(level);
    }

    pub fn attempted_levels(&self, mission_id: &str) -> HashSet<EscalationLevel> {
        self.mission_attempts
            .read()
            .get(mission_id)
            .map(|levels| levels.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn attempt_count(&self, mission_id: &str, level: EscalationLevel) -> usize {
        self.mission_attempts
            .read()
            .get(mission_id)
            .map(|levels| levels.iter().filter(|&&l| l == level).count())
            .unwrap_or(0)
    }

    pub fn get_attempts(&self, mission_id: &str) -> Option<Vec<EscalationLevel>> {
        self.mission_attempts.read().get(mission_id).cloned()
    }
}

#[derive(Debug, Clone)]
pub struct EscalationConfig {
    pub max_retries_per_level: usize,
    pub enable_human_escalation: bool,
    pub human_escalation_timeout: Duration,
    pub fallback_on_human_timeout: FallbackStrategy,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            max_retries_per_level: 2,
            enable_human_escalation: true,
            human_escalation_timeout: Duration::from_secs(86400), // 24 hours
            fallback_on_human_timeout: FallbackStrategy::AbortMission,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStrategy {
    AbortMission,
    UseFirstOption,
    UseSafestOption,
    RetryWithoutConflictingFiles,
}

impl FallbackStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AbortMission => "abort_mission",
            Self::UseFirstOption => "use_first_option",
            Self::UseSafestOption => "use_safest_option",
            Self::RetryWithoutConflictingFiles => "retry_without_conflicts",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HumanDecision {
    pub selected_option_id: String,
    pub additional_context: Option<String>,
}

struct ConsensusData {
    tasks: Vec<ConsensusTask>,
    conflicts: Vec<Conflict>,
    has_blocking: bool,
}

impl ConsensusData {
    fn from_result(result: &ConsensusResult) -> Self {
        match result {
            ConsensusResult::Agreed { tasks, .. } => Self {
                tasks: tasks.clone(),
                conflicts: vec![],
                has_blocking: false,
            },
            ConsensusResult::PartialAgreement {
                unresolved_conflicts,
                ..
            } => Self {
                tasks: vec![],
                conflicts: unresolved_conflicts.clone(),
                has_blocking: unresolved_conflicts
                    .iter()
                    .any(|c| c.severity == ConflictSeverity::Blocking),
            },
            ConsensusResult::NoConsensus {
                blocking_conflicts, ..
            } => Self {
                tasks: vec![],
                conflicts: blocking_conflicts.clone(),
                has_blocking: blocking_conflicts
                    .iter()
                    .any(|c| c.severity == ConflictSeverity::Blocking),
            },
        }
    }
}

pub struct EscalationEngine {
    config: EscalationConfig,
    history: EscalationHistory,
}

impl EscalationEngine {
    pub fn new(config: EscalationConfig) -> Self {
        Self {
            config,
            history: EscalationHistory::default(),
        }
    }

    pub fn escalate(
        &self,
        mission_id: &str,
        result: &ConsensusResult,
    ) -> Result<EscalationStrategy> {
        let data = ConsensusData::from_result(result);
        let level = self.determine_level(mission_id, &data);
        self.history.record(mission_id, level);

        info!(
            level = ?level,
            conflicts = data.conflicts.len(),
            mission_id = %mission_id,
            "Escalating consensus failure"
        );

        match level {
            EscalationLevel::Retry => self.build_retry_strategy(&data),
            EscalationLevel::ScopeReduction => self.build_scope_reduction_strategy(&data),
            EscalationLevel::ArchitecturalMediation => self.build_architectural_mediation(&data),
            EscalationLevel::SplitAndConquer => self.build_split_strategy(&data),
            EscalationLevel::HumanEscalation => self.build_human_escalation(mission_id, &data),
        }
    }

    fn determine_level(&self, mission_id: &str, data: &ConsensusData) -> EscalationLevel {
        let levels = [
            EscalationLevel::Retry,
            EscalationLevel::ScopeReduction,
            EscalationLevel::ArchitecturalMediation,
            EscalationLevel::SplitAndConquer,
            EscalationLevel::HumanEscalation,
        ];

        for level in levels {
            if level == EscalationLevel::HumanEscalation && !self.config.enable_human_escalation {
                continue;
            }

            let attempts = self.history.attempt_count(mission_id, level);
            if attempts < self.config.max_retries_per_level {
                if level == EscalationLevel::Retry && data.has_blocking {
                    continue;
                }
                return level;
            }
        }

        EscalationLevel::HumanEscalation
    }

    fn build_retry_strategy(&self, data: &ConsensusData) -> Result<EscalationStrategy> {
        let exclude: Vec<String> = data
            .conflicts
            .iter()
            .flat_map(|c| c.agents.iter().cloned())
            .take(1)
            .collect();

        debug!(excluded = ?exclude, "Building retry strategy");

        Ok(EscalationStrategy::RetryWithDifferentAgents {
            exclude_agents: exclude,
            modified_context: None,
        })
    }

    fn build_scope_reduction_strategy(&self, data: &ConsensusData) -> Result<EscalationStrategy> {
        let conflicting_files: HashSet<String> = data
            .conflicts
            .iter()
            .flat_map(|c| c.positions.iter().filter(|p| p.contains('/')).cloned())
            .collect();

        let (safe_tasks, deferred_tasks): (Vec<_>, Vec<_>) =
            data.tasks.iter().cloned().partition(|task| {
                !task
                    .files_affected
                    .iter()
                    .any(|f| conflicting_files.contains(f))
            });

        debug!(
            safe = safe_tasks.len(),
            deferred = deferred_tasks.len(),
            "Building scope reduction strategy"
        );

        Ok(EscalationStrategy::ExecuteNonConflicting {
            safe_tasks,
            deferred_tasks,
        })
    }

    fn build_architectural_mediation(&self, data: &ConsensusData) -> Result<EscalationStrategy> {
        let conflict_descriptions: Vec<String> = data
            .conflicts
            .iter()
            .map(|c| {
                format!(
                    "- {} (severity: {:?}, agents: {:?})",
                    c.topic, c.severity, c.agents
                )
            })
            .collect();

        let affected_modules: Vec<String> = data
            .conflicts
            .iter()
            .flat_map(|c| c.agents.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let decision_prompt = format!(
            r#"## Architectural Mediation Required

### Conflicts
{conflicts}

### Affected Modules
{modules}

As the architectural authority, make a binding decision:
1. Which approach should be taken and why?
2. What are the architectural justifications?
3. Any required follow-up changes?

Output JSON: {{"decision": "...", "justification": "...", "follow_ups": [...]}}"#,
            conflicts = conflict_descriptions.join("\n"),
            modules = affected_modules.join(", "),
        );

        Ok(EscalationStrategy::ArchitecturalOverride {
            decision_prompt,
            affected_modules,
        })
    }

    fn build_split_strategy(&self, data: &ConsensusData) -> Result<EscalationStrategy> {
        let mut ordered: Vec<_> = data.tasks.iter().collect();
        ordered.sort_by(|a, b| {
            let a_deps = a.dependencies.len();
            let b_deps = b.dependencies.len();
            a_deps
                .cmp(&b_deps)
                .then_with(|| a.priority.cmp(&b.priority).reverse())
        });

        let task_order: Vec<String> = ordered.iter().map(|t| t.id.clone()).collect();

        debug!(order = ?task_order, "Building split and conquer strategy");

        Ok(EscalationStrategy::SequentializeConflicts {
            task_order,
            rationale: "Ordering by dependency count and priority to minimize conflicts".into(),
        })
    }

    fn build_human_escalation(
        &self,
        mission_id: &str,
        data: &ConsensusData,
    ) -> Result<EscalationStrategy> {
        let options: Vec<EscalationOption> = data
            .conflicts
            .iter()
            .flat_map(|conflict| {
                conflict.agents.iter().map(|agent| EscalationOption {
                    id: agent.clone(),
                    description: format!("Accept {}'s approach", agent),
                    agent_id: Some(agent.clone()),
                    trade_offs: "May conflict with other modules' expectations".into(),
                })
            })
            .collect();

        let attempted_resolutions: Vec<AttemptedResolution> = self
            .history
            .get_attempts(mission_id)
            .map(|levels| {
                levels
                    .iter()
                    .map(|&level| AttemptedResolution {
                        level,
                        strategy: format!("{:?}", level),
                        outcome: "Failed to resolve conflicts".into(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let context = ConflictContext {
            conflict_summary: data
                .conflicts
                .iter()
                .map(|c| c.topic.clone())
                .collect::<Vec<_>>()
                .join("; "),
            affected_files: data
                .conflicts
                .iter()
                .flat_map(|c| c.positions.iter().filter(|p| p.contains('/')).cloned())
                .collect(),
            agent_positions: data
                .conflicts
                .iter()
                .flat_map(|c| {
                    c.agents
                        .iter()
                        .zip(c.positions.iter())
                        .map(|(a, p)| (a.clone(), p.clone()))
                })
                .collect(),
            attempted_resolutions,
        };

        Ok(EscalationStrategy::RequestHumanDecision {
            question: "Multiple approaches exist with no consensus. Please choose:".into(),
            options,
            context,
        })
    }

    pub fn history(&self) -> &EscalationHistory {
        &self.history
    }

    pub async fn escalate_with_timeout(
        &self,
        mission_id: &str,
        result: &ConsensusResult,
        human_decision_rx: Option<watch::Receiver<Option<HumanDecision>>>,
    ) -> Result<EscalationStrategy> {
        let strategy = self.escalate(mission_id, result)?;

        if let EscalationStrategy::RequestHumanDecision {
            ref options,
            ref context,
            ..
        } = strategy
            && let Some(mut rx) = human_decision_rx
        {
            match tokio::time::timeout(self.config.human_escalation_timeout, async {
                loop {
                    rx.changed().await.ok()?;
                    if rx.borrow().is_some() {
                        return rx.borrow().clone();
                    }
                }
            })
            .await
            {
                Ok(Some(decision)) => {
                    return self.apply_human_decision(&decision, options);
                }
                Ok(None) => {
                    warn!(
                        mission_id = %mission_id,
                        "Human decision channel closed"
                    );
                }
                Err(_) => {
                    warn!(
                        mission_id = %mission_id,
                        timeout_secs = self.config.human_escalation_timeout.as_secs(),
                        fallback = self.config.fallback_on_human_timeout.as_str(),
                        "Human escalation timed out, using fallback"
                    );
                }
            }

            return self.apply_fallback(options, context);
        }

        Ok(strategy)
    }

    fn apply_human_decision(
        &self,
        decision: &HumanDecision,
        options: &[EscalationOption],
    ) -> Result<EscalationStrategy> {
        let selected = options
            .iter()
            .find(|o| o.id == decision.selected_option_id)
            .ok_or_else(|| {
                PilotError::Escalation(format!(
                    "Invalid option id: {}",
                    decision.selected_option_id
                ))
            })?;

        if let Some(agent_id) = &selected.agent_id {
            Ok(EscalationStrategy::RetryWithDifferentAgents {
                exclude_agents: options
                    .iter()
                    .filter(|o| o.agent_id.as_ref() != Some(agent_id))
                    .filter_map(|o| o.agent_id.clone())
                    .collect(),
                modified_context: decision.additional_context.clone(),
            })
        } else {
            Ok(EscalationStrategy::ExecuteNonConflicting {
                safe_tasks: vec![],
                deferred_tasks: vec![],
            })
        }
    }

    fn apply_fallback(
        &self,
        options: &[EscalationOption],
        context: &ConflictContext,
    ) -> Result<EscalationStrategy> {
        match self.config.fallback_on_human_timeout {
            FallbackStrategy::AbortMission => Err(PilotError::Escalation(
                "Human decision required but timed out".into(),
            )),
            FallbackStrategy::UseFirstOption => {
                if let Some(first) = options.first() {
                    let decision = HumanDecision {
                        selected_option_id: first.id.clone(),
                        additional_context: Some("Auto-selected due to timeout".into()),
                    };
                    self.apply_human_decision(&decision, options)
                } else {
                    Err(PilotError::Escalation(
                        "No options available for fallback".into(),
                    ))
                }
            }
            FallbackStrategy::UseSafestOption => {
                let safest = options
                    .iter()
                    .min_by_key(|_| context.affected_files.len())
                    .or(options.first());

                if let Some(option) = safest {
                    let decision = HumanDecision {
                        selected_option_id: option.id.clone(),
                        additional_context: Some(
                            "Auto-selected safest option due to timeout".into(),
                        ),
                    };
                    self.apply_human_decision(&decision, options)
                } else {
                    Err(PilotError::Escalation(
                        "No options available for fallback".into(),
                    ))
                }
            }
            FallbackStrategy::RetryWithoutConflictingFiles => {
                Ok(EscalationStrategy::ExecuteNonConflicting {
                    safe_tasks: vec![],
                    deferred_tasks: vec![],
                })
            }
        }
    }
}

impl Default for EscalationEngine {
    fn default() -> Self {
        Self::new(EscalationConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_no_consensus(with_blocking: bool) -> ConsensusResult {
        let conflicts = if with_blocking {
            vec![Conflict {
                id: "c1".into(),
                agents: vec!["auth".into(), "api".into()],
                topic: "Test conflict".into(),
                positions: vec!["Position A".into(), "Position B".into()],
                severity: ConflictSeverity::Blocking,
                resolution: None,
            }]
        } else {
            vec![Conflict {
                id: "c1".into(),
                agents: vec!["auth".into(), "api".into()],
                topic: "Test conflict".into(),
                positions: vec!["Position A".into(), "Position B".into()],
                severity: ConflictSeverity::Minor,
                resolution: None,
            }]
        };

        ConsensusResult::NoConsensus {
            summary: "No consensus reached".into(),
            blocking_conflicts: conflicts,
            respondent_count: 2,
        }
    }

    #[test]
    fn test_escalation_level_progression() {
        let engine = EscalationEngine::new(EscalationConfig {
            max_retries_per_level: 1,
            enable_human_escalation: true,
            ..Default::default()
        });

        let result = mock_no_consensus(false);

        // First escalation should be Retry (non-blocking)
        let strategy = engine.escalate("mission-1", &result).unwrap();
        assert!(matches!(
            strategy,
            EscalationStrategy::RetryWithDifferentAgents { .. }
        ));

        // Second should be ScopeReduction
        let strategy = engine.escalate("mission-1", &result).unwrap();
        assert!(matches!(
            strategy,
            EscalationStrategy::ExecuteNonConflicting { .. }
        ));

        // Third should be ArchitecturalMediation
        let strategy = engine.escalate("mission-1", &result).unwrap();
        assert!(matches!(
            strategy,
            EscalationStrategy::ArchitecturalOverride { .. }
        ));
    }

    #[test]
    fn test_blocking_skips_retry() {
        let engine = EscalationEngine::new(EscalationConfig {
            max_retries_per_level: 2,
            enable_human_escalation: true,
            ..Default::default()
        });

        let result = mock_no_consensus(true);

        // With blocking conflict, should skip Retry
        let strategy = engine.escalate("mission-1", &result).unwrap();
        assert!(matches!(
            strategy,
            EscalationStrategy::ExecuteNonConflicting { .. }
        ));
    }

    #[test]
    fn test_fallback_strategy_as_str() {
        assert_eq!(FallbackStrategy::AbortMission.as_str(), "abort_mission");
        assert_eq!(
            FallbackStrategy::UseFirstOption.as_str(),
            "use_first_option"
        );
        assert_eq!(
            FallbackStrategy::UseSafestOption.as_str(),
            "use_safest_option"
        );
        assert_eq!(
            FallbackStrategy::RetryWithoutConflictingFiles.as_str(),
            "retry_without_conflicts"
        );
    }
}
