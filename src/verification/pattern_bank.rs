//! Pattern Learning with 4-stage pipeline: RETRIEVE → JUDGE → DISTILL → CONSOLIDATE
//!
//! Persists patterns via Event Store (not separate tables).
//! Memory cache is rebuilt from events on startup.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::history::FixStrategy;
use super::issue::{Issue, IssueCategory, IssueSeverity};
use crate::config::PatternBankConfig;
use crate::error::Result;
use crate::state::{DomainEvent, EventPayload, EventStore};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorSignature {
    pub category: IssueCategory,
    pub severity: IssueSeverity,
    pub keywords: Vec<String>,
    pub file_patterns: Vec<String>,
    pub confidence: f32,
}

impl ErrorSignature {
    pub fn from_issue(issue: &Issue) -> Self {
        let keywords = Self::extract_keywords(&issue.message);
        let file_patterns = issue
            .file
            .as_ref()
            .map(|f| vec![Self::extract_file_pattern(f)])
            .unwrap_or_default();

        Self {
            category: issue.category.clone(),
            severity: issue.severity,
            keywords,
            file_patterns,
            confidence: issue.confidence,
        }
    }

    /// Extract keywords from error message.
    ///
    /// LLM-first approach: No stopword filtering. The LLM will determine relevance
    /// when matching patterns. Filtering stopwords was language-specific
    /// and didn't help non-English projects.
    ///
    /// Unicode-aware: Uses char-based splitting to handle non-Latin scripts.
    fn extract_keywords(message: &str) -> Vec<String> {
        let lower = message.to_lowercase();
        let mut keywords: Vec<String> = Vec::new();

        // Unicode-aware word splitting: split on non-alphanumeric (handles CJK, Cyrillic, etc.)
        // char::is_alphanumeric() returns true for letters in any script
        for word in lower.split(|c: char| !c.is_alphanumeric() && c != '_') {
            let word = word.trim();
            // Use char count for proper Unicode handling (not byte length)
            if word.chars().count() >= 2 {
                keywords.push(word.to_string());
            }
        }

        keywords.sort();
        keywords.dedup();
        // No truncation - let similarity calculation handle large keyword sets
        keywords
    }

    /// Extract file pattern from path.
    fn extract_file_pattern(file: &str) -> String {
        // Get filename only
        let filename = file.rsplit('/').next().unwrap_or(file);

        // Has extension? Extract it for pattern matching
        if let Some(ext_pos) = filename.rfind('.') {
            // Skip hidden files (like .gitignore) - treat as exact filename
            if ext_pos == 0 {
                return filename.to_string();
            }
            let ext = &filename[ext_pos..];
            return format!("*{}", ext);
        }

        // Extensionless file - return exact filename for pattern matching
        // LLM will learn from actual occurrences rather than English-specific heuristics
        filename.to_string()
    }

    pub fn jaccard_similarity(&self, other: &ErrorSignature) -> f32 {
        if self.category != other.category {
            return 0.0;
        }

        let self_set: std::collections::HashSet<_> = self.keywords.iter().collect();
        let other_set: std::collections::HashSet<_> = other.keywords.iter().collect();

        if self_set.is_empty() && other_set.is_empty() {
            return 1.0;
        }

        let intersection = self_set.intersection(&other_set).count();
        let union = self_set.union(&other_set).count();

        if union == 0 {
            return 0.0;
        }

        intersection as f32 / union as f32
    }
}

/// Categorical confidence level for LLM context.
/// Avoids false precision from numeric scores with limited samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceCategory {
    /// 1-2 uses: very limited data, LLM judgment should supersede
    Experimental,
    /// 3-5 uses with >50% success: early pattern forming
    Developing,
    /// 6+ uses with >60% success: pattern is established
    Established,
    /// High failure rate regardless of sample size
    Unreliable,
}

