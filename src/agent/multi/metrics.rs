//! Consensus metrics and observability hooks.
//!
//! Provides metrics collection for consensus sessions including:
//! - Session outcomes and durations
//! - Tier execution statistics
//! - Strategy distribution
//! - Timeout and convergence rates

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::RwLock;

use super::shared::{ConsensusOutcome, TierLevel};

/// Thread-safe consensus metrics collector.
#[derive(Debug, Default)]
pub struct ConsensusMetrics {
    sessions: SessionMetrics,
    tiers: TierMetrics,
    strategies: StrategyMetrics,
}

impl ConsensusMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record session start.
    pub fn session_started(&self, strategy: &str, participant_count: usize, tier_count: usize) {
        self.sessions.started.fetch_add(1, Ordering::Relaxed);
        self.strategies.record_selection(strategy);

        let mut dist = self.sessions.participant_distribution.write();
        *dist.entry(participant_count).or_insert(0) += 1;

        let mut tier_dist = self.sessions.tier_distribution.write();
        *tier_dist.entry(tier_count).or_insert(0) += 1;
    }

    /// Record session completion.
    pub fn session_completed(&self, outcome: ConsensusOutcome, duration: Duration, rounds: usize) {
        match outcome {
            ConsensusOutcome::Converged => {
                self.sessions.converged.fetch_add(1, Ordering::Relaxed);
            }
            ConsensusOutcome::PartialConvergence => {
                self.sessions.partial.fetch_add(1, Ordering::Relaxed);
            }
            ConsensusOutcome::Escalated => {
                self.sessions.escalated.fetch_add(1, Ordering::Relaxed);
            }
            ConsensusOutcome::Timeout => {
                self.sessions.timed_out.fetch_add(1, Ordering::Relaxed);
            }
            ConsensusOutcome::Failed => {
                self.sessions.failed.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.sessions
            .total_duration_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
        self.sessions
            .total_rounds
            .fetch_add(rounds as u64, Ordering::Relaxed);
    }

    /// Record tier execution start.
    pub fn tier_started(&self, tier_level: TierLevel, unit_count: usize) {
        let mut counts = self.tiers.execution_counts.write();
        *counts.entry(tier_level).or_insert(0) += 1;

        let mut unit_counts = self.tiers.unit_counts.write();
        *unit_counts.entry(tier_level).or_insert(0) += unit_count;
    }

    /// Record tier unit completion.
    pub fn tier_unit_completed(&self, tier_level: TierLevel, converged: bool, timed_out: bool) {
        if converged {
            let mut converged = self.tiers.converged_units.write();
            *converged.entry(tier_level).or_insert(0) += 1;
        }
        if timed_out {
            let mut timeouts = self.tiers.timed_out_units.write();
            *timeouts.entry(tier_level).or_insert(0) += 1;
        }
    }

    /// Get current metrics snapshot.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let started = self.sessions.started.load(Ordering::Relaxed);
        let converged = self.sessions.converged.load(Ordering::Relaxed);
        let partial = self.sessions.partial.load(Ordering::Relaxed);
        let escalated = self.sessions.escalated.load(Ordering::Relaxed);
        let timed_out = self.sessions.timed_out.load(Ordering::Relaxed);
        let failed = self.sessions.failed.load(Ordering::Relaxed);
        let total_duration_ms = self.sessions.total_duration_ms.load(Ordering::Relaxed);
        let total_rounds = self.sessions.total_rounds.load(Ordering::Relaxed);

        let completed = converged + partial + escalated + timed_out + failed;
        let avg_duration_ms = if completed > 0 {
            total_duration_ms / completed
        } else {
            0
        };
        let avg_rounds = if completed > 0 {
            total_rounds as f64 / completed as f64
        } else {
            0.0
        };

        let convergence_rate = if completed > 0 {
            converged as f64 / completed as f64
        } else {
            0.0
        };

        let timeout_rate = if completed > 0 {
            timed_out as f64 / completed as f64
        } else {
            0.0
        };

        MetricsSnapshot {
            sessions: SessionSnapshot {
                started,
                completed,
                converged,
                partial,
                escalated,
                timed_out,
                failed,
                avg_duration_ms,
                avg_rounds,
                convergence_rate,
                timeout_rate,
            },
            strategies: self.strategies.snapshot(),
            tiers: self.tiers.snapshot(),
        }
    }

    /// Reset all metrics.
    pub fn reset(&self) {
        self.sessions.started.store(0, Ordering::Relaxed);
        self.sessions.converged.store(0, Ordering::Relaxed);
        self.sessions.partial.store(0, Ordering::Relaxed);
        self.sessions.escalated.store(0, Ordering::Relaxed);
        self.sessions.timed_out.store(0, Ordering::Relaxed);
        self.sessions.failed.store(0, Ordering::Relaxed);
        self.sessions.total_duration_ms.store(0, Ordering::Relaxed);
        self.sessions.total_rounds.store(0, Ordering::Relaxed);
        self.sessions.participant_distribution.write().clear();
        self.sessions.tier_distribution.write().clear();

        self.strategies.direct.store(0, Ordering::Relaxed);
        self.strategies.flat.store(0, Ordering::Relaxed);
        self.strategies.hierarchical.store(0, Ordering::Relaxed);

        self.tiers.execution_counts.write().clear();
        self.tiers.unit_counts.write().clear();
        self.tiers.converged_units.write().clear();
        self.tiers.timed_out_units.write().clear();
    }
}

