//! Multi-dimensional task scoring for complexity assessment.
//!
//! Improves complexity judgment accuracy through:
//! - File count and dependency analysis
//! - Pattern bank success rate integration
//! - Evidence confidence scoring
//! - Historical task similarity matching

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::config::{TaskScoringConfig, TaskScoringWeights};
use crate::mission::Task;
use crate::verification::PatternBank;

use super::evidence::Evidence;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskScore {
    /// File complexity score. Always available when evidence exists.
    pub file_score: Option<f32>,
    /// Dependency complexity score. Always available when evidence exists.
    pub dependency_score: Option<f32>,
    /// Pattern success rate. None if no pattern bank or no patterns.
    pub pattern_score: Option<f32>,
    /// Evidence confidence. None if no evidence or no files.
    pub confidence_score: Option<f32>,
    /// Historical success rate. None if no history or no similar tasks.
    pub history_score: Option<f32>,
    /// Weighted total of available scores.
    pub total: f32,
    /// Number of dimensions with actual data (0-5).
    pub available_dimensions: u8,
}

impl TaskScore {
    pub fn calculate_total(&mut self, weights: &TaskScoringWeights) {
        let mut sum = 0.0;
        let mut weight_sum = 0.0;
        let mut dimensions = 0u8;

        if let Some(s) = self.file_score {
            sum += s * weights.file;
            weight_sum += weights.file;
            dimensions += 1;
        }
        if let Some(s) = self.dependency_score {
            sum += s * weights.dependency;
            weight_sum += weights.dependency;
            dimensions += 1;
        }
        if let Some(s) = self.pattern_score {
            sum += s * weights.pattern;
            weight_sum += weights.pattern;
            dimensions += 1;
        }
        if let Some(s) = self.confidence_score {
            sum += s * weights.confidence;
            weight_sum += weights.confidence;
            dimensions += 1;
        }
        if let Some(s) = self.history_score {
            sum += s * weights.history;
            weight_sum += weights.history;
            dimensions += 1;
        }

        self.available_dimensions = dimensions;
        self.total = if weight_sum > 0.0 {
            sum / weight_sum
        } else {
            0.0
        };
    }

