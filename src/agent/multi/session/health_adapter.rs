//! Health monitoring adapter for session management.
//!
//! Integrates the existing HealthMonitor with OrchestrationSession
//! to provide session-aware health tracking, agent health status,
//! and recovery recommendations.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::participant::ParticipantRegistry;
use crate::agent::multi::AgentId;

/// Health status for a session participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantHealth {
    /// Agent is operating normally.
    Healthy,
    /// Agent shows signs of stress (high load, slow responses).
    Stressed,
    /// Agent is unresponsive or consistently failing.
    Unhealthy,
    /// Agent health is unknown (not enough data).
    Unknown,
}

/// Health metrics for a participant.
#[derive(Debug, Clone)]
pub struct ParticipantHealthMetrics {
    /// Current health status.
    pub status: ParticipantHealth,
    /// Recent response times in milliseconds.
    pub response_times_ms: Vec<u64>,
    /// Average response time.
    pub avg_response_ms: u64,
    /// Success rate (0.0 - 1.0).
    pub success_rate: f32,
    /// Number of tasks completed.
    pub tasks_completed: u32,
    /// Number of tasks failed.
    pub tasks_failed: u32,
    /// Current task load.
    pub current_load: u32,
    /// Last activity time.
    pub last_activity: Option<Instant>,
    /// Last check time.
    pub last_check: Instant,
}

impl Default for ParticipantHealthMetrics {
    fn default() -> Self {
        Self {
            status: ParticipantHealth::Unknown,
            response_times_ms: Vec::new(),
            avg_response_ms: 0,
            success_rate: 1.0,
            tasks_completed: 0,
            tasks_failed: 0,
            current_load: 0,
            last_activity: None,
            last_check: Instant::now(),
        }
    }
}

impl ParticipantHealthMetrics {
    /// Record a task completion.
    pub fn record_completion(&mut self, response_time_ms: u64) {
        self.tasks_completed += 1;
        self.record_response_time(response_time_ms);
        self.last_activity = Some(Instant::now());
        self.update_status();
    }

    /// Record a task failure.
    pub fn record_failure(&mut self) {
        self.tasks_failed += 1;
        self.last_activity = Some(Instant::now());
        self.update_status();
    }

    fn record_response_time(&mut self, ms: u64) {
        self.response_times_ms.push(ms);
        // Keep last 20 response times
        if self.response_times_ms.len() > 20 {
            self.response_times_ms.remove(0);
        }
        self.avg_response_ms = if self.response_times_ms.is_empty() {
            0
        } else {
            self.response_times_ms.iter().sum::<u64>() / self.response_times_ms.len() as u64
        };
    }

    fn update_status(&mut self) {
        let total_tasks = self.tasks_completed + self.tasks_failed;
        self.success_rate = if total_tasks == 0 {
            1.0
        } else {
            self.tasks_completed as f32 / total_tasks as f32
        };

        self.last_check = Instant::now();

        // Determine health status
        if total_tasks < 3 {
            self.status = ParticipantHealth::Unknown;
        } else if self.success_rate < 0.5 || self.avg_response_ms > 300_000 {
            self.status = ParticipantHealth::Unhealthy;
        } else if self.success_rate < 0.8 || self.avg_response_ms > 120_000 {
            self.status = ParticipantHealth::Stressed;
        } else {
            self.status = ParticipantHealth::Healthy;
        }
    }

    /// Check if participant is considered active.
    pub fn is_active(&self) -> bool {
        self.last_activity
            .map(|t| t.elapsed() < Duration::from_secs(600))
            .unwrap_or(false)
    }

    /// Get idle duration.
    pub fn idle_duration(&self) -> Duration {
        self.last_activity
            .map(|t| t.elapsed())
            .unwrap_or(Duration::MAX)
    }
}

/// Configuration for health monitoring.
#[derive(Debug, Clone)]
pub struct SessionHealthConfig {
    /// Maximum average response time before considered stressed (ms).
    pub max_avg_response_ms: u64,
    /// Minimum success rate before considered unhealthy.
    pub min_success_rate: f32,
    /// Idle time before considering agent stalled.
    pub stall_timeout: Duration,
    /// Check interval.
    pub check_interval: Duration,
    /// Max retries for unhealthy agents before removal.
    pub max_unhealthy_retries: u32,
}

impl Default for SessionHealthConfig {
    fn default() -> Self {
        Self {
            max_avg_response_ms: 120_000, // 2 minutes
            min_success_rate: 0.7,
            stall_timeout: Duration::from_secs(600), // 10 minutes
            check_interval: Duration::from_secs(30),
            max_unhealthy_retries: 3,
        }
    }
}

