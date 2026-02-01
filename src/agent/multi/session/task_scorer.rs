//! Intelligent task-to-agent scoring for optimal assignment.
//!
//! Provides scoring algorithms to match tasks with the most suitable agents
//! based on capabilities, load, domain expertise, and task requirements.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::participant::{Participant, ParticipantRegistry};
use super::task_graph::TaskInfo;
use crate::agent::multi::AgentRole;

/// Score breakdown for task-agent matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskScore {
    /// Total weighted score (0.0 - 1.0).
    pub total: f32,
    /// Module match score.
    pub module_score: f32,
    /// Role match score.
    pub role_score: f32,
    /// Domain expertise score.
    pub domain_score: f32,
    /// Availability/load score.
    pub load_score: f32,
    /// Capacity score (can agent handle this task).
    pub capacity_score: f32,
    /// Optional notes about the scoring.
    pub notes: Vec<String>,
}

impl TaskScore {
    pub fn zero() -> Self {
        Self {
            total: 0.0,
            module_score: 0.0,
            role_score: 0.0,
            domain_score: 0.0,
            load_score: 0.0,
            capacity_score: 0.0,
            notes: Vec::new(),
        }
    }

    pub fn unavailable(reason: &str) -> Self {
        Self {
            total: 0.0,
            module_score: 0.0,
            role_score: 0.0,
            domain_score: 0.0,
            load_score: 0.0,
            capacity_score: 0.0,
            notes: vec![reason.to_string()],
        }
    }
}

/// Scored agent for ranking.
#[derive(Debug, Clone)]
pub struct ScoredAgent {
    pub agent_id: String,
    pub score: TaskScore,
}

impl ScoredAgent {
    pub fn new(agent_id: String, score: TaskScore) -> Self {
        Self { agent_id, score }
    }
}

/// Configuration for the task scorer.
#[derive(Debug, Clone)]
pub struct ScorerConfig {
    /// Weight for module matching (0.0 - 1.0).
    pub module_weight: f32,
    /// Weight for role matching.
    pub role_weight: f32,
    /// Weight for domain expertise.
    pub domain_weight: f32,
    /// Weight for current load.
    pub load_weight: f32,
    /// Weight for capacity.
    pub capacity_weight: f32,
    /// Minimum score threshold for assignment.
    pub min_threshold: f32,
    /// Prefer agents from same module.
    pub prefer_same_module: bool,
    /// Allow cross-module assignment.
    pub allow_cross_module: bool,
}

impl Default for ScorerConfig {
    fn default() -> Self {
        Self {
            module_weight: 0.25,
            role_weight: 0.30,
            domain_weight: 0.20,
            load_weight: 0.15,
            capacity_weight: 0.10,
            min_threshold: 0.3,
            prefer_same_module: true,
            allow_cross_module: true,
        }
    }
}

impl ScorerConfig {
    /// Strict config that requires same module.
    pub fn strict() -> Self {
        Self {
            module_weight: 0.40,
            role_weight: 0.30,
            domain_weight: 0.15,
            load_weight: 0.10,
            capacity_weight: 0.05,
            min_threshold: 0.5,
            prefer_same_module: true,
            allow_cross_module: false,
        }
    }

    /// Flexible config that allows any capable agent.
    pub fn flexible() -> Self {
        Self {
            module_weight: 0.10,
            role_weight: 0.35,
            domain_weight: 0.25,
            load_weight: 0.20,
            capacity_weight: 0.10,
            min_threshold: 0.2,
            prefer_same_module: false,
            allow_cross_module: true,
        }
    }

    fn total_weight(&self) -> f32 {
        self.module_weight
            + self.role_weight
            + self.domain_weight
            + self.load_weight
            + self.capacity_weight
    }
}

/// Task-to-agent scorer.
pub struct AgentTaskScorer {
    config: ScorerConfig,
    performance_history: HashMap<String, AgentPerformance>,
}

