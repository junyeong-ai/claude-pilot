//! Human escalation handling for durable execution.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::types::FailureAnalysis;
use crate::error::Result;
use crate::mission::Mission;
use crate::notification::Notifier;
use crate::verification::Issue;

/// Context provided when escalating to human intervention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationContext {
    pub id: String,
    pub mission_id: String,
    pub summary: String,
    pub analysis: Option<FailureAnalysis>,
    pub attempted_strategies: Vec<AttemptedStrategy>,
    pub persistent_issues: Vec<Issue>,
    pub relevant_files: Vec<PathBuf>,
    pub suggested_actions: Vec<HumanAction>,
    pub status: EscalationStatus,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution: Option<HumanResponse>,
}

impl EscalationContext {
    pub fn new(mission_id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: format!("esc-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            mission_id: mission_id.into(),
            summary: summary.into(),
            analysis: None,
            attempted_strategies: Vec::new(),
            persistent_issues: Vec::new(),
            relevant_files: Vec::new(),
            suggested_actions: vec![HumanAction::Cancel],
            status: EscalationStatus::Pending,
            created_at: Utc::now(),
            resolved_at: None,
            resolution: None,
        }
    }

    pub fn with_analysis(mut self, analysis: FailureAnalysis) -> Self {
        self.analysis = Some(analysis);
        self
    }

    pub fn with_strategies(mut self, strategies: Vec<AttemptedStrategy>) -> Self {
        self.attempted_strategies = strategies;
        self
    }

    pub fn with_issues(mut self, issues: Vec<Issue>) -> Self {
        self.persistent_issues = issues;
        self
    }

    pub fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.relevant_files = files;
        self
    }

    pub fn add_action(&mut self, action: HumanAction) {
        if !self
            .suggested_actions
            .iter()
            .any(|a| a.label() == action.label())
        {
            self.suggested_actions
                .insert(self.suggested_actions.len().saturating_sub(1), action);
        }
    }

    pub fn resolve(&mut self, response: HumanResponse) {
        self.status = EscalationStatus::Resolved;
        self.resolved_at = Some(Utc::now());
        self.resolution = Some(response);
    }

    pub fn dismiss(&mut self, reason: impl Into<String>) {
        self.status = EscalationStatus::Dismissed;
        self.resolved_at = Some(Utc::now());
        // Store the dismissal reason in the summary for audit purposes
        self.summary = format!("{} [Dismissed: {}]", self.summary, reason.into());
        self.resolution = Some(HumanResponse::Cancelled);
    }

    pub fn is_resolved(&self) -> bool {
        matches!(
            self.status,
            EscalationStatus::Resolved | EscalationStatus::Dismissed
        )
    }

    pub fn to_message(&self) -> String {
        let actions = if self.suggested_actions.is_empty() {
            "No suggested actions".to_string()
        } else {
            self.suggested_actions
                .iter()
                .enumerate()
                .map(|(i, a)| format!("  {}. {}", i + 1, a.display_text()))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let issues = if self.persistent_issues.is_empty() {
            "None".to_string()
        } else {
            self.persistent_issues
                .iter()
                .take(5)
                .map(|i| format!("  - {}", i.message))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let category = self
            .analysis
            .as_ref()
            .map(|a| a.category.to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        let phase = self
            .analysis
            .as_ref()
            .map(|a| a.phase.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        format!(
            r"
╔══════════════════════════════════════════════════════════════╗
║                    ESCALATION REQUIRED                       ║
╠══════════════════════════════════════════════════════════════╣
║ Mission: {}
║ Escalation ID: {}
║ Category: {}
║ Phase: {}
╠══════════════════════════════════════════════════════════════╣
║ Summary:
║ {}
╠══════════════════════════════════════════════════════════════╣
║ Persistent Issues:
{}
╠══════════════════════════════════════════════════════════════╣
║ Suggested Actions:
{}
╚══════════════════════════════════════════════════════════════╝
",
            self.mission_id, self.id, category, phase, self.summary, issues, actions
        )
    }
}

/// Record of a strategy that was attempted before escalation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptedStrategy {
    pub name: String,
    pub level: u8,
    pub rounds: u32,
    pub outcome: StrategyOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyOutcome {
    Success,
    PartialSuccess,
    Failed,
    Stagnated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    Pending,
    Acknowledged,
    InProgress,
    Resolved,
    Dismissed,
}

impl std::fmt::Display for EscalationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Acknowledged => write!(f, "acknowledged"),
            Self::InProgress => write!(f, "in progress"),
            Self::Resolved => write!(f, "resolved"),
            Self::Dismissed => write!(f, "dismissed"),
        }
    }
}