#[derive(Debug, Default)]
struct SessionMetrics {
    started: AtomicU64,
    converged: AtomicU64,
    partial: AtomicU64,
    escalated: AtomicU64,
    timed_out: AtomicU64,
    failed: AtomicU64,
    total_duration_ms: AtomicU64,
    total_rounds: AtomicU64,
    participant_distribution: RwLock<HashMap<usize, u64>>,
    tier_distribution: RwLock<HashMap<usize, u64>>,
}

#[derive(Debug, Default)]
struct TierMetrics {
    execution_counts: RwLock<HashMap<TierLevel, u64>>,
    unit_counts: RwLock<HashMap<TierLevel, usize>>,
    converged_units: RwLock<HashMap<TierLevel, u64>>,
    timed_out_units: RwLock<HashMap<TierLevel, u64>>,
}

impl TierMetrics {
    fn snapshot(&self) -> TierSnapshot {
        let execution_counts = self.execution_counts.read().clone();
        let unit_counts = self.unit_counts.read().clone();
        let converged_units = self.converged_units.read().clone();
        let timed_out_units = self.timed_out_units.read().clone();

        let mut tier_stats = HashMap::new();
        for tier in [
            TierLevel::Module,
            TierLevel::Group,
            TierLevel::Domain,
            TierLevel::Workspace,
            TierLevel::CrossWorkspace,
        ] {
            let executions = *execution_counts.get(&tier).unwrap_or(&0);
            let units = *unit_counts.get(&tier).unwrap_or(&0);
            let converged = *converged_units.get(&tier).unwrap_or(&0);
            let timeouts = *timed_out_units.get(&tier).unwrap_or(&0);

            if executions > 0 || units > 0 {
                let convergence_rate = if units > 0 {
                    converged as f64 / units as f64
                } else {
                    0.0
                };
                let timeout_rate = if units > 0 {
                    timeouts as f64 / units as f64
                } else {
                    0.0
                };

                tier_stats.insert(
                    tier,
                    TierStat {
                        executions,
                        units,
                        converged,
                        timeouts,
                        convergence_rate,
                        timeout_rate,
                    },
                );
            }
        }

        TierSnapshot { tiers: tier_stats }
    }
}

#[derive(Debug, Default)]
struct StrategyMetrics {
    direct: AtomicU64,
    flat: AtomicU64,
    hierarchical: AtomicU64,
}

impl StrategyMetrics {
    fn record_selection(&self, strategy: &str) {
        match strategy {
            "direct" => self.direct.fetch_add(1, Ordering::Relaxed),
            "flat" => self.flat.fetch_add(1, Ordering::Relaxed),
            "hierarchical" => self.hierarchical.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }

    fn snapshot(&self) -> StrategySnapshot {
        let direct = self.direct.load(Ordering::Relaxed);
        let flat = self.flat.load(Ordering::Relaxed);
        let hierarchical = self.hierarchical.load(Ordering::Relaxed);
        let total = direct + flat + hierarchical;

        StrategySnapshot {
            direct,
            flat,
            hierarchical,
            direct_ratio: if total > 0 {
                direct as f64 / total as f64
            } else {
                0.0
            },
            flat_ratio: if total > 0 {
                flat as f64 / total as f64
            } else {
                0.0
            },
            hierarchical_ratio: if total > 0 {
                hierarchical as f64 / total as f64
            } else {
                0.0
            },
        }
    }
}

/// Snapshot of all consensus metrics.
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub sessions: SessionSnapshot,
    pub strategies: StrategySnapshot,
    pub tiers: TierSnapshot,
}

impl MetricsSnapshot {
    pub fn summary(&self) -> String {
        format!(
            "Sessions: {}/{} converged ({:.1}%), Strategies: direct={} flat={} hier={}, Avg rounds: {:.1}",
            self.sessions.converged,
            self.sessions.completed,
            self.sessions.convergence_rate * 100.0,
            self.strategies.direct,
            self.strategies.flat,
            self.strategies.hierarchical,
            self.sessions.avg_rounds,
        )
    }
}

#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub started: u64,
    pub completed: u64,
    pub converged: u64,
    pub partial: u64,
    pub escalated: u64,
    pub timed_out: u64,
    pub failed: u64,
    pub avg_duration_ms: u64,
    pub avg_rounds: f64,
    pub convergence_rate: f64,
    pub timeout_rate: f64,
}

