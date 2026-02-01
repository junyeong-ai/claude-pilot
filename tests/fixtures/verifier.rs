//! Event verification utilities for testing.

use claude_pilot::state::{DomainEvent, EventPayload, VoteDecision};

#[derive(Debug)]
pub enum EventPattern {
    AgentSpawned {
        role: String,
    },
    ConsensusRoundStarted {
        round: usize,
    },
    ConsensusVote {
        agent: String,
        decision: VoteDecision,
    },
    ConsensusRoundCompleted {
        round: usize,
    },
    ConsensusCompleted {
        rounds: usize,
    },
    TaskAssigned {
        agent: String,
    },
    TaskCompleted {
        success: bool,
    },
    VerificationRound {
        round: usize,
    },
}

impl EventPattern {
    pub fn matches(&self, payload: &EventPayload) -> bool {
        match self {
            EventPattern::AgentSpawned { role } => {
                matches!(payload, EventPayload::AgentSpawned { role: r, .. } if r == role)
            }
            EventPattern::ConsensusRoundStarted { round } => {
                matches!(payload, EventPayload::ConsensusRoundStarted { round: r, .. } if *r as usize == *round)
            }
            EventPattern::ConsensusVote { agent, decision } => {
                matches!(
                    payload,
                    EventPayload::ConsensusVoteReceived { agent_id, decision: d, .. }
                        if agent_id == agent && d == decision
                )
            }
            EventPattern::ConsensusRoundCompleted { round } => {
                matches!(payload, EventPayload::ConsensusRoundCompleted { round: r, .. } if *r as usize == *round)
            }
            EventPattern::ConsensusCompleted { rounds } => {
                matches!(payload, EventPayload::ConsensusCompleted { rounds: r, .. } if *r as usize == *rounds)
            }
            EventPattern::TaskAssigned { agent } => {
                matches!(payload, EventPayload::AgentTaskAssigned { agent_id, .. } if agent_id == agent)
            }
            EventPattern::TaskCompleted { success } => {
                matches!(payload, EventPayload::AgentTaskCompleted { success: s, .. } if s == success)
            }
            EventPattern::VerificationRound { round } => {
                matches!(payload, EventPayload::VerificationRound { round: r, .. } if *r as usize == *round)
            }
        }
    }
}

pub struct EventVerifier {
    events: Vec<DomainEvent>,
}

impl EventVerifier {
    pub fn new(events: Vec<DomainEvent>) -> Self {
        Self { events }
    }

    pub fn has_event<F>(&self, predicate: F) -> bool
    where
        F: Fn(&EventPayload) -> bool,
    {
        self.events.iter().any(|e| predicate(&e.payload))
    }

    pub fn assert_sequence(&self, patterns: &[EventPattern]) {
        let mut pattern_idx = 0;

        for event in &self.events {
            if pattern_idx >= patterns.len() {
                break;
            }

            if patterns[pattern_idx].matches(&event.payload) {
                pattern_idx += 1;
            }
        }

        if pattern_idx < patterns.len() {
            let missing: Vec<_> = patterns[pattern_idx..]
                .iter()
                .map(|p| format!("{:?}", p))
                .collect();
            panic!(
                "Event sequence incomplete. Missing patterns:\n  {}",
                missing.join("\n  ")
            );
        }
    }

    pub fn assert_agent_participated(&self, agent_id: &str) {
        let participated = self.has_event(|e| match e {
            EventPayload::AgentTaskAssigned { agent_id: a, .. } => a == agent_id,
            EventPayload::AgentTaskCompleted { agent_id: a, .. } => a == agent_id,
            EventPayload::AgentSpawned { agent_id: a, .. } => a == agent_id,
            _ => false,
        });

        assert!(
            participated,
            "Agent '{}' did not participate in any recorded events",
            agent_id
        );
    }