impl ConfidenceCategory {
    pub fn format_for_llm(&self, uses: u32, success_rate: f32) -> String {
        match self {
            Self::Experimental => format!(
                "EXPERIMENTAL ({} uses, {:.0}% success) - limited data, your judgment supersedes",
                uses,
                success_rate * 100.0
            ),
            Self::Developing => format!(
                "DEVELOPING ({} uses, {:.0}% success) - early pattern, verify applicability",
                uses,
                success_rate * 100.0
            ),
            Self::Established => format!(
                "ESTABLISHED ({} uses, {:.0}% success) - reliable pattern",
                uses,
                success_rate * 100.0
            ),
            Self::Unreliable => format!(
                "UNRELIABLE ({} uses, {:.0}% success) - consider alternative approaches",
                uses,
                success_rate * 100.0
            ),
        }
    }
}

/// A learned fix pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixPattern {
    pub id: String,
    pub signature: ErrorSignature,
    pub strategy: FixStrategy,
    pub confidence: f32,
    pub success_count: u32,
    pub failure_count: u32,
    pub last_used: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// Parameters for pattern confidence calculation.
/// Extracted from PatternBankConfig for use in FixPattern methods.
#[derive(Debug, Clone, Copy)]
pub struct ConfidenceParams {
    pub initial_success: f32,
    pub initial_failure: f32,
    pub success_rate_weight: f32,
    pub decay_days: u32,
    pub decay_rate: f32,
}

impl Default for ConfidenceParams {
    fn default() -> Self {
        Self {
            initial_success: 0.7,
            initial_failure: 0.3,
            success_rate_weight: 0.7,
            decay_days: 7,
            decay_rate: 0.02,
        }
    }
}

impl From<&PatternBankConfig> for ConfidenceParams {
    fn from(config: &PatternBankConfig) -> Self {
        Self {
            initial_success: config.initial_success_confidence,
            initial_failure: config.initial_failure_confidence,
            success_rate_weight: config.confidence_success_rate_weight,
            decay_days: config.recency_decay_days,
            decay_rate: config.recency_decay_rate,
        }
    }
}

impl FixPattern {
    pub fn new(
        id: String,
        signature: ErrorSignature,
        strategy: FixStrategy,
        success: bool,
        params: ConfidenceParams,
    ) -> Self {
        let (success_count, failure_count) = if success { (1, 0) } else { (0, 1) };
        Self {
            id,
            signature,
            strategy,
            confidence: if success {
                params.initial_success
            } else {
                params.initial_failure
            },
            success_count,
            failure_count,
            last_used: Utc::now(),
            created_at: Utc::now(),
        }
    }

    pub fn success_rate(&self) -> f32 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            return 0.0;
        }
        self.success_count as f32 / total as f32
    }

    pub fn total_uses(&self) -> u32 {
        self.success_count + self.failure_count
    }

    pub fn days_since_last_use(&self) -> i64 {
        (Utc::now() - self.last_used).num_days()
    }

    pub fn confidence_category(&self) -> ConfidenceCategory {
        let uses = self.total_uses();
        let rate = self.success_rate();

        if rate < 0.4 {
            return ConfidenceCategory::Unreliable;
        }

        match uses {
            0..=2 => ConfidenceCategory::Experimental,
            3..=5 if rate >= 0.5 => ConfidenceCategory::Developing,
            3..=5 => ConfidenceCategory::Experimental,
            _ if rate >= 0.6 => ConfidenceCategory::Established,
            _ => ConfidenceCategory::Developing,
        }
    }

    pub fn format_confidence_for_llm(&self) -> String {
        self.confidence_category()
            .format_for_llm(self.total_uses(), self.success_rate())
    }

    fn update(&mut self, success: bool, params: ConfidenceParams) {
        if success {
            self.success_count += 1;
        } else {
            self.failure_count += 1;
        }
        self.last_used = Utc::now();
        self.recalculate_confidence(params);
    }

    fn recalculate_confidence(&mut self, params: ConfidenceParams) {
        let base = self.success_rate();
        let sample_factor = (self.total_uses() as f32 / 10.0).min(1.0);

        // Base confidence from success rate and sample size
        let sample_weight = 1.0 - params.success_rate_weight;
        let raw_confidence = base * params.success_rate_weight + sample_factor * sample_weight;

        // Apply recency decay: patterns unused for extended periods lose confidence
        let days_unused = self.days_since_last_use();
        let days_over_threshold = (days_unused - params.decay_days as i64).max(0) as f32;
        let decay_factor = (1.0 - params.decay_rate * days_over_threshold).max(0.1);

        self.confidence = raw_confidence * decay_factor;
    }
}