    pub fn format_for_llm(&self) -> String {
        fn fmt(score: Option<f32>, label: &str, desc: &str) -> String {
            match score {
                Some(s) => format!("- {}: {:.2} ({})", label, s, desc),
                None => format!("- {}: N/A (no data available)", label),
            }
        }

        let mut lines = vec![
            "## Multi-dimensional Task Score (0.0-1.0, higher = simpler)".to_string(),
            fmt(self.file_score, "File complexity", "fewer files = higher"),
            fmt(
                self.dependency_score,
                "Dependency complexity",
                "fewer deps = higher",
            ),
            fmt(
                self.pattern_score,
                "Pattern confidence",
                "similar patterns succeeded = higher",
            ),
            fmt(
                self.confidence_score,
                "Evidence confidence",
                "higher confidence = more certain",
            ),
            fmt(
                self.history_score,
                "Historical success",
                "similar tasks succeeded = higher",
            ),
        ];

        if self.available_dimensions > 0 {
            lines.push(format!(
                "- **Overall score: {:.2}** (based on {}/5 available dimensions)",
                self.total, self.available_dimensions
            ));
        } else {
            lines.push("- **Overall score: N/A** (no scoring data available)".to_string());
        }

        lines.join("\n")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoricalTask {
    pub description: String,
    pub affected_files: Vec<String>,
    pub success: bool,
    pub duration_secs: Option<u64>,
    pub tokens: Vec<String>,
}

impl HistoricalTask {
    pub fn new(description: &str, affected_files: Vec<String>, success: bool) -> Self {
        Self {
            description: description.to_string(),
            affected_files,
            success,
            duration_secs: None,
            tokens: tokenize(description),
        }
    }
}

pub struct TaskScorer {
    config: TaskScoringConfig,
    pattern_bank: Option<Arc<PatternBank>>,
    similarity: SimilarityCalculator,
    history_cache: Arc<RwLock<Vec<HistoricalTask>>>,
}

impl TaskScorer {
    pub fn new(config: TaskScoringConfig) -> Self {
        Self {
            similarity: SimilarityCalculator::new(config.weights.clone()),
            config,
            pattern_bank: None,
            history_cache: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn with_pattern_bank(mut self, bank: Arc<PatternBank>) -> Self {
        self.pattern_bank = Some(bank);
        self
    }

    pub fn add_historical_task(&self, task: HistoricalTask) {
        let mut cache = self.history_cache.write();
        if cache.len() >= self.config.max_history_tasks {
            cache.remove(0);
        }
        cache.push(task);
    }

    pub fn load_history_from_slice(&self, tasks: &[HistoricalTask]) {
        let mut cache = self.history_cache.write();
        cache.clear();
        cache.extend(tasks.iter().cloned());
        debug!(count = cache.len(), "Loaded historical tasks");
    }

    pub fn score(&self, task: &Task, evidence: Option<&Evidence>) -> TaskScore {
        let file_score = self.calculate_file_score(evidence);
        let dependency_score = self.calculate_dependency_score(evidence);
        let pattern_score = self.calculate_pattern_score();
        let confidence_score = self.calculate_confidence_score(evidence);
        let history_score = self.calculate_history_score(task);

        let mut score = TaskScore {
            file_score,
            dependency_score,
            pattern_score,
            confidence_score,
            history_score,
            total: 0.0,
            available_dimensions: 0,
        };
        score.calculate_total(&self.config.weights);
        score
    }

    fn calculate_file_score(&self, evidence: Option<&Evidence>) -> Option<f32> {
        let ev = evidence?;
        if ev.codebase_analysis.relevant_files.is_empty() {
            return None;
        }

        let file_count = ev.codebase_analysis.relevant_files.len();
        let max_files = self.config.max_file_threshold as f32;
        let normalized = 1.0 - (file_count as f32 / max_files).min(1.0);

        debug!(file_count, normalized, "File score calculated");
        Some(normalized)
    }

    fn calculate_dependency_score(&self, evidence: Option<&Evidence>) -> Option<f32> {
        let ev = evidence?;
        let dep_count = ev.dependency_analysis.current_dependencies.len()
            + ev.dependency_analysis.suggested_additions.len();

        let max_deps = self.config.max_dependency_threshold as f32;
        let normalized = 1.0 - (dep_count as f32 / max_deps).min(1.0);

        debug!(dep_count, normalized, "Dependency score calculated");
        Some(normalized)
    }

    fn calculate_pattern_score(&self) -> Option<f32> {
        let bank = self.pattern_bank.as_ref()?;
        let stats = bank.statistics();

        if stats.total_patterns == 0 {
            return None;
        }

        let success_rate = stats.overall_success_rate();
        debug!(success_rate, "Pattern score from PatternBank");
        Some(success_rate)
    }

    fn calculate_confidence_score(&self, evidence: Option<&Evidence>) -> Option<f32> {
        let ev = evidence?;
        if ev.codebase_analysis.relevant_files.is_empty() {
            return None;
        }

        let avg_confidence: f32 = ev
            .codebase_analysis
            .relevant_files
            .iter()
            .map(|f| f.confidence)
            .sum::<f32>()
            / ev.codebase_analysis.relevant_files.len() as f32;

        debug!(avg_confidence, "Confidence score calculated");
        Some(avg_confidence)
    }

    /// Score complexity based on description and evidence (without full Task).
    /// Used by ComplexityEstimator for complexity routing.
    pub fn score_for_description(
        &self,
        description: &str,
        evidence: Option<&Evidence>,
    ) -> TaskScore {
        let file_score = self.calculate_file_score(evidence);
        let dependency_score = self.calculate_dependency_score(evidence);
        let pattern_score = self.calculate_pattern_score();
        let confidence_score = self.calculate_confidence_score(evidence);
        let history_score = self.calculate_history_score_from_description(description);

        let mut score = TaskScore {
            file_score,
            dependency_score,
            pattern_score,
            confidence_score,
            history_score,
            total: 0.0,
            available_dimensions: 0,
        };
        score.calculate_total(&self.config.weights);
        score
    }

    fn calculate_history_score_from_description(&self, description: &str) -> Option<f32> {
        let history = self.history_cache.read();
        if history.is_empty() {
            return None;
        }

        let task_tokens = tokenize(description);
        let similar_tasks: Vec<_> = history
            .iter()
            .map(|h| {
                (
                    h,
                    self.similarity
                        .calculate_token_similarity(&task_tokens, &h.tokens),
                )
            })
            .filter(|(_, sim)| *sim > self.config.min_similarity_threshold)
            .collect();

        if similar_tasks.is_empty() {
            return None;
        }

        let weighted_success: f32 = similar_tasks
            .iter()
            .map(|(h, sim)| if h.success { *sim } else { 0.0 })
            .sum();

        let total_weight: f32 = similar_tasks.iter().map(|(_, sim)| *sim).sum();

        Some(weighted_success / total_weight)
    }

    fn calculate_history_score(&self, task: &Task) -> Option<f32> {
        let history = self.history_cache.read();
        if history.is_empty() {
            return None;
        }

        let task_tokens = tokenize(&task.description);
        let similar_tasks: Vec<_> = history
            .iter()
            .map(|h| {
                (
                    h,
                    self.similarity
                        .calculate_token_similarity(&task_tokens, &h.tokens),
                )
            })
            .filter(|(_, sim)| *sim > self.config.min_similarity_threshold)
            .collect();

        if similar_tasks.is_empty() {
            return None;
        }

        let weighted_success: f32 = similar_tasks
            .iter()
            .map(|(h, sim)| if h.success { *sim } else { 0.0 })
            .sum();

        let total_weight: f32 = similar_tasks.iter().map(|(_, sim)| *sim).sum();

        let score = weighted_success / total_weight;
        debug!(
            similar_count = similar_tasks.len(),
            score, "History score calculated"
        );
        Some(score)
    }
}

pub struct SimilarityCalculator {
    weights: TaskScoringWeights,
    idf_cache: Arc<RwLock<HashMap<String, f32>>>,
}

impl SimilarityCalculator {
    pub fn new(weights: TaskScoringWeights) -> Self {
        Self {
            weights,
            idf_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn calculate(&self, task: &Task, history: &[HistoricalTask]) -> f32 {
        if history.is_empty() {
            return 0.0;
        }

        let task_tokens = tokenize(&task.description);
        self.update_idf(&task_tokens, history);

        let mut max_similarity = 0.0f32;

        for historical in history {
            let token_sim = self.calculate_tfidf_similarity(&task_tokens, &historical.tokens);
            let structural_sim = self.calculate_structural_similarity(task, historical);
            let outcome_weight = if historical.success { 1.0 } else { 0.5 };

            let combined = token_sim * self.weights.file
                + structural_sim * self.weights.dependency
                + outcome_weight * self.weights.history;

            max_similarity = max_similarity.max(combined);
        }

        max_similarity
    }

    fn update_idf(&self, task_tokens: &[String], history: &[HistoricalTask]) {
        let doc_count = history.len() + 1;
        let mut token_doc_freq: HashMap<String, usize> = HashMap::new();

        for token in task_tokens {
            *token_doc_freq.entry(token.clone()).or_default() += 1;
        }

        for historical in history {
            let unique_tokens: HashSet<_> = historical.tokens.iter().collect();
            for token in unique_tokens {
                *token_doc_freq.entry(token.clone()).or_default() += 1;
            }
        }

        let mut cache = self.idf_cache.write();
        for (token, freq) in token_doc_freq {
            let idf = (doc_count as f32 / (freq as f32 + 1.0)).ln() + 1.0;
            cache.insert(token, idf);
        }
    }

    fn calculate_tfidf_similarity(&self, tokens1: &[String], tokens2: &[String]) -> f32 {
        let cache = self.idf_cache.read();

        let vec1 = self.to_tfidf_vector(tokens1, &cache);
        let vec2 = self.to_tfidf_vector(tokens2, &cache);

        self.cosine_similarity(&vec1, &vec2)
    }

    fn to_tfidf_vector(
        &self,
        tokens: &[String],
        idf_cache: &HashMap<String, f32>,
    ) -> HashMap<String, f32> {
        let mut tf: HashMap<String, f32> = HashMap::new();
        let total = tokens.len() as f32;

        for token in tokens {
            *tf.entry(token.clone()).or_default() += 1.0 / total;
        }

        let mut tfidf = HashMap::new();
        for (token, freq) in tf {
            let idf = idf_cache.get(&token).copied().unwrap_or(1.0);
            tfidf.insert(token, freq * idf);
        }

        tfidf
    }

    fn cosine_similarity(&self, vec1: &HashMap<String, f32>, vec2: &HashMap<String, f32>) -> f32 {
        let dot_product: f32 = vec1
            .iter()
            .filter_map(|(k, v1)| vec2.get(k).map(|v2| v1 * v2))
            .sum();

        let norm1: f32 = vec1.values().map(|v| v * v).sum::<f32>().sqrt();
        let norm2: f32 = vec2.values().map(|v| v * v).sum::<f32>().sqrt();

        if norm1 == 0.0 || norm2 == 0.0 {
            return 0.0;
        }

        dot_product / (norm1 * norm2)
    }

    pub fn calculate_token_similarity(&self, tokens1: &[String], tokens2: &[String]) -> f32 {
        let set1: HashSet<_> = tokens1.iter().collect();
        let set2: HashSet<_> = tokens2.iter().collect();

        let intersection = set1.intersection(&set2).count();
        let union = set1.union(&set2).count();

        if union == 0 {
            return 0.0;
        }

        intersection as f32 / union as f32
    }

    fn calculate_structural_similarity(&self, task: &Task, historical: &HistoricalTask) -> f32 {
        let task_files: HashSet<_> = task
            .description
            .split_whitespace()
            .filter(|w| w.contains('.') || w.contains('/'))
            .collect();

        let historical_files: HashSet<_> = historical
            .affected_files
            .iter()
            .map(|s| s.as_str())
            .collect();

        let intersection = task_files.intersection(&historical_files).count();
        let union = task_files.union(&historical_files).count();

        if union == 0 {
            return 0.0;
        }

        intersection as f32 / union as f32
    }
}

/// Tokenize text for similarity matching.
///
/// Unicode-aware: Uses character count, not byte length.
/// Works for Latin, Cyrillic, Greek, and other alphabetic scripts.
///
/// LIMITATION: CJK (Chinese/Japanese/Korean) text doesn't have word boundaries
/// delimited by spaces/punctuation. Similarity matching for CJK-only task
/// descriptions may be less accurate. This is acceptable because:
/// 1. Mixed CJK+Latin (common in code) still works via Latin tokens
/// 2. File paths and identifiers (Latin) provide reliable matching signals
/// 3. Full semantic matching would require LLM, defeating the purpose of fast filtering
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| s.chars().count() > 2) // Use char count, not byte length
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Fix the authentication bug in login.rs");
        assert!(tokens.contains(&"authentication".to_string()));
        assert!(tokens.contains(&"bug".to_string()));
        assert!(tokens.contains(&"login".to_string()));
        // LLM-first: all words preserved, no stopword filtering
        assert!(tokens.contains(&"the".to_string()));
        assert!(tokens.contains(&"fix".to_string()));
    }

    #[test]
    fn test_scoring_weights_default() {
        let weights = TaskScoringWeights::default();
        let total = weights.file
            + weights.dependency
            + weights.pattern
            + weights.confidence
            + weights.history;
        assert!((total - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_task_score_calculation() {
        let mut score = TaskScore {
            file_score: Some(0.8),
            dependency_score: Some(0.7),
            pattern_score: Some(0.9),
            confidence_score: Some(0.6),
            history_score: Some(0.5),
            total: 0.0,
            available_dimensions: 0,
        };

        let weights = TaskScoringWeights::default();
        score.calculate_total(&weights);

        assert!(score.total > 0.0 && score.total <= 1.0);
        assert_eq!(score.available_dimensions, 5);
    }

    #[test]
    fn test_task_score_with_missing_dimensions() {
        let mut score = TaskScore {
            file_score: Some(0.8),
            dependency_score: Some(0.7),
            pattern_score: None,
            confidence_score: Some(0.6),
            history_score: None,
            total: 0.0,
            available_dimensions: 0,
        };

        let weights = TaskScoringWeights::default();
        score.calculate_total(&weights);

        assert!(score.total > 0.0 && score.total <= 1.0);
        assert_eq!(score.available_dimensions, 3);
    }

    #[test]
    fn test_task_score_format_for_llm() {
        let score = TaskScore {
            file_score: Some(0.8),
            dependency_score: Some(0.7),
            pattern_score: None,
            confidence_score: Some(0.6),
            history_score: None,
            total: 0.7,
            available_dimensions: 3,
        };

        let formatted = score.format_for_llm();
        assert!(formatted.contains("File complexity: 0.80"));
        assert!(formatted.contains("Pattern confidence: N/A"));
        assert!(formatted.contains("Historical success: N/A"));
        assert!(formatted.contains("3/5 available dimensions"));
    }

    #[test]
    fn test_similarity_calculator_token_similarity() {
        let calc = SimilarityCalculator::new(TaskScoringWeights::default());

        let tokens1 = vec![
            "authentication".to_string(),
            "bug".to_string(),
            "login".to_string(),
        ];
        let tokens2 = vec![
            "authentication".to_string(),
            "error".to_string(),
            "login".to_string(),
        ];

        let sim = calc.calculate_token_similarity(&tokens1, &tokens2);
        assert!(sim > 0.0);
        assert!(sim < 1.0);
    }

    #[test]
    fn test_cosine_similarity() {
        let calc = SimilarityCalculator::new(TaskScoringWeights::default());

        let mut vec1 = HashMap::new();
        vec1.insert("a".to_string(), 1.0);
        vec1.insert("b".to_string(), 1.0);

        let mut vec2 = HashMap::new();
        vec2.insert("a".to_string(), 1.0);
        vec2.insert("c".to_string(), 1.0);

        let sim = calc.cosine_similarity(&vec1, &vec2);
        assert!(sim > 0.0 && sim < 1.0);
    }

    #[test]
    fn test_historical_task_creation() {
        let task = HistoricalTask::new("Fix authentication bug", vec!["auth.rs".to_string()], true);

        assert!(task.success);
        assert!(!task.tokens.is_empty());
        assert!(task.tokens.contains(&"authentication".to_string()));
    }
}
