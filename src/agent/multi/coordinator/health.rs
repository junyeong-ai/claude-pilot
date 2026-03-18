//! Health monitoring methods for the Coordinator.

use tracing::{info, warn};

use super::super::health::HealthStatus;
use super::super::traits::AgentId;
use super::types::HealthAdaptation;
use super::Coordinator;

impl Coordinator {
    /// Record task completion in session health tracker.
    pub(super) fn record_task_completion(&self, agent_id: &AgentId, response_time_ms: u64) {
        self.session_health
            .write()
            .record_task_completion(agent_id, response_time_ms);
    }

    /// Record task failure in session health tracker.
    pub(super) fn record_task_failure(&self, agent_id: &AgentId) {
        self.session_health.write().record_task_failure(agent_id);
    }

    pub(super) fn check_health_adaptation(&self) -> HealthAdaptation {
        let report = self.health_monitor.check_health();
        match report.status {
            HealthStatus::Critical => {
                warn!(
                    score = %format!("{:.1}%", report.overall_score * 100.0),
                    bottlenecks = %report.bottlenecks.len(),
                    "Critical health status detected"
                );
                for bottleneck in &report.bottlenecks {
                    warn!(
                        role = %bottleneck.role_id,
                        severity = %format!("{:.2}", bottleneck.severity),
                        recommendation = %bottleneck.recommendation,
                        "Bottleneck detected"
                    );
                }
                if report.highest_severity() > 0.8 {
                    HealthAdaptation::Escalate {
                        reason: format!(
                            "Critical health: {:.0}% score, {} bottlenecks with severity > 0.8",
                            report.overall_score * 100.0,
                            report.bottlenecks.len()
                        ),
                    }
                } else {
                    HealthAdaptation::ForceSequential
                }
            }
            HealthStatus::Degraded => {
                info!(
                    score = %format!("{:.1}%", report.overall_score * 100.0),
                    alerts = %report.alerts.len(),
                    "Degraded health status"
                );
                HealthAdaptation::Throttle { delay_ms: 1000 }
            }
            HealthStatus::Healthy => HealthAdaptation::Continue,
        }
    }
}