/// A single fix attempt in a trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixAttemptRecord {
    pub strategy: FixStrategy,
    pub success: bool,
    pub side_effects: Vec<String>,
}

/// Outcome of a fix trajectory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrajectoryOutcome {
    Resolved,
    PartiallyResolved,
    NotResolved,
}

/// Complete trajectory of fix attempts for an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixTrajectory {
    pub issue_id: String,
    pub signature: ErrorSignature,
    pub attempts: Vec<FixAttemptRecord>,
    pub outcome: TrajectoryOutcome,
    pub completed_at: DateTime<Utc>,
}

impl FixTrajectory {
    pub fn new(issue: &Issue) -> Self {
        Self {
            issue_id: issue.id.clone(),
            signature: ErrorSignature::from_issue(issue),
            attempts: Vec::new(),
            outcome: TrajectoryOutcome::NotResolved,
            completed_at: Utc::now(),
        }
    }

    pub fn record_attempt(
        &mut self,
        strategy: FixStrategy,
        success: bool,
        side_effects: Vec<String>,
    ) {
        self.attempts.push(FixAttemptRecord {
            strategy,
            success,
            side_effects,
        });
        if success {
            self.outcome = TrajectoryOutcome::Resolved;
        }
    }

    pub fn finalize(&mut self, resolved: bool) {
        self.outcome = if resolved {
            TrajectoryOutcome::Resolved
        } else if self.attempts.iter().any(|a| a.success) {
            TrajectoryOutcome::PartiallyResolved
        } else {
            TrajectoryOutcome::NotResolved
        };
        self.completed_at = Utc::now();
    }

    pub fn successful_strategy(&self) -> Option<FixStrategy> {
        self.attempts
            .iter()
            .rev()
            .find(|a| a.success)
            .map(|a| a.strategy)
    }
}

/// Pattern match result from RETRIEVE.
#[derive(Debug, Clone)]
pub struct PatternMatch {
    pub pattern: FixPattern,
    pub similarity: f32,
    pub score: f32,
}

impl PatternMatch {
    pub fn format_for_llm(&self) -> String {
        format!(
            "- Strategy: {:?}\n  Confidence: {}\n  Similarity: {:.0}%",
            self.pattern.strategy,
            self.pattern.format_confidence_for_llm(),
            self.similarity * 100.0
        )
    }
}

/// Memory cache for patterns.
#[derive(Debug, Default)]
struct PatternCache {
    patterns: HashMap<String, FixPattern>,
    by_category: HashMap<IssueCategory, Vec<String>>,
}

impl PatternCache {
    fn insert(&mut self, pattern: FixPattern) {
        let id = pattern.id.clone();
        let category = pattern.signature.category.clone();
        self.patterns.insert(id.clone(), pattern);
        self.by_category.entry(category).or_default().push(id);
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut FixPattern> {
        self.patterns.get_mut(id)
    }

    fn remove(&mut self, id: &str) -> Option<FixPattern> {
        if let Some(pattern) = self.patterns.remove(id) {
            let cat = pattern.signature.category.clone();
            if let Some(ids) = self.by_category.get_mut(&cat) {
                ids.retain(|i| i != id);
            }
            return Some(pattern);
        }
        None
    }

    fn patterns_for_category(&self, category: &IssueCategory) -> Vec<&FixPattern> {
        self.by_category
            .get(category)
            .map(|ids| ids.iter().filter_map(|id| self.patterns.get(id)).collect())
            .unwrap_or_default()
    }

    fn all_patterns(&self) -> impl Iterator<Item = &FixPattern> {
        self.patterns.values()
    }

    fn len(&self) -> usize {
        self.patterns.len()
    }
}

/// Pattern Bank with Event Store integration.
pub struct PatternBank {
    config: PatternBankConfig,
    event_store: Option<Arc<EventStore>>,
    cache: Arc<RwLock<PatternCache>>,
    mission_id: String,
}

impl PatternBank {
    pub fn new(config: PatternBankConfig, mission_id: impl Into<String>) -> Self {
        Self {
            config,
            event_store: None,
            cache: Arc::new(RwLock::new(PatternCache::default())),
            mission_id: mission_id.into(),
        }
    }