/// Health tracker for session participants.
#[derive(Debug)]
pub struct SessionHealthTracker {
    config: SessionHealthConfig,
    metrics: HashMap<String, ParticipantHealthMetrics>,
    unhealthy_counts: HashMap<String, u32>,
    last_check: Instant,
}

impl SessionHealthTracker {
    pub fn new(config: SessionHealthConfig) -> Self {
        Self {
            config,
            metrics: HashMap::new(),
            unhealthy_counts: HashMap::new(),
            last_check: Instant::now(),
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(SessionHealthConfig::default())
    }

    /// Register a participant for tracking.
    pub fn register(&mut self, agent_id: &AgentId) {
        self.metrics
            .entry(agent_id.as_str().to_string())
            .or_default();
    }

    /// Unregister a participant.
    pub fn unregister(&mut self, agent_id: &AgentId) {
        self.metrics.remove(agent_id.as_str());
        self.unhealthy_counts.remove(agent_id.as_str());
    }

    /// Record a task completion.
    pub fn record_task_completion(&mut self, agent_id: &AgentId, response_time_ms: u64) {
        if let Some(metrics) = self.metrics.get_mut(agent_id.as_str()) {
            metrics.record_completion(response_time_ms);
            // Reset unhealthy count on successful completion
            self.unhealthy_counts.remove(agent_id.as_str());
        }
    }

    /// Record a task failure.
    pub fn record_task_failure(&mut self, agent_id: &AgentId) {
        if let Some(metrics) = self.metrics.get_mut(agent_id.as_str()) {
            metrics.record_failure();
        }
        // Increment unhealthy count
        *self
            .unhealthy_counts
            .entry(agent_id.as_str().to_string())
            .or_insert(0) += 1;
    }

    /// Update load for a participant.
    pub fn update_load(&mut self, agent_id: &AgentId, load: u32) {
        if let Some(metrics) = self.metrics.get_mut(agent_id.as_str()) {
            metrics.current_load = load;
        }
    }

    /// Get health metrics for a participant.
    pub fn get_metrics(&self, agent_id: &AgentId) -> Option<&ParticipantHealthMetrics> {
        self.metrics.get(agent_id.as_str())
    }

    /// Get health status for a participant.
    pub fn get_health(&self, agent_id: &AgentId) -> ParticipantHealth {
        self.metrics
            .get(agent_id.as_str())
            .map(|m| m.status)
            .unwrap_or(ParticipantHealth::Unknown)
    }

    /// Check all participants and return health report.
    pub fn check_all(&mut self, registry: &ParticipantRegistry) -> SessionHealthReport {
        self.last_check = Instant::now();

        let mut healthy = Vec::new();
        let mut stressed = Vec::new();
        let mut unhealthy = Vec::new();
        let mut stalled = Vec::new();
        let mut recommendations = Vec::new();

        for participant in registry.active() {
            let agent_id = participant.id.as_str();

            // Get or create metrics
            let metrics = self.metrics.entry(agent_id.to_string()).or_default();

            // Update load from participant
            metrics.current_load = participant.current_load();

            // Check for stalled agents
            if metrics.idle_duration() > self.config.stall_timeout {
                stalled.push(agent_id.to_string());
                recommendations.push(HealthRecommendation {
                    agent_id: agent_id.to_string(),
                    action: RecommendedAction::CheckStalled,
                    reason: format!("Agent idle for {}s", metrics.idle_duration().as_secs()),
                });
                continue;
            }

            // Categorize by health
            match metrics.status {
                ParticipantHealth::Healthy => healthy.push(agent_id.to_string()),
                ParticipantHealth::Stressed => {
                    stressed.push(agent_id.to_string());
                    recommendations.push(HealthRecommendation {
                        agent_id: agent_id.to_string(),
                        action: RecommendedAction::ReduceLoad,
                        reason: format!(
                            "Success rate: {:.0}%, Avg response: {}ms",
                            metrics.success_rate * 100.0,
                            metrics.avg_response_ms
                        ),
                    });
                }
                ParticipantHealth::Unhealthy => {
                    unhealthy.push(agent_id.to_string());
                    let unhealthy_count = self.unhealthy_counts.get(agent_id).copied().unwrap_or(0);

                    if unhealthy_count >= self.config.max_unhealthy_retries {
                        recommendations.push(HealthRecommendation {
                            agent_id: agent_id.to_string(),
                            action: RecommendedAction::Remove,
                            reason: format!("Failed {} times consecutively", unhealthy_count),
                        });
                    } else {
                        recommendations.push(HealthRecommendation {
                            agent_id: agent_id.to_string(),
                            action: RecommendedAction::Retry,
                            reason: format!(
                                "Unhealthy ({}/{})",
                                unhealthy_count, self.config.max_unhealthy_retries
                            ),
                        });
                    }
                }
                ParticipantHealth::Unknown => {
                    // Not enough data yet
                }
            }
        }

        // Calculate overall status
        let total = healthy.len() + stressed.len() + unhealthy.len() + stalled.len();
        let overall_status = if unhealthy.len() + stalled.len() > total / 2 {
            SessionHealthStatus::Critical
        } else if stressed.len() + unhealthy.len() > total / 4 {
            SessionHealthStatus::Degraded
        } else {
            SessionHealthStatus::Healthy
        };

        // Calculate health score
        let health_score = if total == 0 {
            1.0
        } else {
            let healthy_weight = 1.0;
            let stressed_weight = 0.6;
            let unhealthy_weight = 0.2;
            let stalled_weight = 0.0;

            (healthy.len() as f32 * healthy_weight
                + stressed.len() as f32 * stressed_weight
                + unhealthy.len() as f32 * unhealthy_weight
                + stalled.len() as f32 * stalled_weight)
                / total as f32
        };

        SessionHealthReport {
            status: overall_status,
            health_score,
            healthy_count: healthy.len(),
            stressed_count: stressed.len(),
            unhealthy_count: unhealthy.len(),
            stalled_count: stalled.len(),
            healthy_agents: healthy,
            stressed_agents: stressed,
            unhealthy_agents: unhealthy,
            stalled_agents: stalled,
            recommendations,
            timestamp: Instant::now(),
        }
    }

    /// Get agents that should be avoided for new tasks.
    pub fn agents_to_avoid(&self) -> Vec<String> {
        self.metrics
            .iter()
            .filter(|(_, m)| m.status == ParticipantHealth::Unhealthy)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Get healthy agents suitable for tasks.
    pub fn healthy_agents(&self) -> Vec<String> {
        self.metrics
            .iter()
            .filter(|(_, m)| m.status == ParticipantHealth::Healthy)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Clear metrics for all agents.
    pub fn clear(&mut self) {
        self.metrics.clear();
        self.unhealthy_counts.clear();
    }

    /// Get config.
    pub fn config(&self) -> &SessionHealthConfig {
        &self.config
    }

    /// Update config.
    pub fn set_config(&mut self, config: SessionHealthConfig) {
        self.config = config;
    }
}

/// Overall session health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionHealthStatus {
    Healthy,
    Degraded,
    Critical,
}

/// Health report for the session.
#[derive(Debug, Clone)]
pub struct SessionHealthReport {
    pub status: SessionHealthStatus,
    pub health_score: f32,
    pub healthy_count: usize,
    pub stressed_count: usize,
    pub unhealthy_count: usize,
    pub stalled_count: usize,
    pub healthy_agents: Vec<String>,
    pub stressed_agents: Vec<String>,
    pub unhealthy_agents: Vec<String>,
    pub stalled_agents: Vec<String>,
    pub recommendations: Vec<HealthRecommendation>,
    pub timestamp: Instant,
}

impl SessionHealthReport {
    pub fn total_agents(&self) -> usize {
        self.healthy_count + self.stressed_count + self.unhealthy_count + self.stalled_count
    }

    pub fn has_critical_issues(&self) -> bool {
        self.status == SessionHealthStatus::Critical
    }

    pub fn needs_attention(&self) -> bool {
        !self.recommendations.is_empty()
    }
}

/// Recommended action for a health issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthRecommendation {
    pub agent_id: String,
    pub action: RecommendedAction,
    pub reason: String,
}

/// Actions that can be taken to address health issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecommendedAction {
    /// Reduce load on this agent.
    ReduceLoad,
    /// Retry tasks that failed.
    Retry,
    /// Check if agent is stalled and restart.
    CheckStalled,
    /// Remove agent from pool.
    Remove,
    /// Add more agents of this type.
    ScaleUp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::AgentRole;
    use crate::agent::multi::session::participant::{AgentCapabilities, Participant};

