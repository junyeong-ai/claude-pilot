//! Health monitoring and bottleneck detection for multi-agent system.
//!
//! # Purpose
//!
//! Provides comprehensive health monitoring, bottleneck detection, and alert management
//! for the multi-agent pool system. Tracks agent performance, load distribution, and
//! system degradation to enable proactive capacity planning and optimization.
//!
//! # Key Features
//!
//! ## Health Status Levels
//! - **Healthy**: System operating within normal parameters
//! - **Degraded**: Some metrics exceeding thresholds, performance impacted
//! - **Critical**: Severe issues detected, immediate action required
//!
//! ## Bottleneck Detection
//! - **Insufficient Agents**: Role has no available agents
//! - **High Load**: Average load per agent exceeds threshold
//! - **High Latency**: Task execution time exceeds limits
//! - **High Failure Rate**: Success rate below minimum threshold
//!
//! ## Alert System
//! - Categorized alerts (LoadImbalance, HighLatency, HighFailureRate, etc.)
//! - Severity-based prioritization (Info, Warning, Critical)
//! - Actionable recommendations for remediation
//! - Historical alert tracking (maintains last 100 alerts)
//!
//! ## Role-Based Health Tracking
//! - Per-role health assessment with agent count and load metrics
//! - Individual role status evaluation (Healthy/Degraded/Critical)
//! - Role-specific bottleneck identification and recommendations
//!
//! ## Trend Analysis
//! - Historical health report tracking for pattern detection
//! - Degrading health score trend detection
//! - Recurring bottleneck identification
//! - Proactive capacity planning alerts
//!
//! # Location
//!
//! `src/agent/multi/health.rs`
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use claude_pilot::agent::multi::{AgentPool, HealthMonitor, HealthThresholds};
//! use claude_pilot::config::MultiAgentConfig;
//!
//! # fn main() {
//! let config = MultiAgentConfig::default();
//! let pool = Arc::new(AgentPool::new(config));
//!
//! // Create monitor with custom thresholds
//! let thresholds = HealthThresholds {
//!     max_load_per_agent: 5,
//!     max_avg_latency_ms: 30000,
//!     min_success_rate: 0.8,
//!     load_imbalance_threshold: 0.5,
//! };
//!
//! let monitor = HealthMonitor::new(pool).with_thresholds(thresholds);
//!
//! // Check system health
//! let report = monitor.check_health();
//! println!("Health status: {:?}", report.status);
//! println!("Overall score: {:.2}", report.overall_score);
//!
//! // Review alerts
//! for alert in report.alerts {
//!     println!("[{:?}] {}: {}", alert.severity, alert.message, alert.recommendation);
//! }
//!
//! // Identify bottlenecks
//! for bottleneck in report.bottlenecks {
//!     println!("Bottleneck in {}: {:?} (severity: {:.2})",
//!         bottleneck.role_id, bottleneck.bottleneck_type, bottleneck.severity);
//! }
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::RwLock;

use super::pool::{AgentPool, PoolStatistics};
use super::traits::MetricsSnapshot;

/// Health status levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Critical,
}

/// Alert severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// Health alert with recommendation.
#[derive(Debug, Clone)]
pub struct HealthAlert {
    pub severity: AlertSeverity,
    pub category: AlertCategory,
    pub message: String,
    pub recommendation: String,
    pub timestamp: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertCategory {
    LoadImbalance,
    HighLatency,
    HighFailureRate,
    ResourceExhaustion,
    Bottleneck,
}

/// Comprehensive health report.
#[derive(Debug, Clone)]
pub struct HealthReport {
    pub status: HealthStatus,
    pub alerts: Vec<HealthAlert>,
    pub bottlenecks: Vec<Bottleneck>,
    pub role_health: HashMap<String, RoleHealth>,
    pub overall_score: f64,
    pub timestamp: Instant,
}

#[derive(Debug, Clone)]
pub struct RoleHealth {
    pub role_id: String,
    pub agent_count: usize,
    pub total_load: u32,
    pub avg_load: f32,
    pub status: HealthStatus,
}

/// Detected bottleneck in the system.
#[derive(Debug, Clone)]
pub struct Bottleneck {
    pub role_id: String,
    pub bottleneck_type: BottleneckType,
    pub severity: f64,
    pub recommendation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottleneckType {
    InsufficientAgents,
    HighLoad,
    HighLatency,
    HighFailureRate,
}

/// Health monitoring for the agent pool.
pub struct HealthMonitor {
    pool: Arc<AgentPool>,
    thresholds: HealthThresholds,
    alert_history: RwLock<Vec<HealthAlert>>,
}

#[derive(Debug, Clone)]
pub struct HealthThresholds {
    pub max_load_per_agent: u32,
    pub max_avg_latency_ms: u64,
    pub min_success_rate: f64,
    pub load_imbalance_threshold: f32,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        Self {
            max_load_per_agent: 5,
            max_avg_latency_ms: 30000,
            min_success_rate: 0.8,
            load_imbalance_threshold: 0.5,
        }
    }
}

impl HealthMonitor {
    pub fn new(pool: Arc<AgentPool>) -> Self {
        Self {
            pool,
            thresholds: HealthThresholds::default(),
            alert_history: RwLock::new(Vec::new()),
        }
    }