/// Historical performance data for an agent.
#[derive(Debug, Clone, Default)]
pub struct AgentPerformance {
    pub tasks_completed: u32,
    pub tasks_failed: u32,
    pub avg_completion_time_secs: f32,
    pub domain_successes: HashMap<String, u32>,
}

impl AgentPerformance {
    pub fn success_rate(&self) -> f32 {
        let total = self.tasks_completed + self.tasks_failed;
        if total == 0 {
            0.5 // Neutral for new agents
        } else {
            self.tasks_completed as f32 / total as f32
        }
    }

    pub fn domain_success_rate(&self, domain: &str) -> f32 {
        self.domain_successes
            .get(domain)
            .map(|&successes| {
                let total = self.tasks_completed.max(1);
                (successes as f32 / total as f32).min(1.0)
            })
            .unwrap_or(0.5)
    }
}

impl AgentTaskScorer {
    pub fn new(config: ScorerConfig) -> Self {
        Self {
            config,
            performance_history: HashMap::new(),
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(ScorerConfig::default())
    }

    /// Score a single agent for a task.
    pub fn score(&self, task: &TaskInfo, agent: &Participant) -> TaskScore {
        // Check availability first
        if !agent.status.is_available() {
            return TaskScore::unavailable(&format!(
                "Agent status is {:?}, not available",
                agent.status
            ));
        }

        // Check capacity
        if !agent.has_capacity() {
            return TaskScore::unavailable("Agent at maximum capacity");
        }

        // Check module constraint
        if !self.config.allow_cross_module && agent.module != task.module {
            return TaskScore::unavailable("Cross-module assignment not allowed");
        }

        let mut score = TaskScore::zero();
        let mut notes = Vec::new();

        // Module score
        score.module_score = if agent.module == task.module {
            notes.push("Exact module match".to_string());
            1.0
        } else if self.config.prefer_same_module {
            notes.push("Different module".to_string());
            0.3
        } else {
            0.5
        };

        // Role score
        let required_role = AgentRole::new(&task.required_role);
        score.role_score = if agent.capabilities.can_fulfill_role(&required_role) {
            notes.push(format!("Can fulfill role '{}'", task.required_role));
            1.0
        } else {
            notes.push(format!("Cannot fulfill role '{}'", task.required_role));
            0.0
        };

        // Domain score
        score.domain_score = self.calculate_domain_score(task, agent, &mut notes);

        // Load score (lower load = higher score)
        let load_ratio =
            agent.current_load() as f32 / agent.capabilities.max_concurrent_tasks.max(1) as f32;
        score.load_score = 1.0 - load_ratio;
        if load_ratio > 0.5 {
            notes.push(format!("High load: {:.0}%", load_ratio * 100.0));
        }

        // Capacity score
        let task_tokens = task.estimated_complexity;
        let budget = agent.capabilities.context_budget;
        score.capacity_score = if budget == 0 {
            0.5 // Unknown budget, neutral score
        } else if task_tokens <= budget {
            let utilization = task_tokens as f32 / budget as f32;
            if (0.4..=0.8).contains(&utilization) {
                1.0
            } else if utilization < 0.4 {
                0.7 + (utilization / 0.4) * 0.3 // 0.7 - 1.0
            } else {
                0.5 + (1.0 - utilization) * 0.5 // 0.5 - 1.0
            }
        } else {
            notes.push("Task may exceed context budget".to_string());
            0.2
        };

        // Calculate weighted total
        let weights = &self.config;
        score.total = (score.module_score * weights.module_weight
            + score.role_score * weights.role_weight
            + score.domain_score * weights.domain_weight
            + score.load_score * weights.load_weight
            + score.capacity_score * weights.capacity_weight)
            / weights.total_weight();

        // Apply performance bonus/penalty if we have history
        if let Some(perf) = self.performance_history.get(agent.id.as_str()) {
            let success_rate = perf.success_rate();
            if success_rate > 0.8 {
                score.total *= 1.1; // 10% bonus for high performers
                notes.push(format!(
                    "High performer bonus ({}% success)",
                    (success_rate * 100.0) as u32
                ));
            } else if success_rate < 0.5 {
                score.total *= 0.9; // 10% penalty for poor performers
                notes.push(format!(
                    "Performance penalty ({}% success)",
                    (success_rate * 100.0) as u32
                ));
            }
        }

        score.total = score.total.clamp(0.0, 1.0);
        score.notes = notes;
        score
    }

    fn calculate_domain_score(
        &self,
        task: &TaskInfo,
        agent: &Participant,
        notes: &mut Vec<String>,
    ) -> f32 {
        if agent.capabilities.domains.is_empty() {
            return 0.5; // No domain info, neutral
        }

        // Check if task files match agent's domains
        let task_domains = self.infer_domains_from_task(task);
        if task_domains.is_empty() {
            return 0.5;
        }

        let matching_domains: Vec<&str> = task_domains
            .iter()
            .filter(|d| agent.capabilities.has_domain(d))
            .map(|s| s.as_str())
            .collect();

        if matching_domains.is_empty() {
            0.3
        } else {
            let match_ratio = matching_domains.len() as f32 / task_domains.len() as f32;
            notes.push(format!(
                "Domain match: {} ({}%)",
                matching_domains.join(", "),
                (match_ratio * 100.0) as u32
            ));
            0.5 + match_ratio * 0.5
        }
    }

    fn infer_domains_from_task(&self, task: &TaskInfo) -> Vec<String> {
        let mut domains = Vec::new();

        // Infer from module
        domains.push(task.module.clone());

        // Infer from file paths
        for file in &task.affected_files {
            let path_lower = file.to_lowercase();
            if path_lower.contains("auth") {
                domains.push("auth".to_string());
            }
            if path_lower.contains("api") {
                domains.push("api".to_string());
            }
            if path_lower.contains("database") || path_lower.contains("db") {
                domains.push("database".to_string());
            }
            if path_lower.contains("test") {
                domains.push("testing".to_string());
            }
            if path_lower.contains("config") {
                domains.push("configuration".to_string());
            }
        }

        // Deduplicate
        domains.sort();
        domains.dedup();
        domains
    }

    /// Score all agents for a task and return ranked list.
    pub fn rank_agents(&self, task: &TaskInfo, registry: &ParticipantRegistry) -> Vec<ScoredAgent> {
        let mut scored: Vec<ScoredAgent> = registry
            .active()
            .into_iter()
            .map(|agent| {
                let score = self.score(task, agent);
                ScoredAgent::new(agent.id.as_str().to_string(), score)
            })
            .filter(|s| s.score.total >= self.config.min_threshold)
            .collect();

        // Sort by score descending
        scored.sort_by(|a, b| {
            b.score
                .total
                .partial_cmp(&a.score.total)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        scored
    }

    /// Find the best agent for a task.
    pub fn find_best(
        &self,
        task: &TaskInfo,
        registry: &ParticipantRegistry,
    ) -> Option<ScoredAgent> {
        self.rank_agents(task, registry).into_iter().next()
    }

    /// Find top N agents for a task.
    pub fn find_top_n(
        &self,
        task: &TaskInfo,
        registry: &ParticipantRegistry,
        n: usize,
    ) -> Vec<ScoredAgent> {
        self.rank_agents(task, registry)
            .into_iter()
            .take(n)
            .collect()
    }

    /// Record task completion for performance tracking.
    pub fn record_completion(&mut self, agent_id: &str, domain: Option<&str>, success: bool) {
        let perf = self
            .performance_history
            .entry(agent_id.to_string())
            .or_default();

        if success {
            perf.tasks_completed += 1;
            if let Some(d) = domain {
                *perf.domain_successes.entry(d.to_string()).or_insert(0) += 1;
            }
        } else {
            perf.tasks_failed += 1;
        }
    }

    /// Get performance history for an agent.
    pub fn get_performance(&self, agent_id: &str) -> Option<&AgentPerformance> {
        self.performance_history.get(agent_id)
    }

    /// Clear performance history.
    pub fn clear_history(&mut self) {
        self.performance_history.clear();
    }

    /// Get current config.
    pub fn config(&self) -> &ScorerConfig {
        &self.config
    }

    /// Update config.
    pub fn set_config(&mut self, config: ScorerConfig) {
        self.config = config;
    }
}

/// Assignment result from the scorer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentResult {
    pub task_id: String,
    pub agent_id: Option<String>,
    pub score: Option<TaskScore>,
    pub alternatives: Vec<(String, f32)>, // (agent_id, score)
    pub reason: String,
}

impl AssignmentResult {
    pub fn assigned(
        task_id: String,
        agent_id: String,
        score: TaskScore,
        alternatives: Vec<ScoredAgent>,
    ) -> Self {
        Self {
            task_id,
            agent_id: Some(agent_id),
            score: Some(score),
            alternatives: alternatives
                .into_iter()
                .map(|s| (s.agent_id, s.score.total))
                .collect(),
            reason: "Best matching agent selected".to_string(),
        }
    }

    pub fn unassigned(task_id: String, reason: &str) -> Self {
        Self {
            task_id,
            agent_id: None,
            score: None,
            alternatives: Vec::new(),
            reason: reason.to_string(),
        }
    }

    pub fn is_assigned(&self) -> bool {
        self.agent_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::AgentId;
    use crate::agent::multi::session::participant::AgentCapabilities;

    fn create_test_task(id: &str, module: &str, role: &str, complexity: u32) -> TaskInfo {
        TaskInfo {
            id: id.to_string(),
            description: "Test task".to_string(),
            module: module.to_string(),
            required_role: role.to_string(),
            estimated_complexity: complexity,
            priority: 1,
            affected_files: vec!["auth/handler.rs".to_string()],
        }
    }

    fn create_test_participant(id: &str, module: &str, role: &str, load: u32) -> Participant {
        let mut p = Participant::new(
            AgentId::new(id),
            module.to_string(),
            "workspace-a".to_string(),
        );
        p.capabilities = AgentCapabilities {
            roles: vec![AgentRole::new(role)],
            max_concurrent_tasks: 3,
            context_budget: 100000,
            domains: vec!["auth".to_string(), "api".to_string()],
            languages: vec!["rust".to_string()],
        };
        p.mark_ready();
        for i in 0..load {
            p.assign_task(format!("existing-task-{}", i));
        }
        p
    }

    #[test]
    fn test_perfect_match() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);
        let agent = create_test_participant("agent-1", "auth", "coder", 0);

        let score = scorer.score(&task, &agent);

        assert!(
            score.total > 0.8,
            "Perfect match should have high score: {}",
            score.total
        );
        assert_eq!(score.module_score, 1.0);
        assert_eq!(score.role_score, 1.0);
    }

    #[test]
    fn test_role_mismatch() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);
        let agent = create_test_participant("agent-1", "auth", "reviewer", 0);