    fn create_test_participant(id: &str) -> Participant {
        let mut p = Participant::new(
            AgentId::new(id),
            "test".to_string(),
            "workspace".to_string(),
        );
        p.capabilities = AgentCapabilities {
            roles: vec![AgentRole::new("coder")],
            max_concurrent_tasks: 3,
            context_budget: 100000,
            domains: vec![],
            languages: vec![],
        };
        p.mark_ready();
        p
    }

    #[test]
    fn test_participant_health_metrics() {
        let mut metrics = ParticipantHealthMetrics::default();

        // Initially unknown
        assert_eq!(metrics.status, ParticipantHealth::Unknown);

        // Record completions
        for _ in 0..5 {
            metrics.record_completion(10000);
        }

        assert_eq!(metrics.tasks_completed, 5);
        assert_eq!(metrics.status, ParticipantHealth::Healthy);
        assert!(metrics.is_active());
    }

    #[test]
    fn test_health_degrades_on_failures() {
        let mut metrics = ParticipantHealthMetrics::default();

        // Some successes
        for _ in 0..3 {
            metrics.record_completion(10000);
        }

        // Many failures
        for _ in 0..7 {
            metrics.record_failure();
        }

        // 3/10 = 30% success rate -> unhealthy
        assert!(metrics.success_rate < 0.5);
        assert_eq!(metrics.status, ParticipantHealth::Unhealthy);
    }