    pub fn assert_convergent_verification(&self, clean_rounds: usize) {
        let convergence_achieved = self.has_event(|e| {
            matches!(e, EventPayload::ConvergenceAchieved { clean_rounds: c, .. } if *c >= clean_rounds as u32)
        });

        if convergence_achieved {
            return;
        }

        let verification_events: Vec<_> = self
            .events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::VerificationRound { .. }))
            .collect();

        let mut consecutive_clean = 0;
        for event in &verification_events {
            if let EventPayload::VerificationRound { passed, .. } = &event.payload {
                if *passed {
                    consecutive_clean += 1;
                } else {
                    consecutive_clean = 0;
                }
            }
        }

        assert!(
            consecutive_clean >= clean_rounds,
            "Expected {} consecutive clean verification rounds, found {}",
            clean_rounds,
            consecutive_clean
        );
    }

    pub fn assert_escalation(
        &self,
        expected_level: claude_pilot::agent::multi::escalation::EscalationLevel,
    ) {
        let escalated = self.has_event(|e| {
            matches!(
                e,
                EventPayload::ConsensusEscalated { level, .. }
                    if format!("{:?}", level) == format!("{:?}", expected_level)
            )
        });

        assert!(
            escalated,
            "Expected escalation at level {:?} but none occurred",
            expected_level
        );
    }

    pub fn assert_no_escalation(&self) {
        let escalated = self.has_event(|e| matches!(e, EventPayload::ConsensusEscalated { .. }));

        assert!(!escalated, "Expected no escalation but one occurred");
    }

    pub fn assert_conflicts_resolved(&self) {
        let detected =
            self.has_event(|e| matches!(e, EventPayload::ConsensusConflictDetected { .. }));
        let resolved =
            self.has_event(|e| matches!(e, EventPayload::ConsensusConflictResolved { .. }));

        assert!(
            detected && resolved,
            "Expected conflicts to be detected and resolved. Detected: {}, Resolved: {}",
            detected,
            resolved
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(payload: EventPayload) -> DomainEvent {
        DomainEvent::new("test-stream", payload)
    }

    #[test]
    fn test_has_event() {
        let events = vec![
            make_event(EventPayload::AgentSpawned {
                agent_id: "coder-0".into(),
                role: "coder".into(),
                persona: None,
            }),
            make_event(EventPayload::AgentTaskAssigned {
                agent_id: "coder-0".into(),
                task_id: "task-1".into(),
                role: "coder".into(),
            }),
        ];

        let verifier = EventVerifier::new(events);

        assert!(verifier.has_event(|e| matches!(e, EventPayload::AgentSpawned { .. })));
        assert!(!verifier.has_event(|e| matches!(e, EventPayload::ConsensusEscalated { .. })));
    }

    #[test]
    fn test_assert_sequence() {
        let events = vec![
            make_event(EventPayload::AgentSpawned {
                agent_id: "research-0".into(),
                role: "research".into(),
                persona: None,
            }),
            make_event(EventPayload::AgentTaskAssigned {
                agent_id: "coder-0".into(),
                task_id: "task-1".into(),
                role: "coder".into(),
            }),
            make_event(EventPayload::AgentTaskCompleted {
                agent_id: "coder-0".into(),
                task_id: "task-1".into(),
                success: true,
                duration_ms: 100,
            }),
        ];

        let verifier = EventVerifier::new(events);

        verifier.assert_sequence(&[
            EventPattern::AgentSpawned {
                role: "research".into(),
            },
            EventPattern::TaskAssigned {
                agent: "coder-0".into(),
            },
            EventPattern::TaskCompleted { success: true },
        ]);
    }

    #[test]
    fn test_assert_agent_participated() {
        let events = vec![make_event(EventPayload::AgentTaskAssigned {
            agent_id: "coder-0".into(),
            task_id: "task-1".into(),
            role: "coder".into(),
        })];

        let verifier = EventVerifier::new(events);
        verifier.assert_agent_participated("coder-0");
    }

    #[test]
    fn test_event_pattern_matching() {
        let payload = EventPayload::ConsensusRoundStarted {
            round: 1,
            proposal_hash: "abc".into(),
            participants: vec!["a".into()],
        };

        assert!(EventPattern::ConsensusRoundStarted { round: 1 }.matches(&payload));
        assert!(!EventPattern::ConsensusRoundStarted { round: 2 }.matches(&payload));
    }
}
