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
    pub fn from_severity(severity: crate::domain::Severity) -> Self {
        match severity {
            crate::domain::Severity::Info => Self::Low,
            crate::domain::Severity::Warning => Self::Medium,
            crate::domain::Severity::Error => Self::High,
            crate::domain::Severity::Critical => Self::Critical,
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
        warn!(mission_id = %ctx.mission_id, "{}", ctx.to_message());

        if let Some(ref notifier) = self.notifier {
            notifier
                .notify_escalation(&ctx.mission_id, &ctx.to_message())
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;

    // ── EscalationContext lifecycle ──

    #[test]
    fn test_escalation_context_new() {
        let ctx = EscalationContext::new("mission-1", "Something went wrong");
        assert!(ctx.id.starts_with("esc-"));
        assert_eq!(ctx.mission_id, "mission-1");
        assert_eq!(ctx.summary, "Something went wrong");
        assert!(ctx.analysis.is_none());
        assert!(ctx.attempted_strategies.is_empty());
        assert!(ctx.persistent_issues.is_empty());
        assert!(ctx.relevant_files.is_empty());
        // Default action is Cancel
        assert_eq!(ctx.suggested_actions.len(), 1);
        assert_eq!(ctx.suggested_actions[0].label(), "cancel");
        assert_eq!(ctx.status, EscalationStatus::Pending);
        assert!(ctx.resolved_at.is_none());
        assert!(ctx.resolution.is_none());
    }

    #[test]
    fn test_escalation_context_with_strategies() {
        let strategies = vec![AttemptedStrategy {
            name: "retry".into(),
            level: 1,
            rounds: 3,
            outcome: StrategyOutcome::Failed,
        }];
        let ctx = EscalationContext::new("m1", "fail").with_strategies(strategies);
        assert_eq!(ctx.attempted_strategies.len(), 1);
        assert_eq!(ctx.attempted_strategies[0].outcome, StrategyOutcome::Failed);
    }

    #[test]
    fn test_add_action_no_duplicates() {
        let mut ctx = EscalationContext::new("m1", "fail");
        // Cancel is the default
        assert_eq!(ctx.suggested_actions.len(), 1);

        ctx.add_action(HumanAction::Retry);
        assert_eq!(ctx.suggested_actions.len(), 2);

        // Adding Retry again should be a no-op (same label)
        ctx.add_action(HumanAction::Retry);
        assert_eq!(ctx.suggested_actions.len(), 2);
    }

    #[test]
    fn test_add_action_inserts_before_cancel() {
        let mut ctx = EscalationContext::new("m1", "fail");
        ctx.add_action(HumanAction::Retry);
        // Retry should be inserted before Cancel
        assert_eq!(ctx.suggested_actions[0].label(), "retry");
        assert_eq!(ctx.suggested_actions[1].label(), "cancel");
    }

    #[test]
    fn test_resolve() {
        let mut ctx = EscalationContext::new("m1", "fail");
        assert!(!ctx.is_resolved());

        ctx.resolve(HumanResponse::RetryRequested);
        assert!(ctx.is_resolved());
        assert_eq!(ctx.status, EscalationStatus::Resolved);
        assert!(ctx.resolved_at.is_some());
        assert!(ctx.resolution.is_some());
    }

    #[test]
    fn test_dismiss() {
        let mut ctx = EscalationContext::new("m1", "fail");
        ctx.dismiss("Not relevant");
        assert!(ctx.is_resolved());
        assert_eq!(ctx.status, EscalationStatus::Dismissed);
        assert!(ctx.summary.contains("[Dismissed: Not relevant]"));
        assert!(matches!(ctx.resolution, Some(HumanResponse::Cancelled)));
    }

    #[test]
    fn test_to_message_contains_key_sections() {
        let ctx = EscalationContext::new("mission-42", "Build keeps failing");
        let msg = ctx.to_message();
        assert!(msg.contains("ESCALATION REQUIRED"));
        assert!(msg.contains("mission-42"));
        assert!(msg.contains("Build keeps failing"));
        assert!(msg.contains("Suggested Actions"));
    }

    // ── HumanAction ──

    #[test]
    fn test_human_action_labels() {
        assert_eq!(
            HumanAction::ProvideInfo {
                question: "q".into()
            }
            .label(),
            "provide-info"
        );
        assert_eq!(
            HumanAction::ManualFix {
                file: PathBuf::from("f"),
                hint: "h".into()
            }
            .label(),
            "manual-fix"
        );
        assert_eq!(
            HumanAction::ReduceScope {
                task_id: "t".into(),
                reason: "r".into()
            }
            .label(),
            "reduce-scope"
        );
        assert_eq!(
            HumanAction::AcceptPartial {
                remaining_issues: 3
            }
            .label(),
            "accept-partial"
        );
        assert_eq!(HumanAction::Retry.label(), "retry");
        assert_eq!(HumanAction::Cancel.label(), "cancel");
    }

    #[test]
    fn test_human_action_display_text() {
        let action = HumanAction::ProvideInfo {
            question: "What is the API key?".into(),
        };
        assert_eq!(action.display_text(), "Provide info: What is the API key?");

        let action = HumanAction::ManualFix {
            file: PathBuf::from("src/lib.rs"),
            hint: "fix it".into(),
        };
        assert_eq!(action.display_text(), "Manual fix: src/lib.rs");

        let action = HumanAction::ReduceScope {
            task_id: "task-5".into(),
            reason: "too complex".into(),
        };
        assert_eq!(action.display_text(), "Skip task: task-5");

        let action = HumanAction::AcceptPartial {
            remaining_issues: 2,
        };
        assert_eq!(action.display_text(), "Accept with 2 remaining issues");

        assert_eq!(HumanAction::Retry.display_text(), "Retry from last checkpoint");
        assert_eq!(HumanAction::Cancel.display_text(), "Cancel mission");
    }

    // ── HumanResponse ──

    #[test]
    fn test_human_response_should_continue() {
        assert!(HumanResponse::ProvidedInfo { info: "x".into() }.should_continue());
        assert!(
            HumanResponse::ManuallyFixed {
                description: "d".into()
            }
            .should_continue()
        );
        assert!(
            HumanResponse::AcceptedReducedScope {
                skipped_task: "t".into()
            }
            .should_continue()
        );
        assert!(HumanResponse::AcceptedPartial.should_continue());
        assert!(HumanResponse::RetryRequested.should_continue());
        assert!(!HumanResponse::Cancelled.should_continue());
    }

    #[test]
    fn test_human_response_should_retry() {
        assert!(HumanResponse::ProvidedInfo { info: "x".into() }.should_retry());
        assert!(
            HumanResponse::ManuallyFixed {
                description: "d".into()
            }
            .should_retry()
        );
        assert!(HumanResponse::RetryRequested.should_retry());
        // These should NOT trigger retry
        assert!(
            !HumanResponse::AcceptedReducedScope {
                skipped_task: "t".into()
            }
            .should_retry()
        );
        assert!(!HumanResponse::AcceptedPartial.should_retry());
        assert!(!HumanResponse::Cancelled.should_retry());
    }

    // ── EscalationUrgency ──

    #[test]
    fn test_escalation_urgency_from_severity() {
        assert_eq!(
            EscalationUrgency::from_severity(Severity::Info),
            EscalationUrgency::Low
        );
        assert_eq!(
            EscalationUrgency::from_severity(Severity::Warning),
            EscalationUrgency::Medium
        );
        assert_eq!(
            EscalationUrgency::from_severity(Severity::Error),
            EscalationUrgency::High
        );
        assert_eq!(
            EscalationUrgency::from_severity(Severity::Critical),
            EscalationUrgency::Critical
        );
    }

    #[test]
    fn test_escalation_urgency_ordering() {
        assert!(EscalationUrgency::Low < EscalationUrgency::Medium);
        assert!(EscalationUrgency::Medium < EscalationUrgency::High);
        assert!(EscalationUrgency::High < EscalationUrgency::Critical);
    }

    // ── EscalationStatus Display ──

    #[test]
    fn test_escalation_status_display() {
        assert_eq!(format!("{}", EscalationStatus::Pending), "pending");
        assert_eq!(format!("{}", EscalationStatus::Acknowledged), "acknowledged");
        assert_eq!(format!("{}", EscalationStatus::InProgress), "in progress");
        assert_eq!(format!("{}", EscalationStatus::Resolved), "resolved");
        assert_eq!(format!("{}", EscalationStatus::Dismissed), "dismissed");
    }

    // ── EscalationHandler ──

    #[test]
    fn test_escalation_handler_new_without_notifier() {
        let handler = EscalationHandler::new(None);
        assert!(handler.notifier.is_none());
    }
}