    pub fn with_event_store(mut self, event_store: Arc<EventStore>) -> Self {
        self.event_store = Some(event_store);
        self
    }

    /// Get confidence calculation parameters from config.
    fn confidence_params(&self) -> ConfidenceParams {
        ConfidenceParams::from(&self.config)
    }

    /// Initialize cache from Event Store.
    pub async fn initialize(&self) -> Result<()> {
        let Some(ref store) = self.event_store else {
            return Ok(());
        };

        let events = store.query(&self.mission_id, 0).await?;
        let mut cache = self.cache.write();

        let params = self.confidence_params();
        for event in events {
            match &event.payload {
                EventPayload::PatternLearned {
                    pattern_id,
                    category,
                    strategy,
                    success,
                } => {
                    let initial_confidence = if *success {
                        params.initial_success
                    } else {
                        params.initial_failure
                    };
                    let signature = ErrorSignature {
                        category: category.clone(),
                        severity: IssueSeverity::Error,
                        keywords: vec![],
                        file_patterns: vec![],
                        confidence: initial_confidence,
                    };
                    let pattern =
                        FixPattern::new(pattern_id.clone(), signature, *strategy, *success, params);
                    cache.insert(pattern);
                }
                EventPayload::PatternApplied { pattern_id, .. } => {
                    if let Some(pattern) = cache.get_mut(pattern_id) {
                        pattern.last_used = event.timestamp;
                    }
                }
                EventPayload::PatternEvolved {
                    pattern_id,
                    new_confidence,
                    ..
                } => {
                    if let Some(pattern) = cache.get_mut(pattern_id) {
                        pattern.confidence = *new_confidence;
                    }
                }
                _ => {}
            }
        }

        info!(
            patterns = cache.len(),
            mission_id = %self.mission_id,
            "PatternBank initialized from events"
        );

        Ok(())
    }

    // ========== 1. RETRIEVE ==========

    /// Retrieve patterns matching the given signature.
    /// Uses Jaccard similarity + success_rate + recency for scoring.
    pub fn retrieve(&self, signature: &ErrorSignature) -> Vec<PatternMatch> {
        if !self.config.enabled {
            return vec![];
        }

        let cache = self.cache.read();
        let candidates = cache.patterns_for_category(&signature.category);

        let mut matches: Vec<PatternMatch> = candidates
            .into_iter()
            .filter_map(|pattern| {
                let similarity = signature.jaccard_similarity(&pattern.signature);
                if similarity < self.config.min_similarity_threshold {
                    return None;
                }

                let recency_factor = 1.0
                    / (1.0
                        + pattern.days_since_last_use() as f32
                            / self.config.recency_decay_days as f32);
                let score = similarity * self.config.similarity_weight
                    + pattern.success_rate() * self.config.success_rate_weight
                    + recency_factor * self.config.recency_weight
                    + pattern.confidence * self.config.confidence_weight;

                Some(PatternMatch {
                    pattern: pattern.clone(),
                    similarity,
                    score,
                })
            })
            .collect();

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // MMR diversity: avoid returning patterns with identical strategies
        let mut result = Vec::new();
        let mut seen_strategies = std::collections::HashSet::new();

        for m in matches {
            if result.len() >= self.config.max_retrieve_results {
                break;
            }
            if seen_strategies.len() < 3 || !seen_strategies.contains(&m.pattern.strategy) {
                seen_strategies.insert(m.pattern.strategy);
                result.push(m);
            }
        }

        debug!(
            category = ?signature.category,
            count = result.len(),
            "RETRIEVE completed"
        );

        result
    }