    pub fn with_thresholds(mut self, thresholds: HealthThresholds) -> Self {
        self.thresholds = thresholds;
        self
    }

    pub fn check_health(&self) -> HealthReport {
        let stats = self.pool.statistics();
        let metrics = self.pool.metrics();

        let mut alerts = Vec::new();
        let mut bottlenecks = Vec::new();
        let mut role_health = HashMap::new();

        // Analyze each role
        for (role_id, role_stats) in &stats.by_role {
            let avg_load = if role_stats.count > 0 {
                role_stats.load as f32 / role_stats.count as f32
            } else {
                0.0
            };

            let status = self.assess_role_health(role_stats.count, avg_load, &metrics);

            role_health.insert(
                role_id.clone(),
                RoleHealth {
                    role_id: role_id.clone(),
                    agent_count: role_stats.count,
                    total_load: role_stats.load,
                    avg_load,
                    status,
                },
            );

            // Check for bottlenecks
            if let Some(bottleneck) =
                self.detect_bottleneck(role_id, role_stats.count, avg_load, &metrics)
            {
                bottlenecks.push(bottleneck);
            }
        }

        // Global health checks
        self.check_global_health(&stats, &metrics, &mut alerts);

        // Determine overall status
        let status = self.determine_overall_status(&alerts, &bottlenecks);
        let overall_score = self.calculate_health_score(&stats, &metrics);

        // Store alerts
        {
            let mut history = self.alert_history.write();
            history.extend(alerts.clone());
            // Keep only recent alerts (last 100)
            let len = history.len();
            if len > 100 {
                let drain_count = len - 100;
                history.drain(..drain_count);
            }
        }

        HealthReport {
            status,
            alerts,
            bottlenecks,
            role_health,
            overall_score,
            timestamp: Instant::now(),
        }
    }

    fn assess_role_health(
        &self,
        count: usize,
        avg_load: f32,
        _metrics: &MetricsSnapshot,
    ) -> HealthStatus {
        if count == 0 {
            return HealthStatus::Critical;
        }

        if avg_load > self.thresholds.max_load_per_agent as f32 {
            return HealthStatus::Critical;
        }

        if avg_load > self.thresholds.max_load_per_agent as f32 * 0.7 {
            return HealthStatus::Degraded;
        }

        HealthStatus::Healthy
    }

    fn detect_bottleneck(
        &self,
        role_id: &str,
        count: usize,
        avg_load: f32,
        metrics: &MetricsSnapshot,
    ) -> Option<Bottleneck> {
        if count == 0 {
            return Some(Bottleneck {
                role_id: role_id.to_string(),
                bottleneck_type: BottleneckType::InsufficientAgents,
                severity: 1.0,
                recommendation: format!("Add at least one agent for role '{}'", role_id),
            });
        }

        if avg_load > self.thresholds.max_load_per_agent as f32 {
            return Some(Bottleneck {
                role_id: role_id.to_string(),
                bottleneck_type: BottleneckType::HighLoad,
                severity: (avg_load / self.thresholds.max_load_per_agent as f32) as f64,
                recommendation: format!(
                    "Scale up agents for role '{}' or reduce task frequency",
                    role_id
                ),
            });
        }

        if metrics.avg_duration_ms > self.thresholds.max_avg_latency_ms {
            return Some(Bottleneck {
                role_id: role_id.to_string(),
                bottleneck_type: BottleneckType::HighLatency,
                severity: metrics.avg_duration_ms as f64
                    / self.thresholds.max_avg_latency_ms as f64,
                recommendation: "Optimize agent execution or increase timeout".to_string(),
            });
        }

        if metrics.total_executions > 10 && metrics.success_rate < self.thresholds.min_success_rate
        {
            return Some(Bottleneck {
                role_id: role_id.to_string(),
                bottleneck_type: BottleneckType::HighFailureRate,
                severity: 1.0 - metrics.success_rate,
                recommendation: "Investigate failure causes and improve agent prompts".to_string(),
            });
        }

        None
    }

