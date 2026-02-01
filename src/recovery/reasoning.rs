//! Hypothesis and decision tracking for durable execution.
//!
//! This module captures the reasoning chain during mission execution,
//! enabling intelligent recovery after long interruptions (week+ missions).
//!
//! Key components:
//! - `Hypothesis`: Tracks approaches being considered and their validation status
//! - `DecisionRecord`: Captures why specific approaches were chosen over alternatives
//! - `ReasoningContext`: Aggregates hypotheses and decisions for checkpoint persistence
//! - `ReasoningHistory`: JSONL persistence for cross-mission learning

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::error::{PilotError, Result};

/// A hypothesis about how to approach a problem.
/// Tracks the reasoning chain for week+ recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub task_id: Option<String>,
    pub description: String,
    pub rationale: String,
    pub status: HypothesisStatus,
    pub confidence: f32,
    pub evidence_refs: Vec<EvidenceRef>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub resolution_reason: Option<String>,
}

impl Hypothesis {
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            task_id: None,
            description: description.into(),
            rationale: rationale.into(),
            status: HypothesisStatus::Proposed,
            confidence: 0.5,
            evidence_refs: Vec::new(),
            created_at: Utc::now(),
            resolved_at: None,
            resolution_reason: None,
        }
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub fn with_evidence(mut self, refs: Vec<EvidenceRef>) -> Self {
        self.evidence_refs = refs;
        self
    }

    pub fn activate(&mut self) {
        self.status = HypothesisStatus::Active;
    }

    pub fn validate(&mut self, reason: impl Into<String>) {
        self.status = HypothesisStatus::Validated;
        self.resolved_at = Some(Utc::now());
        self.resolution_reason = Some(reason.into());
    }

    pub fn reject(&mut self, reason: impl Into<String>) {
        self.status = HypothesisStatus::Rejected;
        self.resolved_at = Some(Utc::now());
        self.resolution_reason = Some(reason.into());
    }

    pub fn supersede(&mut self, reason: impl Into<String>) {
        self.status = HypothesisStatus::Superseded;
        self.resolved_at = Some(Utc::now());
        self.resolution_reason = Some(reason.into());
    }

    pub fn is_active(&self) -> bool {
        self.status == HypothesisStatus::Active
    }

    pub fn is_resolved(&self) -> bool {
        matches!(
            self.status,
            HypothesisStatus::Validated | HypothesisStatus::Rejected | HypothesisStatus::Superseded
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// Initial proposal, not yet acted upon
    Proposed,
    /// Currently being executed
    Active,
    /// Confirmed to work
    Validated,
    /// Proven incorrect or failed
    Rejected,
    /// Replaced by a better hypothesis
    Superseded,
}

/// Reference to evidence that supports a hypothesis or decision.
/// Enables traceability back to source facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub evidence_type: EvidenceRefType,
    pub identifier: String,
    pub description: String,
    pub confidence: f32,
}