/// Action that a human can take to resolve an escalation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HumanAction {
    ProvideInfo { question: String },
    ManualFix { file: PathBuf, hint: String },
    ReduceScope { task_id: String, reason: String },
    AcceptPartial { remaining_issues: usize },
    Retry,
    Cancel,
}

impl HumanAction {
    pub fn display_text(&self) -> String {
        match self {
            Self::ProvideInfo { question } => format!("Provide info: {}", question),
            Self::ManualFix { file, .. } => format!("Manual fix: {}", file.display()),
            Self::ReduceScope { task_id, .. } => format!("Skip task: {}", task_id),
            Self::AcceptPartial { remaining_issues } => {
                format!("Accept with {} remaining issues", remaining_issues)
            }
            Self::Retry => "Retry from last checkpoint".to_string(),
            Self::Cancel => "Cancel mission".to_string(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ProvideInfo { .. } => "provide-info",
            Self::ManualFix { .. } => "manual-fix",
            Self::ReduceScope { .. } => "reduce-scope",
            Self::AcceptPartial { .. } => "accept-partial",
            Self::Retry => "retry",
            Self::Cancel => "cancel",
        }
    }
}

/// Human response to an escalation.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HumanResponse {
    ProvidedInfo { info: String },
    ManuallyFixed { description: String },
    AcceptedReducedScope { skipped_task: String },
    AcceptedPartial,
    RetryRequested,
    Cancelled,
}

impl HumanResponse {
    pub fn should_continue(&self) -> bool {
        !matches!(self, Self::Cancelled)
    }

    pub fn should_retry(&self) -> bool {
        matches!(
            self,
            Self::ProvidedInfo { .. } | Self::ManuallyFixed { .. } | Self::RetryRequested
        )
    }
}

/// Urgency level for escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationUrgency {
    Low,
    Medium,
    High,
    Critical,
}

impl EscalationUrgency {
    pub fn from_issue_severity(severity: crate::verification::IssueSeverity) -> Self {
        match severity {
            crate::verification::IssueSeverity::Info => Self::Low,
            crate::verification::IssueSeverity::Warning => Self::Medium,
            crate::verification::IssueSeverity::Error => Self::High,
            crate::verification::IssueSeverity::Critical => Self::Critical,
        }
    }
}

/// Handler for creating and managing escalations.
pub struct EscalationHandler {
    notifier: Option<Notifier>,
}

impl EscalationHandler {
    pub fn new(notifier: Option<Notifier>) -> Self {
        Self { notifier }
    }

    pub async fn escalate(
        &self,
        mission: &Mission,
        reason: &str,
        analysis: FailureAnalysis,
        suggested_actions: Vec<String>,
    ) -> Result<EscalationContext> {
        let mut ctx = EscalationContext::new(&mission.id, reason).with_analysis(analysis);

        for action_str in suggested_actions {
            ctx.add_action(HumanAction::ProvideInfo {
                question: action_str,
            });
        }

        warn!(
            mission_id = mission.id,
            escalation_id = ctx.id,
            reason = reason,
            "Escalation created"
        );

        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_escalation(&mission.id, &ctx.to_message())
                .await?;
        }

        Ok(ctx)
    }

    pub async fn notify_user(&self, ctx: &EscalationContext) -> Result<()> {
        eprintln!("{}", ctx.to_message());

        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_escalation(&ctx.mission_id, &ctx.to_message())
                .await?;
        }

        Ok(())
    }
}