    /// Get recommended strategy for an issue based on learned patterns.
    pub fn recommend_strategy(&self, issue: &Issue) -> Option<(FixStrategy, f32)> {
        let signature = ErrorSignature::from_issue(issue);
        let matches = self.retrieve(&signature);

        matches
            .into_iter()
            .filter(|m| m.pattern.confidence >= self.config.min_confidence)
            .max_by(|a, b| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|m| (m.pattern.strategy, m.pattern.confidence))
    }

    // ========== 2. JUDGE ==========

    /// Evaluate trajectory quality.
    ///
    /// LLM-first approach: Penalty weights are configurable to adapt to project needs.
    /// Some projects may tolerate more attempts, others prioritize minimal side effects.
    fn judge(&self, trajectory: &FixTrajectory) -> f32 {
        let outcome_score = match trajectory.outcome {
            TrajectoryOutcome::Resolved => 1.0,
            TrajectoryOutcome::PartiallyResolved => 0.5,
            TrajectoryOutcome::NotResolved => 0.0,
        };

        let attempt_penalty =
            (trajectory.attempts.len() as f32 - 1.0) * self.config.trajectory_attempt_penalty;
        let side_effect_penalty = trajectory
            .attempts
            .iter()
            .map(|a| a.side_effects.len() as f32 * self.config.trajectory_side_effect_penalty)
            .sum::<f32>();

        (outcome_score - attempt_penalty - side_effect_penalty).clamp(0.0, 1.0)
    }

    // ========== 3. DISTILL ==========

    /// Extract pattern from high-quality trajectory.
    pub async fn distill(&self, trajectory: &FixTrajectory) -> Result<Option<String>> {
        let quality = self.judge(trajectory);

        if quality < self.config.min_confidence {
            debug!(
                quality,
                threshold = self.config.min_confidence,
                "Trajectory quality below threshold, skipping distill"
            );
            return Ok(None);
        }

        let Some(strategy) = trajectory.successful_strategy() else {
            return Ok(None);
        };

        let pattern_id = format!("pat-{}", &uuid::Uuid::new_v4().to_string()[..8]);
        let pattern = FixPattern::new(
            pattern_id.clone(),
            trajectory.signature.clone(),
            strategy,
            true,
            self.confidence_params(),
        );

        // Store in cache
        {
            let mut cache = self.cache.write();
            cache.insert(pattern.clone());
        }

        // Emit PatternLearned event
        if let Some(ref store) = self.event_store {
            let event = DomainEvent::pattern_learned(
                &self.mission_id,
                &pattern_id,
                trajectory.signature.category.clone(),
                strategy,
                true,
            );
            store.append(event).await?;
        }

        info!(
            pattern_id = %pattern_id,
            strategy = ?strategy,
            category = ?trajectory.signature.category,
            quality,
            "Pattern distilled"
        );

        Ok(Some(pattern_id))
    }

    /// Record pattern application outcome.
    pub async fn record_application(
        &self,
        pattern_id: &str,
        issue_id: &str,
        success: bool,
    ) -> Result<()> {
        let params = self.confidence_params();
        let old_confidence = {
            let mut cache = self.cache.write();
            if let Some(pattern) = cache.get_mut(pattern_id) {
                let old = pattern.confidence;
                pattern.update(success, params);
                Some((old, pattern.confidence))
            } else {
                None
            }
        };

        if let Some(ref store) = self.event_store {
            let event = DomainEvent::new(
                &self.mission_id,
                EventPayload::PatternApplied {
                    pattern_id: pattern_id.to_string(),
                    issue_id: issue_id.to_string(),
                    confidence: old_confidence.map(|(_, new)| new).unwrap_or(0.0),
                },
            );
            store.append(event).await?;

            if let Some((old, new)) = old_confidence
                && (old - new).abs() > 0.05
            {
                let evolve_event = DomainEvent::new(
                    &self.mission_id,
                    EventPayload::PatternEvolved {
                        pattern_id: pattern_id.to_string(),
                        old_confidence: old,
                        new_confidence: new,
                    },
                );
                store.append(evolve_event).await?;
            }
        }

        Ok(())
    }