impl EvidenceRef {
    pub fn new(
        evidence_type: EvidenceRefType,
        identifier: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            evidence_type,
            identifier: identifier.into(),
            description: description.into(),
            confidence: 0.8,
        }
    }

    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }

    pub fn codebase_file(path: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(EvidenceRefType::CodebaseFile, path, description)
    }

    pub fn pattern_match(pattern: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(EvidenceRefType::PatternMatch, pattern, description)
    }

    pub fn prior_outcome(outcome_id: impl Into<String>, description: impl Into<String>) -> Self {
        Self::new(EvidenceRefType::PriorOutcome, outcome_id, description)
    }

    pub fn verification_result(round: u32, description: impl Into<String>) -> Self {
        Self::new(
            EvidenceRefType::VerificationResult,
            format!("round_{}", round),
            description,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRefType {
    /// Reference to a file in Evidence.codebase_analysis.relevant_files
    CodebaseFile,
    /// Reference to Evidence.codebase_analysis.existing_patterns
    PatternMatch,
    /// Reference to RetryHistory outcome
    PriorOutcome,
    /// User-provided context or clarification
    UserInput,
    /// Reference to VerificationRound result
    VerificationResult,
    /// Reference to Evidence.prior_knowledge
    PriorKnowledge,
}

/// A decision made during mission execution with full rationale.
/// Critical for understanding "why" when resuming after long breaks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRecord {
    pub id: String,
    pub task_id: Option<String>,
    pub category: DecisionCategory,
    pub context_summary: String,
    pub chosen: ChosenOption,
    pub alternatives: Vec<RejectedAlternative>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub impact: DecisionImpact,
    pub hypothesis_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl DecisionRecord {
    pub fn new(
        id: impl Into<String>,
        category: DecisionCategory,
        context_summary: impl Into<String>,
        chosen: ChosenOption,
    ) -> Self {
        Self {
            id: id.into(),
            task_id: None,
            category,
            context_summary: context_summary.into(),
            chosen,
            alternatives: Vec::new(),
            evidence_refs: Vec::new(),
            impact: DecisionImpact::Medium,
            hypothesis_id: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn with_alternatives(mut self, alternatives: Vec<RejectedAlternative>) -> Self {
        self.alternatives = alternatives;
        self
    }

    pub fn with_evidence(mut self, refs: Vec<EvidenceRef>) -> Self {
        self.evidence_refs = refs;
        self
    }

    pub fn with_impact(mut self, impact: DecisionImpact) -> Self {
        self.impact = impact;
        self
    }

    pub fn with_hypothesis(mut self, hypothesis_id: impl Into<String>) -> Self {
        self.hypothesis_id = Some(hypothesis_id.into());
        self
    }

    /// Create a retry strategy decision record.
    pub fn retry_strategy(
        task_id: &str,
        attempt: u32,
        chosen_strategy: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self::new(
            format!("{}_retry_{}", task_id, attempt),
            DecisionCategory::RetryStrategy,
            format!("Task {} after {} attempts", task_id, attempt),
            ChosenOption {
                description: chosen_strategy.into(),
                rationale: reasoning.into(),
            },
        )
        .with_task(task_id)
    }

    /// Create a fix strategy decision record.
    pub fn fix_strategy(
        task_id: &str,
        issue_signature: &str,
        strategy: impl Into<String>,
        reasoning: impl Into<String>,
    ) -> Self {
        Self::new(
            format!("{}_{}", task_id, issue_signature),
            DecisionCategory::FixStrategy,
            format!("Fix strategy for issue {}", issue_signature),
            ChosenOption {
                description: strategy.into(),
                rationale: reasoning.into(),
            },
        )
        .with_task(task_id)
    }

    /// Create an escalation decision record.
    pub fn escalation(
        task_id: &str,
        reason: impl Into<String>,
        diagnosis: impl Into<String>,
    ) -> Self {
        Self::new(
            format!("{}_escalation", task_id),
            DecisionCategory::Escalation,
            "Escalating to human intervention",
            ChosenOption {
                description: "Escalate to human".into(),
                rationale: format!("{}: {}", reason.into(), diagnosis.into()),
            },
        )
        .with_task(task_id)
        .with_impact(DecisionImpact::High)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionCategory {
    /// Choosing between implementation approaches
    ApproachSelection,
    /// Retry vs escalate decision
    RetryStrategy,
    /// Which fix strategy to apply
    FixStrategy,
    /// Planning phase decisions
    PlanningChoice,
    /// Recovery from failure decisions
    RecoveryAction,
    /// Decision to escalate to human
    Escalation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChosenOption {
    pub description: String,
    pub rationale: String,
}

impl ChosenOption {
    pub fn new(description: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            rationale: rationale.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedAlternative {
    pub description: String,
    pub rejection_reason: String,
}

impl RejectedAlternative {
    pub fn new(description: impl Into<String>, rejection_reason: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            rejection_reason: rejection_reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionImpact {
    /// Minor decision, easily reversible
    Low,
    /// Moderate impact, affects task outcome
    Medium,
    /// Significant impact, affects mission direction
    High,
    /// Critical decision, may require human review
    Critical,
}

/// Complete reasoning context for checkpoint persistence.
/// This is serialized as YAML and stored in Checkpoint.reasoning_snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReasoningContext {
    pub mission_id: String,
    pub hypotheses: Vec<Hypothesis>,
    pub active_hypothesis_id: Option<String>,
    pub decisions: Vec<DecisionRecord>,
    pub created_at: DateTime<Utc>,
    pub last_updated_at: DateTime<Utc>,
}

impl ReasoningContext {
    pub fn new(mission_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            mission_id: mission_id.into(),
            hypotheses: Vec::new(),
            active_hypothesis_id: None,
            decisions: Vec::new(),
            created_at: now,
            last_updated_at: now,
        }
    }

    pub fn add_hypothesis(&mut self, hypothesis: Hypothesis) {
        if hypothesis.status == HypothesisStatus::Active {
            // Deactivate any currently active hypothesis
            if let Some(ref id) = self.active_hypothesis_id
                && let Some(h) = self.hypotheses.iter_mut().find(|h| &h.id == id)
            {
                h.supersede("Replaced by new active hypothesis");
            }
            self.active_hypothesis_id = Some(hypothesis.id.clone());
        }
        self.hypotheses.push(hypothesis);
        self.last_updated_at = Utc::now();
    }

    pub fn add_decision(&mut self, decision: DecisionRecord) {
        self.decisions.push(decision);
        self.last_updated_at = Utc::now();
    }

    pub fn active_hypothesis(&self) -> Option<&Hypothesis> {
        self.active_hypothesis_id
            .as_ref()
            .and_then(|id| self.hypotheses.iter().find(|h| &h.id == id))
    }

    pub fn active_hypothesis_mut(&mut self) -> Option<&mut Hypothesis> {
        let id = self.active_hypothesis_id.clone()?;
        self.hypotheses.iter_mut().find(|h| h.id == id)
    }

    pub fn validate_active_hypothesis(&mut self, reason: impl Into<String>) {
        if let Some(h) = self.active_hypothesis_mut() {
            h.validate(reason);
        }
        self.active_hypothesis_id = None;
        self.last_updated_at = Utc::now();
    }

    pub fn reject_active_hypothesis(&mut self, reason: impl Into<String>) {
        if let Some(h) = self.active_hypothesis_mut() {
            h.reject(reason);
        }
        self.active_hypothesis_id = None;
        self.last_updated_at = Utc::now();
    }

    pub fn hypotheses_for_task(&self, task_id: &str) -> Vec<&Hypothesis> {
        self.hypotheses
            .iter()
            .filter(|h| h.task_id.as_deref() == Some(task_id))
            .collect()
    }

    pub fn decisions_for_task(&self, task_id: &str) -> Vec<&DecisionRecord> {
        self.decisions
            .iter()
            .filter(|d| d.task_id.as_deref() == Some(task_id))
            .collect()
    }

    /// Get recent decisions for context window (most important for recovery).
    pub fn recent_decisions(&self, limit: usize) -> Vec<&DecisionRecord> {
        self.decisions.iter().rev().take(limit).collect()
    }

    /// Get high-impact decisions that shaped the mission direction.
    pub fn high_impact_decisions(&self) -> Vec<&DecisionRecord> {
        self.decisions
            .iter()
            .filter(|d| matches!(d.impact, DecisionImpact::High | DecisionImpact::Critical))
            .collect()
    }

    /// Compact the context by removing resolved hypotheses and old decisions.
    /// Keeps recent items and high-impact decisions.
    pub fn compact(&mut self, max_hypotheses: usize, max_decisions: usize) {
        self.compact_with_ratio(max_hypotheses, max_decisions, 0.5)
    }

    /// Compact with configurable ratio for recent vs high-impact decisions.
    ///
    /// `recent_ratio`: Fraction of max_decisions allocated to recent decisions (0.0-1.0).
    /// The rest is allocated to high-impact decisions.
    pub fn compact_with_ratio(
        &mut self,
        max_hypotheses: usize,
        max_decisions: usize,
        recent_ratio: f32,
    ) {
        // Keep active and recent hypotheses
        if self.hypotheses.len() > max_hypotheses {
            let active_id = self.active_hypothesis_id.clone();
            self.hypotheses.retain(|h| {
                Some(&h.id) == active_id.as_ref()
                    || !h.is_resolved()
                    || h.status == HypothesisStatus::Validated
            });

            // If still too many, sort by created_at and keep most recent
            // But ensure active hypothesis is always preserved
            if self.hypotheses.len() > max_hypotheses {
                // Extract active hypothesis first
                let active_hypothesis = active_id.as_ref().and_then(|id| {
                    self.hypotheses
                        .iter()
                        .position(|h| &h.id == id)
                        .map(|pos| self.hypotheses.remove(pos))
                });

                // Sort remaining by created_at descending and truncate
                self.hypotheses
                    .sort_by(|a, b| b.created_at.cmp(&a.created_at));
                self.hypotheses.truncate(
                    max_hypotheses.saturating_sub(if active_hypothesis.is_some() { 1 } else { 0 }),
                );

                // Re-insert active hypothesis at the front
                if let Some(h) = active_hypothesis {
                    self.hypotheses.insert(0, h);
                }
            }
        }

        // Keep high-impact and recent decisions with configurable ratio
        if self.decisions.len() > max_decisions {
            let recent_count = (max_decisions as f32 * recent_ratio.clamp(0.0, 1.0)) as usize;

            let mut high_impact: Vec<_> = self
                .decisions
                .iter()
                .filter(|d| matches!(d.impact, DecisionImpact::High | DecisionImpact::Critical))
                .cloned()
                .collect();

            let mut recent: Vec<_> = self
                .decisions
                .iter()
                .rev()
                .take(recent_count.max(1))
                .cloned()
                .collect();

            // Merge and deduplicate
            high_impact.append(&mut recent);
            high_impact.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            high_impact.dedup_by(|a, b| a.id == b.id);
            high_impact.truncate(max_decisions);

            self.decisions = high_impact;
        }

        self.last_updated_at = Utc::now();
    }

    /// Summary for logging/debugging.
    pub fn summary(&self) -> String {
        let active = self
            .active_hypothesis()
            .map(|h| h.description.as_str())
            .unwrap_or("none");
        let validated = self
            .hypotheses
            .iter()
            .filter(|h| h.status == HypothesisStatus::Validated)
            .count();
        let rejected = self
            .hypotheses
            .iter()
            .filter(|h| h.status == HypothesisStatus::Rejected)
            .count();

        format!(
            "hypotheses: {} ({} validated, {} rejected), decisions: {}, active: {}",
            self.hypotheses.len(),
            validated,
            rejected,
            self.decisions.len(),
            active
        )
    }
}

/// Persistent JSONL storage for reasoning history.
/// Follows the same pattern as RetryHistory for consistency.
pub struct ReasoningHistory {
    path: PathBuf,
    retention_days: i64,
}

impl ReasoningHistory {
    pub fn new(path: PathBuf, retention_days: i64) -> Self {
        Self {
            path,
            retention_days,
        }
    }

    pub fn from_pilot_dir(pilot_dir: &Path, retention_days: i64) -> Self {
        let path = pilot_dir.join("learning").join("reasoning_history.jsonl");
        Self::new(path, retention_days)
    }

    /// Check if a task_id belongs to the given mission.
    /// Task IDs follow the format "{mission_id}_{task_suffix}" (e.g., "M001_T001").
    /// This performs exact prefix match with underscore delimiter to avoid false positives
    /// (e.g., "M001" should not match "M0012_T001").
    fn matches_mission(task_id: &Option<String>, mission_id: &str) -> bool {
        task_id
            .as_ref()
            .is_some_and(|t| t == mission_id || t.starts_with(&format!("{}_", mission_id)))
    }

    async fn ensure_dir(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    pub async fn append_decision(&self, decision: &DecisionRecord) -> Result<()> {
        self.ensure_dir().await?;

        let entry = ReasoningHistoryEntry::Decision(decision.clone());
        let line = serde_json::to_string(&entry)
            .map_err(|e| PilotError::Learning(format!("JSON serialize failed: {}", e)))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        file.write_all(format!("{}\n", line).as_bytes()).await?;
        file.flush().await?;

        debug!(path = %self.path.display(), "Appended decision to reasoning history");
        Ok(())
    }

    pub async fn append_hypothesis(&self, hypothesis: &Hypothesis) -> Result<()> {
        self.ensure_dir().await?;

        let entry = ReasoningHistoryEntry::Hypothesis(hypothesis.clone());
        let line = serde_json::to_string(&entry)
            .map_err(|e| PilotError::Learning(format!("JSON serialize failed: {}", e)))?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        file.write_all(format!("{}\n", line).as_bytes()).await?;
        file.flush().await?;

        debug!(path = %self.path.display(), "Appended hypothesis to reasoning history");
        Ok(())
    }

    pub async fn load_for_mission(&self, mission_id: &str) -> Result<ReasoningContext> {
        if !self.path.exists() {
            return Ok(ReasoningContext::new(mission_id));
        }

        let content = fs::read_to_string(&self.path).await?;
        let cutoff = Utc::now() - Duration::days(self.retention_days);

        let mut context = ReasoningContext::new(mission_id);

        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<ReasoningHistoryEntry>(line) {
                Ok(ReasoningHistoryEntry::Hypothesis(h))
                    if h.created_at >= cutoff && Self::matches_mission(&h.task_id, mission_id) =>
                {
                    context.hypotheses.push(h);
                }
                Ok(ReasoningHistoryEntry::Decision(d))
                    if d.created_at >= cutoff && Self::matches_mission(&d.task_id, mission_id) =>
                {
                    context.decisions.push(d);
                }
                Ok(_) => {} // Filtered out by date or mission
                Err(e) => {
                    warn!(line = %line, error = %e, "Skipping invalid reasoning history line");
                }
            }
        }

        // Restore active hypothesis if any
        context.active_hypothesis_id = context
            .hypotheses
            .iter()
            .find(|h| h.status == HypothesisStatus::Active)
            .map(|h| h.id.clone());

        debug!(
            hypotheses = context.hypotheses.len(),
            decisions = context.decisions.len(),
            path = %self.path.display(),
            "Loaded reasoning history for mission"
        );

        Ok(context)
    }

    pub async fn compact(&self) -> Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }

        let content = fs::read_to_string(&self.path).await?;
        let all_lines: Vec<_> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        let original_count = all_lines.len();

        let cutoff = Utc::now() - Duration::days(self.retention_days);
        let valid_entries: Vec<ReasoningHistoryEntry> = all_lines
            .into_iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .filter(|e: &ReasoningHistoryEntry| match e {
                ReasoningHistoryEntry::Hypothesis(h) => h.created_at >= cutoff,
                ReasoningHistoryEntry::Decision(d) => d.created_at >= cutoff,
            })
            .collect();

        if valid_entries.len() == original_count {
            return Ok(0);
        }

        self.ensure_dir().await?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)
            .await?;

        for entry in &valid_entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| PilotError::Learning(format!("JSON serialize failed: {}", e)))?;
            file.write_all(format!("{}\n", line).as_bytes()).await?;
        }

        file.flush().await?;
        let removed = original_count - valid_entries.len();
        debug!(removed, "Compacted reasoning history");
        Ok(removed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReasoningHistoryEntry {
    Hypothesis(Hypothesis),
    Decision(DecisionRecord),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hypothesis_lifecycle() {
        let mut h = Hypothesis::new("H001", "Test hypothesis", "Based on evidence");
        assert_eq!(h.status, HypothesisStatus::Proposed);
        assert!(!h.is_active());
        assert!(!h.is_resolved());

        h.activate();
        assert!(h.is_active());
        assert!(!h.is_resolved());

        h.validate("Tests passed");
        assert!(!h.is_active());
        assert!(h.is_resolved());
        assert_eq!(h.status, HypothesisStatus::Validated);
    }

    #[test]
    fn test_reasoning_context_hypothesis_management() {
        let mut ctx = ReasoningContext::new("M001");

        let mut h1 = Hypothesis::new("H001", "First approach", "Rationale 1");
        h1.activate();
        ctx.add_hypothesis(h1);

        assert_eq!(ctx.active_hypothesis_id, Some("H001".into()));
        assert!(ctx.active_hypothesis().is_some());

        let mut h2 = Hypothesis::new("H002", "Second approach", "Rationale 2");
        h2.activate();
        ctx.add_hypothesis(h2);

        // H1 should be superseded, H2 should be active
        assert_eq!(ctx.active_hypothesis_id, Some("H002".into()));
        assert_eq!(ctx.hypotheses[0].status, HypothesisStatus::Superseded);
        assert_eq!(ctx.hypotheses[1].status, HypothesisStatus::Active);
    }

    #[test]
    fn test_decision_record_factories() {
        let d = DecisionRecord::retry_strategy("T001", 3, "DirectFix", "Error is localized");
        assert_eq!(d.category, DecisionCategory::RetryStrategy);
        assert_eq!(d.task_id, Some("T001".into()));

        let d = DecisionRecord::escalation("T002", "Max retries exceeded", "Pattern not fixable");
        assert_eq!(d.category, DecisionCategory::Escalation);
        assert_eq!(d.impact, DecisionImpact::High);
    }

    #[test]
    fn test_context_compaction() {
        let mut ctx = ReasoningContext::new("M001");

        // Add many hypotheses
        for i in 0..20 {
            let mut h = Hypothesis::new(format!("H{:03}", i), format!("Hypothesis {}", i), "Test");
            if i == 15 {
                h.activate();
            } else if i < 10 {
                h.validate("Done");
            }
            ctx.add_hypothesis(h);
        }

        // Add many decisions
        for i in 0..30 {
            let mut d = DecisionRecord::new(
                format!("D{:03}", i),
                DecisionCategory::FixStrategy,
                "Context",
                ChosenOption::new("Choice", "Reason"),
            );
            if i % 10 == 0 {
                d = d.with_impact(DecisionImpact::High);
            }
            ctx.add_decision(d);
        }

        ctx.compact(5, 10);

        assert!(ctx.hypotheses.len() <= 5);
        assert!(ctx.decisions.len() <= 10);

        // Active hypothesis should be preserved
        assert!(ctx.active_hypothesis().is_some());
    }

    #[tokio::test]
    async fn test_reasoning_history_persistence() {
        let dir = TempDir::new().unwrap();
        let history = ReasoningHistory::new(dir.path().join("reasoning.jsonl"), 90);

        let h =
            Hypothesis::new("H001", "Test hypothesis", "Based on evidence").with_task("M001_T001");
        history.append_hypothesis(&h).await.unwrap();

        let d = DecisionRecord::retry_strategy("M001_T001", 1, "DirectFix", "Localized error");
        history.append_decision(&d).await.unwrap();

        let ctx = history.load_for_mission("M001").await.unwrap();
        assert_eq!(ctx.hypotheses.len(), 1);
        assert_eq!(ctx.decisions.len(), 1);
    }
}