    fn check_global_health(
        &self,
        stats: &PoolStatistics,
        metrics: &MetricsSnapshot,
        alerts: &mut Vec<HealthAlert>,
    ) {
        // Check for high failure rate
        if metrics.total_executions > 10 && metrics.success_rate < self.thresholds.min_success_rate
        {
            alerts.push(HealthAlert {
                severity: AlertSeverity::Warning,
                category: AlertCategory::HighFailureRate,
                message: format!(
                    "High failure rate: {:.1}% success rate",
                    metrics.success_rate * 100.0
                ),
                recommendation: "Review agent prompts and error patterns".to_string(),
                timestamp: Instant::now(),
            });
        }

        // Check for high latency
        if metrics.avg_duration_ms > self.thresholds.max_avg_latency_ms {
            alerts.push(HealthAlert {
                severity: AlertSeverity::Warning,
                category: AlertCategory::HighLatency,
                message: format!("High average latency: {}ms", metrics.avg_duration_ms),
                recommendation: "Consider optimizing prompts or increasing parallelism".to_string(),
                timestamp: Instant::now(),
            });
        }

        // Check for load imbalance
        let loads: Vec<f32> = stats
            .by_role
            .values()
            .filter(|r| r.count > 0)
            .map(|r| r.load as f32 / r.count as f32)
            .collect();

        if loads.len() > 1 {
            let max_load = loads.iter().copied().fold(0.0f32, f32::max);
            let min_load = loads.iter().copied().reduce(f32::min).unwrap_or(0.0);

            if max_load > 0.0
                && (max_load - min_load) / max_load > self.thresholds.load_imbalance_threshold
            {
                alerts.push(HealthAlert {
                    severity: AlertSeverity::Info,
                    category: AlertCategory::LoadImbalance,
                    message: format!(
                        "Load imbalance detected: max {:.1} vs min {:.1}",
                        max_load, min_load
                    ),
                    recommendation: "Consider rebalancing work distribution".to_string(),
                    timestamp: Instant::now(),
                });
            }
        }
    }

    fn determine_overall_status(
        &self,
        alerts: &[HealthAlert],
        bottlenecks: &[Bottleneck],
    ) -> HealthStatus {
        let has_critical = alerts.iter().any(|a| a.severity == AlertSeverity::Critical)
            || bottlenecks.iter().any(|b| b.severity >= 1.0);

        let has_warning = alerts.iter().any(|a| a.severity == AlertSeverity::Warning)
            || bottlenecks.iter().any(|b| b.severity >= 0.5);

        if has_critical {
            HealthStatus::Critical
        } else if has_warning {
            HealthStatus::Degraded
        } else {
            HealthStatus::Healthy
        }
    }

    fn calculate_health_score(&self, stats: &PoolStatistics, metrics: &MetricsSnapshot) -> f64 {
        let mut score = 1.0;

        // Penalize for failures
        if metrics.total_executions > 0 {
            score *= metrics.success_rate.max(0.1);
        }

        // Penalize for high latency
        if metrics.avg_duration_ms > self.thresholds.max_avg_latency_ms {
            let latency_factor =
                self.thresholds.max_avg_latency_ms as f64 / metrics.avg_duration_ms as f64;
            score *= latency_factor.max(0.3);
        }

        // Penalize for high load
        let total_capacity = stats.agent_count as u32 * self.thresholds.max_load_per_agent;
        if total_capacity > 0 && stats.total_load > 0 {
            let load_ratio = stats.total_load as f64 / total_capacity as f64;
            if load_ratio > 0.8 {
                // Prevent negative score: clamp the penalty factor to [0.0, 1.0]
                let penalty_factor = ((load_ratio - 0.8) * 2.0).min(1.0);
                score *= 1.0 - penalty_factor;
            }
        }

        score.clamp(0.0, 1.0)
    }