        let score = scorer.score(&task, &agent);

        assert_eq!(score.role_score, 0.0);
        // Role weight is 0.30, so losing 30% of potential score
        // With other scores high, total should still be significantly reduced
        assert!(
            score.total < 0.85,
            "Role mismatch should lower score: {}",
            score.total
        );
    }

    #[test]
    fn test_module_mismatch() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);
        let agent = create_test_participant("agent-1", "api", "coder", 0);

        let score = scorer.score(&task, &agent);

        assert!(score.module_score < 1.0);
        assert!(score.total < 0.9, "Module mismatch should lower score");
    }

    #[test]
    fn test_high_load_penalty() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);

        let agent_idle = create_test_participant("agent-1", "auth", "coder", 0);
        let agent_busy = create_test_participant("agent-2", "auth", "coder", 2);

        let score_idle = scorer.score(&task, &agent_idle);
        let score_busy = scorer.score(&task, &agent_busy);

        assert!(score_idle.load_score > score_busy.load_score);
        assert!(score_idle.total > score_busy.total);
    }

    #[test]
    fn test_unavailable_agent() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);

        let mut agent = create_test_participant("agent-1", "auth", "coder", 0);
        agent.mark_busy();

        let score = scorer.score(&task, &agent);

        assert_eq!(score.total, 0.0);
        assert!(!score.notes.is_empty());
    }

    #[test]
    fn test_strict_config_blocks_cross_module() {
        let scorer = AgentTaskScorer::new(ScorerConfig::strict());
        let task = create_test_task("task-1", "auth", "coder", 50000);
        let agent = create_test_participant("agent-1", "api", "coder", 0);

        let score = scorer.score(&task, &agent);

        assert_eq!(score.total, 0.0, "Strict config should block cross-module");
    }

    #[test]
    fn test_rank_agents() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);

        let mut registry = ParticipantRegistry::new();
        registry.register(create_test_participant("agent-1", "auth", "coder", 0)); // Best
        registry.register(create_test_participant("agent-2", "api", "coder", 0)); // Module mismatch
        registry.register(create_test_participant("agent-3", "auth", "coder", 2)); // High load

        let ranked = scorer.rank_agents(&task, &registry);

        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].agent_id, "agent-1");
    }

    #[test]
    fn test_find_best() {
        let scorer = AgentTaskScorer::with_default_config();
        let task = create_test_task("task-1", "auth", "coder", 50000);

        let mut registry = ParticipantRegistry::new();
        registry.register(create_test_participant("agent-1", "auth", "coder", 0));
        registry.register(create_test_participant("agent-2", "api", "coder", 0));

        let best = scorer.find_best(&task, &registry);

        assert!(best.is_some());
        assert_eq!(best.unwrap().agent_id, "agent-1");
    }

    #[test]
    fn test_no_suitable_agent() {
        // Use higher threshold that requires role match
        let mut scorer = AgentTaskScorer::with_default_config();
        scorer.config.min_threshold = 0.75; // High threshold requiring good role match

        let task = create_test_task("task-1", "auth", "architect", 50000);

        let mut registry = ParticipantRegistry::new();
        registry.register(create_test_participant("agent-1", "auth", "coder", 0));

        let score = scorer.score(&task, registry.active()[0]);
        let ranked = scorer.rank_agents(&task, &registry);

        // With role_score = 0, total should be below threshold
        assert!(
            ranked.is_empty(),
            "No suitable agents should pass 0.75 threshold. Actual score: {}",
            score.total
        );
    }

    #[test]
    fn test_performance_tracking() {
        let mut scorer = AgentTaskScorer::with_default_config();

        scorer.record_completion("agent-1", Some("auth"), true);
        scorer.record_completion("agent-1", Some("auth"), true);
        scorer.record_completion("agent-1", Some("api"), false);

        let perf = scorer.get_performance("agent-1").unwrap();

        assert_eq!(perf.tasks_completed, 2);
        assert_eq!(perf.tasks_failed, 1);
        assert!((perf.success_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_performance_bonus() {
        let mut scorer = AgentTaskScorer::with_default_config();

        // Record high success rate
        for _ in 0..10 {
            scorer.record_completion("agent-1", None, true);
        }

        let task = create_test_task("task-1", "auth", "coder", 50000);
        let agent = create_test_participant("agent-1", "auth", "coder", 0);

        let score = scorer.score(&task, &agent);

        // Should have bonus applied
        assert!(score.notes.iter().any(|n| n.contains("bonus")));
    }
}