    // ========== 4. CONSOLIDATE ==========

    /// Cleanup: remove old low-performing patterns, merge similar ones.
    pub async fn consolidate(&self) -> Result<ConsolidationResult> {
        if !self.config.enabled {
            return Ok(ConsolidationResult::default());
        }

        let mut removed = 0;
        let mut merged = 0;

        let patterns_to_remove: Vec<String> = {
            let cache = self.cache.read();
            cache
                .all_patterns()
                .filter(|p| {
                    let inactive =
                        p.days_since_last_use() > self.config.inactive_days_threshold as i64;
                    let low_performing =
                        p.success_rate() < self.config.min_success_rate && p.total_uses() >= 3;
                    inactive && low_performing
                })
                .map(|p| p.id.clone())
                .collect()
        };

        {
            let mut cache = self.cache.write();
            for id in &patterns_to_remove {
                if cache.remove(id).is_some() {
                    removed += 1;
                }
            }
        }

        // Merge highly similar patterns (similarity > threshold)
        let merge_candidates: Vec<(String, String)> = {
            let cache = self.cache.read();
            let mut candidates = Vec::new();
            let patterns: Vec<_> = cache.all_patterns().collect();

            for i in 0..patterns.len() {
                for j in (i + 1)..patterns.len() {
                    let a = patterns[i];
                    let b = patterns[j];
                    if a.strategy == b.strategy {
                        let sim = a.signature.jaccard_similarity(&b.signature);
                        if sim > self.config.merge_similarity_threshold {
                            let (keep, remove) = if a.total_uses() >= b.total_uses() {
                                (a.id.clone(), b.id.clone())
                            } else {
                                (b.id.clone(), a.id.clone())
                            };
                            candidates.push((keep, remove));
                        }
                    }
                }
            }
            candidates
        };

        {
            let params = self.confidence_params();
            let mut cache = self.cache.write();
            for (keep_id, remove_id) in merge_candidates {
                if let Some(removed_pattern) = cache.remove(&remove_id)
                    && let Some(keep_pattern) = cache.get_mut(&keep_id)
                {
                    keep_pattern.success_count += removed_pattern.success_count;
                    keep_pattern.failure_count += removed_pattern.failure_count;
                    keep_pattern.recalculate_confidence(params);
                    merged += 1;
                }
            }
        }

        // Enforce max_patterns limit
        let patterns_over_limit: Vec<String> = {
            let cache = self.cache.read();
            if cache.len() <= self.config.max_patterns {
                vec![]
            } else {
                let mut patterns: Vec<_> = cache.all_patterns().collect();
                patterns.sort_by(|a, b| {
                    a.confidence
                        .partial_cmp(&b.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                patterns
                    .iter()
                    .take(cache.len() - self.config.max_patterns)
                    .map(|p| p.id.clone())
                    .collect()
            }
        };

        {
            let mut cache = self.cache.write();
            for id in &patterns_over_limit {
                if cache.remove(id).is_some() {
                    removed += 1;
                }
            }
        }

        let remaining = self.cache.read().len();

        info!(removed, merged, remaining, "CONSOLIDATE completed");

        Ok(ConsolidationResult {
            removed,
            merged,
            remaining,
        })
    }

    /// Get statistics about the pattern bank.
    pub fn statistics(&self) -> PatternBankStatistics {
        let cache = self.cache.read();
        let total = cache.len();

        let mut by_category: HashMap<IssueCategory, usize> = HashMap::new();
        let mut by_strategy: HashMap<FixStrategy, usize> = HashMap::new();
        let mut total_success = 0u32;
        let mut total_failure = 0u32;

        for pattern in cache.all_patterns() {
            *by_category
                .entry(pattern.signature.category.clone())
                .or_default() += 1;
            *by_strategy.entry(pattern.strategy).or_default() += 1;
            total_success += pattern.success_count;
            total_failure += pattern.failure_count;
        }

        let avg_confidence = if total > 0 {
            cache.all_patterns().map(|p| p.confidence).sum::<f32>() / total as f32
        } else {
            0.0
        };

        PatternBankStatistics {
            total_patterns: total,
            by_category,
            by_strategy,
            total_success,
            total_failure,
            avg_confidence,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConsolidationResult {
    pub removed: usize,
    pub merged: usize,
    pub remaining: usize,
}

#[derive(Debug, Clone)]
pub struct PatternBankStatistics {
    pub total_patterns: usize,
    pub by_category: HashMap<IssueCategory, usize>,
    pub by_strategy: HashMap<FixStrategy, usize>,
    pub total_success: u32,
    pub total_failure: u32,
    pub avg_confidence: f32,
}

impl PatternBankStatistics {
    pub fn overall_success_rate(&self) -> f32 {
        let total = self.total_success + self.total_failure;
        if total == 0 {
            return 0.0;
        }
        self.total_success as f32 / total as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CONFIDENCE: f32 = 0.8;

    fn test_config() -> PatternBankConfig {
        PatternBankConfig {
            enabled: true,
            min_success_rate: 0.3,
            min_confidence: 0.5,
            max_patterns: 100,
            min_similarity_threshold: 0.2,
            ..Default::default()
        }
    }

    #[test]
    fn test_error_signature_extraction() {
        // Test with Rust-like error message
        let issue = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "error[E0308]: mismatched types - expected i32, found String",
            TEST_CONFIDENCE,
        )
        .with_location("src/main.rs", Some(10));

        let sig = ErrorSignature::from_issue(&issue);

        assert_eq!(sig.category, IssueCategory::BuildError);
        // Extract all meaningful words (not just predefined list)
        assert!(sig.keywords.contains(&"error".to_string()));
        assert!(sig.keywords.contains(&"mismatched".to_string()));
        assert!(sig.keywords.contains(&"types".to_string()));
        // Error codes ARE extracted - they're meaningful identifiers
        assert!(sig.keywords.iter().any(|k| k.contains("e0308")));

        // Test with TypeScript-like error message
        let ts_issue = Issue::new(
            IssueCategory::TypeCheckError,
            IssueSeverity::Error,
            "error TS2322: Type string is not assignable to type number",
            TEST_CONFIDENCE,
        )
        .with_location("src/index.ts", Some(5));

        let ts_sig = ErrorSignature::from_issue(&ts_issue);

        assert_eq!(ts_sig.category, IssueCategory::TypeCheckError);
        assert!(ts_sig.keywords.contains(&"error".to_string()));
        assert!(ts_sig.keywords.contains(&"type".to_string()));
        assert!(ts_sig.keywords.contains(&"assignable".to_string()));
        // TS2322 IS extracted - error codes are meaningful
        assert!(ts_sig.keywords.iter().any(|k| k.contains("ts2322")));
    }

    #[test]
    fn test_jaccard_similarity() {
        let sig1 = ErrorSignature {
            category: IssueCategory::BuildError,
            severity: IssueSeverity::Error,
            keywords: vec!["error".into(), "type".into(), "mismatch".into()],
            file_patterns: vec!["*.rs".into()],
            confidence: 0.8,
        };

        let sig2 = ErrorSignature {
            category: IssueCategory::BuildError,
            severity: IssueSeverity::Error,
            keywords: vec!["error".into(), "type".into(), "expected".into()],
            file_patterns: vec!["*.rs".into()],
            confidence: 0.8,
        };

        let sig3 = ErrorSignature {
            category: IssueCategory::TestFailure,
            severity: IssueSeverity::Error,
            keywords: vec!["error".into(), "type".into()],
            file_patterns: vec![],
            confidence: 0.8,
        };

        let sim12 = sig1.jaccard_similarity(&sig2);
        let sim13 = sig1.jaccard_similarity(&sig3);

        assert!(
            sim12 > 0.3,
            "Similar signatures should have high similarity"
        );
        assert_eq!(sim13, 0.0, "Different categories should have 0 similarity");
    }

    #[test]
    fn test_pattern_creation_and_update() {
        let sig = ErrorSignature {
            category: IssueCategory::BuildError,
            severity: IssueSeverity::Error,
            keywords: vec!["error".into()],
            file_patterns: vec![],
            confidence: 0.8,
        };

        let params = ConfidenceParams::default();
        let mut pattern = FixPattern::new("p1".into(), sig, FixStrategy::DirectFix, true, params);

        assert_eq!(pattern.success_count, 1);
        assert_eq!(pattern.failure_count, 0);
        assert_eq!(pattern.success_rate(), 1.0);

        pattern.update(false, params);
        assert_eq!(pattern.success_count, 1);
        assert_eq!(pattern.failure_count, 1);
        assert_eq!(pattern.success_rate(), 0.5);
    }

    #[test]
    fn test_trajectory_recording() {
        let issue = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "test error",
            TEST_CONFIDENCE,
        );

        let mut trajectory = FixTrajectory::new(&issue);
        assert_eq!(trajectory.outcome, TrajectoryOutcome::NotResolved);

        trajectory.record_attempt(FixStrategy::DirectFix, false, vec![]);
        assert_eq!(trajectory.outcome, TrajectoryOutcome::NotResolved);

        trajectory.record_attempt(FixStrategy::ContextualFix, true, vec![]);
        assert_eq!(trajectory.outcome, TrajectoryOutcome::Resolved);

        assert_eq!(
            trajectory.successful_strategy(),
            Some(FixStrategy::ContextualFix)
        );
    }

    #[test]
    fn test_pattern_bank_retrieve() {
        let config = test_config();
        let bank = PatternBank::new(config, "test-mission");

        let sig = ErrorSignature {
            category: IssueCategory::BuildError,
            severity: IssueSeverity::Error,
            keywords: vec!["error".into(), "type".into()],
            file_patterns: vec![],
            confidence: 0.8,
        };

        // Add patterns directly to cache
        {
            let params = bank.confidence_params();
            let mut cache = bank.cache.write();
            cache.insert(FixPattern::new(
                "p1".into(),
                sig.clone(),
                FixStrategy::DirectFix,
                true,
                params,
            ));
            cache.insert(FixPattern::new(
                "p2".into(),
                sig.clone(),
                FixStrategy::ContextualFix,
                true,
                params,
            ));
        }

        let matches = bank.retrieve(&sig);
        assert!(!matches.is_empty());
        assert!(matches.len() <= 5);
    }

    #[test]
    fn test_judge_trajectory() {
        let config = test_config();
        let bank = PatternBank::new(config, "test-mission");

        let issue = Issue::new(
            IssueCategory::BuildError,
            IssueSeverity::Error,
            "test",
            TEST_CONFIDENCE,
        );

        // Perfect trajectory
        let mut perfect = FixTrajectory::new(&issue);
        perfect.record_attempt(FixStrategy::DirectFix, true, vec![]);
        perfect.finalize(true);
        assert!(bank.judge(&perfect) > 0.9);

        // Failed trajectory
        let mut failed = FixTrajectory::new(&issue);
        failed.record_attempt(FixStrategy::DirectFix, false, vec![]);
        failed.record_attempt(FixStrategy::ContextualFix, false, vec![]);
        failed.finalize(false);
        assert!(bank.judge(&failed) < 0.2);

        // Trajectory with side effects
        let mut side_effects = FixTrajectory::new(&issue);
        side_effects.record_attempt(
            FixStrategy::DirectFix,
            true,
            vec!["broke other test".into()],
        );
        side_effects.finalize(true);
        assert!(bank.judge(&side_effects) < bank.judge(&perfect));
    }
}
