use crate::config::PilotConfig;
use crate::mission::{IsolationMode, Mission, RiskLevel};
use crate::state::MissionState;

pub struct IsolationPlanner<'a> {
    config: &'a PilotConfig,
    active_missions: Vec<&'a Mission>,
}

impl<'a> IsolationPlanner<'a> {
    pub fn new(config: &'a PilotConfig, active_missions: Vec<&'a Mission>) -> Self {
        Self {
            config,
            active_missions,
        }
    }

    pub fn determine_isolation(&self, mission: &Mission, estimated_files: usize) -> IsolationMode {
        if mission.flags.isolated {
            return IsolationMode::Worktree;
        }

        if mission.flags.direct {
            return IsolationMode::None;
        }

        if mission.isolation != IsolationMode::Auto {
            return mission.isolation;
        }

        if self.has_active_missions() {
            return IsolationMode::Worktree;
        }

        self.isolation_by_scope(estimated_files)
    }

    fn has_active_missions(&self) -> bool {
        self.active_missions
            .iter()
            .any(|m| m.status == MissionState::Running)
    }

    fn isolation_by_scope(&self, estimated_files: usize) -> IsolationMode {
        let small = self.config.isolation.small_change_threshold;
        let large = self.config.isolation.large_change_threshold;

        match estimated_files {
            n if n <= small => IsolationMode::None,
            n if n <= large => IsolationMode::Branch,
            _ => IsolationMode::Worktree,
        }
    }
}

pub struct ScopeAnalysis {
    pub estimated_files: usize,
    pub risk_level: RiskLevel,
    pub reasons: Vec<String>,
}

impl ScopeAnalysis {
    /// Neutral defaults until evidence is gathered.
    pub fn analyze(_description: &str) -> Self {
        Self::default()
    }

    /// Accurate scope from gathered evidence.
    pub fn from_evidence(evidence: &crate::planning::Evidence) -> Self {
        let file_count = evidence.codebase_analysis.relevant_files.len();

        let risk_level = if file_count > 20 || !evidence.completeness.is_complete {
            RiskLevel::High
        } else if file_count > 5 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        let mut reasons = vec![format!("{} files identified", file_count)];
        if !evidence.completeness.is_complete {
            reasons.push(format!(
                "Incomplete: {}/{}",
                evidence.completeness.files_found, evidence.completeness.files_limit
            ));
        }

        Self {
            estimated_files: file_count,
            risk_level,
            reasons,
        }
    }
}

impl Default for ScopeAnalysis {
    fn default() -> Self {
        Self {
            estimated_files: 3,
            risk_level: RiskLevel::Medium,
            reasons: vec!["Pending evidence".to_string()],
        }
    }
}