    pub fn recent_alerts(&self) -> Vec<HealthAlert> {
        self.alert_history.read().clone()
    }
}

impl HealthReport {
    pub fn highest_severity(&self) -> f64 {
        self.bottlenecks
            .iter()
            .map(|b| b.severity)
            .fold(0.0f64, f64::max)
    }
}

/// Bottleneck detector for proactive capacity planning.
pub struct BottleneckDetector {
    history: RwLock<Vec<HealthReport>>,
    max_history: usize,
}

impl Default for BottleneckDetector {
    fn default() -> Self {
        Self::new(50)
    }
}

impl BottleneckDetector {
    pub fn new(max_history: usize) -> Self {
        Self {
            history: RwLock::new(Vec::new()),
            max_history,
        }
    }

    pub fn record(&self, report: HealthReport) {
        let mut history = self.history.write();
        history.push(report);
        if history.len() > self.max_history {
            history.remove(0);
        }
    }

    pub fn analyze_trends(&self) -> Vec<TrendAlert> {
        let history = self.history.read();
        if history.len() < 5 {
            return vec![];
        }

        let mut alerts = Vec::new();

        // Check for degrading health score trend
        let recent: Vec<f64> = history
            .iter()
            .rev()
            .take(10)
            .map(|r| r.overall_score)
            .collect();
        if recent.len() >= 5 {
            let avg_recent: f64 = recent.iter().take(5).sum::<f64>() / 5.0;
            let older_samples: Vec<f64> = recent.iter().skip(5).take(5).copied().collect();
            let avg_older: f64 = if !older_samples.is_empty() {
                older_samples.iter().sum::<f64>() / older_samples.len() as f64
            } else {
                0.0
            };

            if avg_older > 0.0 && avg_recent < avg_older * 0.8 {
                alerts.push(TrendAlert {
                    trend_type: TrendType::DegradingHealth,
                    severity: (avg_older - avg_recent) / avg_older,
                    message: format!(
                        "Health score degrading: {:.1}% -> {:.1}%",
                        avg_older * 100.0,
                        avg_recent * 100.0
                    ),
                });
            }
        }

        // Check for recurring bottlenecks
        let bottleneck_counts: HashMap<String, usize> = history
            .iter()
            .flat_map(|r| r.bottlenecks.iter().map(|b| b.role_id.clone()))
            .fold(HashMap::new(), |mut acc, role| {
                *acc.entry(role).or_insert(0) += 1;
                acc
            });

        for (role_id, count) in bottleneck_counts {
            if count > history.len() / 2 {
                alerts.push(TrendAlert {
                    trend_type: TrendType::RecurringBottleneck,
                    severity: count as f64 / history.len() as f64,
                    message: format!(
                        "Recurring bottleneck on role '{}': {} of {} reports",
                        role_id,
                        count,
                        history.len()
                    ),
                });
            }
        }

        alerts
    }
}

#[derive(Debug, Clone)]
pub struct TrendAlert {
    pub trend_type: TrendType,
    pub severity: f64,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrendType {
    DegradingHealth,
    RecurringBottleneck,
    CapacityWarning,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MultiAgentConfig;

    #[test]
    fn test_health_thresholds_default() {
        let thresholds = HealthThresholds::default();
        assert_eq!(thresholds.max_load_per_agent, 5);
        assert_eq!(thresholds.min_success_rate, 0.8);
    }

    #[test]
    fn test_health_monitor_creation() {
        let config = MultiAgentConfig::default();
        let pool = Arc::new(AgentPool::new(config));
        let monitor = HealthMonitor::new(pool);

        let report = monitor.check_health();
        assert_eq!(report.status, HealthStatus::Healthy);
    }

    #[test]
    fn test_bottleneck_detector() {
        let detector = BottleneckDetector::new(10);
        assert!(detector.analyze_trends().is_empty());
    }
}