    #[test]
    fn test_health_tracker_registration() {
        let mut tracker = SessionHealthTracker::with_default_config();
        let agent_id = AgentId::new("agent-1");

        tracker.register(&agent_id);
        assert!(tracker.get_metrics(&agent_id).is_some());

        tracker.unregister(&agent_id);
        assert!(tracker.get_metrics(&agent_id).is_none());
    }

    #[test]
    fn test_health_tracker_recording() {
        let mut tracker = SessionHealthTracker::with_default_config();
        let agent_id = AgentId::new("agent-1");

        tracker.register(&agent_id);

        // Record some completions
        for _ in 0..5 {
            tracker.record_task_completion(&agent_id, 5000);
        }

        let metrics = tracker.get_metrics(&agent_id).unwrap();
        assert_eq!(metrics.tasks_completed, 5);
        assert_eq!(metrics.status, ParticipantHealth::Healthy);

        // Record a failure
        tracker.record_task_failure(&agent_id);
        let metrics = tracker.get_metrics(&agent_id).unwrap();
        assert_eq!(metrics.tasks_failed, 1);
    }

    #[test]
    fn test_health_report() {
        let mut tracker = SessionHealthTracker::with_default_config();
        let mut registry = ParticipantRegistry::new();

        // Add healthy agent
        let agent1 = create_test_participant("agent-1");
        registry.register(agent1);
        tracker.register(&AgentId::new("agent-1"));
        for _ in 0..5 {
            tracker.record_task_completion(&AgentId::new("agent-1"), 5000);
        }

        // Add unhealthy agent
        let agent2 = create_test_participant("agent-2");
        registry.register(agent2);
        tracker.register(&AgentId::new("agent-2"));
        for _ in 0..3 {
            tracker.record_task_completion(&AgentId::new("agent-2"), 5000);
        }
        for _ in 0..7 {
            tracker.record_task_failure(&AgentId::new("agent-2"));
        }

        let report = tracker.check_all(&registry);

        assert!(report.healthy_count >= 1);
        assert!(report.unhealthy_count >= 1);
        assert!(!report.recommendations.is_empty());
    }

    #[test]
    fn test_agents_to_avoid() {
        let mut tracker = SessionHealthTracker::with_default_config();

        tracker.register(&AgentId::new("healthy-agent"));
        for _ in 0..5 {
            tracker.record_task_completion(&AgentId::new("healthy-agent"), 5000);
        }

        tracker.register(&AgentId::new("unhealthy-agent"));
        for _ in 0..3 {
            tracker.record_task_completion(&AgentId::new("unhealthy-agent"), 5000);
        }
        for _ in 0..7 {
            tracker.record_task_failure(&AgentId::new("unhealthy-agent"));
        }

        let avoid = tracker.agents_to_avoid();
        assert!(avoid.contains(&"unhealthy-agent".to_string()));
        assert!(!avoid.contains(&"healthy-agent".to_string()));
    }

    #[test]
    fn test_unhealthy_retry_limit() {
        let mut tracker = SessionHealthTracker::with_default_config();
        let agent_id = AgentId::new("agent-1");

        tracker.register(&agent_id);

        // Record 1 completion to establish activity (resets unhealthy_count)
        // After max_unhealthy_retries failures: total=4, success_rate=1/4=0.25 < 0.5 → Unhealthy
        tracker.record_task_completion(&agent_id, 5000);

        // Record failures up to retry limit
        for _ in 0..tracker.config.max_unhealthy_retries {
            tracker.record_task_failure(&agent_id);
        }

        let mut registry = ParticipantRegistry::new();
        let participant = create_test_participant("agent-1");
        registry.register(participant);

        let report = tracker.check_all(&registry);

        // Should have a Remove recommendation
        let remove_rec = report
            .recommendations
            .iter()
            .find(|r| r.agent_id == "agent-1" && r.action == RecommendedAction::Remove);
        assert!(remove_rec.is_some());
    }

    #[test]
    fn test_load_update() {
        let mut tracker = SessionHealthTracker::with_default_config();
        let agent_id = AgentId::new("agent-1");

        tracker.register(&agent_id);
        tracker.update_load(&agent_id, 5);

        let metrics = tracker.get_metrics(&agent_id).unwrap();
        assert_eq!(metrics.current_load, 5);
    }
}
