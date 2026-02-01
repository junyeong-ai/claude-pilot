//! Progress tracking for convergence analysis.

use serde::{Deserialize, Serialize};

/// Snapshot of convergence state at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressSnapshot {
    pub round: u32,
    pub issue_count: usize,
    pub critical_count: usize,
    pub fixed_count: usize,
    pub new_count: usize,
}

impl ProgressSnapshot {
    pub fn new(round: u32, issue_count: usize, critical_count: usize) -> Self {
        Self {
            round,
            issue_count,
            critical_count,
            fixed_count: 0,
            new_count: 0,
        }
    }

    pub fn with_delta(mut self, fixed: usize, new: usize) -> Self {
        self.fixed_count = fixed;
        self.new_count = new;
        self
    }
}

/// Analysis of progress trend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProgressAnalysis {
    /// Issues are decreasing.
    Improving { rate: f32 },
    /// No significant change (1-2 rounds).
    Stable,
    /// No change for 3+ rounds.
    Stagnating { rounds: u32 },
    /// Issues are increasing.
    Degrading { new_issues: usize },
}

impl ProgressAnalysis {
    pub fn is_improving(&self) -> bool {
        matches!(self, Self::Improving { .. })
    }

    pub fn is_stagnating(&self) -> bool {
        matches!(self, Self::Stagnating { .. })
    }

    pub fn is_degrading(&self) -> bool {
        matches!(self, Self::Degrading { .. })
    }

    pub fn should_escalate(&self, stagnation_threshold: u32) -> bool {
        match self {
            Self::Stagnating { rounds } => *rounds >= stagnation_threshold,
            Self::Degrading { .. } => true,
            _ => false,
        }
    }
}

/// Tracks progress across convergence rounds.
#[derive(Debug, Clone)]
pub struct ProgressTracker {
    history: Vec<ProgressSnapshot>,
    stagnation_threshold: u32,
}

impl Default for ProgressTracker {
    fn default() -> Self {
        Self::new(3) // Sensible default: 3 rounds of stagnation before escalation
    }
}

impl ProgressTracker {
    pub fn new(stagnation_threshold: u32) -> Self {
        Self {
            history: Vec::new(),
            stagnation_threshold,
        }
    }

    pub fn record(&mut self, snapshot: ProgressSnapshot) {
        self.history.push(snapshot);
    }

    pub fn analyze(&self) -> ProgressAnalysis {
        if self.history.len() < 2 {
            return ProgressAnalysis::Stable;
        }

        let current = &self.history[self.history.len() - 1];
        let previous = &self.history[self.history.len() - 2];

        if current.issue_count < previous.issue_count {
            let rate = (previous.issue_count - current.issue_count) as f32
                / previous.issue_count.max(1) as f32;
            return ProgressAnalysis::Improving { rate };
        }

        if current.issue_count > previous.issue_count {
            return ProgressAnalysis::Degrading {
                new_issues: current.issue_count - previous.issue_count,
            };
        }

        // Same issue count - check for stagnation
        let stagnant_rounds = self.consecutive_stagnation();
        if stagnant_rounds >= self.stagnation_threshold {
            ProgressAnalysis::Stagnating {
                rounds: stagnant_rounds,
            }
        } else {
            ProgressAnalysis::Stable
        }
    }

    fn consecutive_stagnation(&self) -> u32 {
        if self.history.is_empty() {
            return 0;
        }

        let current_count = self.history.last().unwrap().issue_count;
        self.history
            .iter()
            .rev()
            .take_while(|s| s.issue_count == current_count)
            .count() as u32
    }

    pub fn should_escalate_level(&self) -> bool {
        self.analyze().should_escalate(self.stagnation_threshold)
    }

    pub fn best_round(&self) -> Option<u32> {
        self.history
            .iter()
            .min_by_key(|s| s.issue_count)
            .map(|s| s.round)
    }

    pub fn current_round(&self) -> u32 {
        self.history.last().map(|s| s.round).unwrap_or(0)
    }

    pub fn total_rounds(&self) -> usize {
        self.history.len()
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_improving_progress() {
        let mut tracker = ProgressTracker::new(3);
        tracker.record(ProgressSnapshot::new(1, 10, 2));
        tracker.record(ProgressSnapshot::new(2, 7, 1));

        assert!(tracker.analyze().is_improving());
    }

    #[test]
    fn test_stagnating_progress() {
        let mut tracker = ProgressTracker::new(3);
        tracker.record(ProgressSnapshot::new(1, 5, 1));
        tracker.record(ProgressSnapshot::new(2, 5, 1));
        tracker.record(ProgressSnapshot::new(3, 5, 1));
        tracker.record(ProgressSnapshot::new(4, 5, 1));

        assert!(tracker.analyze().is_stagnating());
        assert!(tracker.should_escalate_level());
    }

    #[test]
    fn test_degrading_progress() {
        let mut tracker = ProgressTracker::new(3);
        tracker.record(ProgressSnapshot::new(1, 5, 1));
        tracker.record(ProgressSnapshot::new(2, 8, 2));

        assert!(tracker.analyze().is_degrading());
        assert!(tracker.should_escalate_level());
    }

    #[test]
    fn test_best_round() {
        let mut tracker = ProgressTracker::new(3);
        tracker.record(ProgressSnapshot::new(1, 10, 2));
        tracker.record(ProgressSnapshot::new(2, 5, 1));
        tracker.record(ProgressSnapshot::new(3, 7, 1));

        assert_eq!(tracker.best_round(), Some(2));
    }
}