#[derive(Debug, Clone)]
pub struct StrategySnapshot {
    pub direct: u64,
    pub flat: u64,
    pub hierarchical: u64,
    pub direct_ratio: f64,
    pub flat_ratio: f64,
    pub hierarchical_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct TierSnapshot {
    pub tiers: HashMap<TierLevel, TierStat>,
}

#[derive(Debug, Clone)]
pub struct TierStat {
    pub executions: u64,
    pub units: usize,
    pub converged: u64,
    pub timeouts: u64,
    pub convergence_rate: f64,
    pub timeout_rate: f64,
}

/// Observer trait for metrics collection hooks.
pub trait MetricsObserver: Send + Sync {
    fn on_session_started(&self, _strategy: &str, _participants: usize, _tiers: usize) {}
    fn on_session_completed(
        &self,
        _outcome: ConsensusOutcome,
        _duration: Duration,
        _rounds: usize,
    ) {
    }
    fn on_tier_started(&self, _tier: TierLevel, _units: usize) {}
    fn on_tier_completed(&self, _tier: TierLevel, _converged: bool, _timed_out: bool) {}
}

impl MetricsObserver for ConsensusMetrics {
    fn on_session_started(&self, strategy: &str, participants: usize, tiers: usize) {
        self.session_started(strategy, participants, tiers);
    }

    fn on_session_completed(&self, outcome: ConsensusOutcome, duration: Duration, rounds: usize) {
        self.session_completed(outcome, duration, rounds);
    }

    fn on_tier_started(&self, tier: TierLevel, units: usize) {
        self.tier_started(tier, units);
    }

    fn on_tier_completed(&self, tier: TierLevel, converged: bool, timed_out: bool) {
        self.tier_unit_completed(tier, converged, timed_out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_metrics() {
        let metrics = ConsensusMetrics::new();

        metrics.session_started("direct", 1, 1);
        metrics.session_completed(ConsensusOutcome::Converged, Duration::from_millis(100), 1);

        metrics.session_started("flat", 5, 1);
        metrics.session_completed(ConsensusOutcome::Converged, Duration::from_millis(200), 2);

        metrics.session_started("hierarchical", 10, 3);
        metrics.session_completed(ConsensusOutcome::Failed, Duration::from_millis(300), 5);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.sessions.started, 3);
        assert_eq!(snapshot.sessions.completed, 3);
        assert_eq!(snapshot.sessions.converged, 2);
        assert_eq!(snapshot.sessions.failed, 1);
        assert!((snapshot.sessions.convergence_rate - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_strategy_metrics() {
        let metrics = ConsensusMetrics::new();

        metrics.session_started("direct", 1, 1);
        metrics.session_started("direct", 1, 1);
        metrics.session_started("flat", 3, 1);
        metrics.session_started("hierarchical", 10, 3);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.strategies.direct, 2);
        assert_eq!(snapshot.strategies.flat, 1);
        assert_eq!(snapshot.strategies.hierarchical, 1);
        assert_eq!(snapshot.strategies.direct_ratio, 0.5);
    }

    #[test]
    fn test_tier_metrics() {
        let metrics = ConsensusMetrics::new();

        metrics.tier_started(TierLevel::Module, 3);
        metrics.tier_unit_completed(TierLevel::Module, true, false);
        metrics.tier_unit_completed(TierLevel::Module, true, false);
        metrics.tier_unit_completed(TierLevel::Module, false, true);

        let snapshot = metrics.snapshot();
        let module_stats = snapshot.tiers.tiers.get(&TierLevel::Module).unwrap();
        assert_eq!(module_stats.executions, 1);
        assert_eq!(module_stats.units, 3);
        assert_eq!(module_stats.converged, 2);
        assert_eq!(module_stats.timeouts, 1);
    }

    #[test]
    fn test_metrics_reset() {
        let metrics = ConsensusMetrics::new();

        metrics.session_started("direct", 1, 1);
        metrics.session_completed(ConsensusOutcome::Converged, Duration::from_millis(100), 1);

        let before = metrics.snapshot();
        assert_eq!(before.sessions.completed, 1);

        metrics.reset();

        let after = metrics.snapshot();
        assert_eq!(after.sessions.completed, 0);
        assert_eq!(after.strategies.direct, 0);
    }

    #[test]
    fn test_metrics_snapshot_summary() {
        let metrics = ConsensusMetrics::new();

        metrics.session_started("flat", 5, 1);
        metrics.session_completed(ConsensusOutcome::Converged, Duration::from_millis(100), 3);

        let snapshot = metrics.snapshot();
        let summary = snapshot.summary();

        assert!(summary.contains("1/1 converged"));
        assert!(summary.contains("100.0%"));
        assert!(summary.contains("flat=1"));
    }

    #[test]
    fn test_observer_trait() {
        let metrics = ConsensusMetrics::new();
        let observer: &dyn MetricsObserver = &metrics;

        observer.on_session_started("direct", 1, 1);
        observer.on_session_completed(ConsensusOutcome::Converged, Duration::from_millis(50), 1);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.sessions.converged, 1);
    }
}
