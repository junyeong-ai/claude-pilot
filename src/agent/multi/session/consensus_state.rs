//! Consensus state for multi-round planning discussions.
//!
//! Manages the state of multi-round consensus including proposals, votes,
//! and agreement tracking. This is separate from the NotificationLog as it
//! handles the structured state of consensus rounds.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::multi::AgentId;
use crate::state::VoteDecision;

/// Status of a consensus round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoundStatus {
    /// Round is open for proposals.
    Proposing,
    /// Round is open for voting.
    Voting,
    /// Round reached consensus.
    Agreed,
    /// Round failed to reach consensus.
    Failed,
    /// Round was cancelled.
    Cancelled,
}

impl RoundStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Agreed | Self::Failed | Self::Cancelled)
    }

    pub fn can_submit_proposal(&self) -> bool {
        matches!(self, Self::Proposing)
    }

    pub fn can_vote(&self) -> bool {
        matches!(self, Self::Voting)
    }
}

/// A proposal submitted during a consensus round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Unique proposal ID.
    pub id: String,
    /// Agent that submitted the proposal.
    pub author: AgentId,
    /// Proposal content.
    pub content: String,
    /// Hash of the proposal content.
    pub content_hash: String,
    /// Submission timestamp.
    #[serde(skip)]
    pub submitted_at: Option<Instant>,
    /// Supporting evidence/rationale.
    pub rationale: Option<String>,
    /// Affected files/modules.
    pub affected_scope: Vec<String>,
    /// Estimated complexity.
    pub complexity: Option<u32>,
}

impl Proposal {
    pub fn new(id: &str, author: AgentId, content: String) -> Self {
        let content_hash = simple_hash(&content);
        Self {
            id: id.to_string(),
            author,
            content,
            content_hash,
            submitted_at: Some(Instant::now()),
            rationale: None,
            affected_scope: Vec::new(),
            complexity: None,
        }
    }

    pub fn with_rationale(mut self, rationale: &str) -> Self {
        self.rationale = Some(rationale.to_string());
        self
    }

    pub fn with_scope(mut self, scope: Vec<String>) -> Self {
        self.affected_scope = scope;
        self
    }

    pub fn with_complexity(mut self, complexity: u32) -> Self {
        self.complexity = Some(complexity);
        self
    }

    pub fn age(&self) -> Option<Duration> {
        self.submitted_at.map(|t| t.elapsed())
    }
}

/// A vote cast on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    /// Agent that cast the vote.
    pub voter: AgentId,
    /// Proposal being voted on.
    pub proposal_id: String,
    /// Vote decision.
    pub decision: VoteDecision,
    /// Rationale for the vote.
    pub rationale: Option<String>,
    /// Suggested modifications (for conditional approvals).
    pub modifications: Option<String>,
    /// Vote weight (based on expertise).
    pub weight: f32,
    /// Timestamp.
    #[serde(skip)]
    pub cast_at: Option<Instant>,
}

impl Vote {
    pub fn new(voter: AgentId, proposal_id: &str, decision: VoteDecision) -> Self {
        Self {
            voter,
            proposal_id: proposal_id.to_string(),
            decision,
            rationale: None,
            modifications: None,
            weight: 1.0,
            cast_at: Some(Instant::now()),
        }
    }

    pub fn with_rationale(mut self, rationale: &str) -> Self {
        self.rationale = Some(rationale.to_string());
        self
    }

    pub fn with_modifications(mut self, modifications: &str) -> Self {
        self.modifications = Some(modifications.to_string());
        self
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn is_approval(&self) -> bool {
        matches!(self.decision, VoteDecision::Approve)
    }

    pub fn is_rejection(&self) -> bool {
        matches!(self.decision, VoteDecision::Reject)
    }
}

/// A single consensus round.
#[derive(Debug, Clone)]
pub struct ConsensusRound {
    /// Round number.
    pub round: u32,
    /// Round status.
    pub status: RoundStatus,
    /// Topic of discussion.
    pub topic: String,
    /// Participants expected to vote.
    pub participants: HashSet<AgentId>,
    /// Proposals submitted.
    pub proposals: Vec<Proposal>,
    /// Votes cast.
    pub votes: Vec<Vote>,
    /// Winning proposal (if agreed).
    pub winning_proposal: Option<String>,
    /// Start time.
    pub started_at: Instant,
    /// End time (if terminal).
    pub ended_at: Option<Instant>,
    /// Required approval threshold (0.0-1.0).
    pub approval_threshold: f32,
    /// Minimum votes required.
    pub min_votes: usize,
}

impl ConsensusRound {
    pub fn new(round: u32, topic: &str, participants: HashSet<AgentId>) -> Self {
        let min_votes = participants.len();
        Self {
            round,
            status: RoundStatus::Proposing,
            topic: topic.to_string(),
            participants,
            proposals: Vec::new(),
            votes: Vec::new(),
            winning_proposal: None,
            started_at: Instant::now(),
            ended_at: None,
            approval_threshold: 0.67,
            min_votes,
        }
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.approval_threshold = threshold.clamp(0.5, 1.0);
        self
    }

    pub fn with_min_votes(mut self, min: usize) -> Self {
        self.min_votes = min;
        self
    }

    /// Submit a proposal.
    pub fn submit_proposal(&mut self, proposal: Proposal) -> Result<(), String> {
        if !self.status.can_submit_proposal() {
            return Err(format!(
                "Cannot submit proposals in {:?} status",
                self.status
            ));
        }
        if !self.participants.contains(&proposal.author) {
            return Err(format!(
                "Agent {} is not a participant",
                proposal.author.as_str()
            ));
        }
        self.proposals.push(proposal);
        Ok(())
    }

    /// Transition to voting phase.
    pub fn start_voting(&mut self) -> Result<(), String> {
        if self.status != RoundStatus::Proposing {
            return Err("Can only transition to voting from proposing phase".to_string());
        }
        if self.proposals.is_empty() {
            return Err("Cannot start voting with no proposals".to_string());
        }
        self.status = RoundStatus::Voting;
        Ok(())
    }

    /// Cast a vote.
    pub fn cast_vote(&mut self, vote: Vote) -> Result<(), String> {
        if !self.status.can_vote() {
            return Err(format!("Cannot vote in {:?} status", self.status));
        }
        if !self.participants.contains(&vote.voter) {
            return Err(format!(
                "Agent {} is not a participant",
                vote.voter.as_str()
            ));
        }
        // Check if already voted on this proposal
        if self
            .votes
            .iter()
            .any(|v| v.voter == vote.voter && v.proposal_id == vote.proposal_id)
        {
            return Err(format!(
                "Agent {} already voted on proposal {}",
                vote.voter.as_str(),
                vote.proposal_id
            ));
        }
        self.votes.push(vote);
        Ok(())
    }

    /// Tally votes and determine if consensus is reached.
    pub fn tally(&mut self) -> TallyResult {
        let mut result = TallyResult {
            total_participants: self.participants.len(),
            total_votes: self.votes.len(),
            ..Default::default()
        };

        if self.proposals.is_empty() {
            result.status = TallyStatus::NoProposals;
            return result;
        }

        if result.total_votes < self.min_votes {
            result.status = TallyStatus::InsufficientVotes;
            return result;
        }

        // Count votes per proposal
        let mut proposal_scores: HashMap<&str, ProposalScore> = HashMap::new();

        for proposal in &self.proposals {
            proposal_scores.insert(&proposal.id, ProposalScore::default());
        }

        for vote in &self.votes {
            if let Some(score) = proposal_scores.get_mut(vote.proposal_id.as_str()) {
                match vote.decision {
                    VoteDecision::Approve => {
                        score.approvals += 1;
                        score.weighted_approvals += vote.weight;
                    }
                    VoteDecision::ApproveWithChanges => {
                        // Counted as approval with slightly reduced weight
                        score.approvals += 1;
                        score.weighted_approvals += vote.weight * 0.8;
                    }
                    VoteDecision::Reject => {
                        score.rejections += 1;
                        score.weighted_rejections += vote.weight;
                    }
                    VoteDecision::Abstain => {
                        score.abstentions += 1;
                    }
                }
            }
        }

        // Find winning proposal
        let mut best_proposal: Option<&str> = None;
        let mut best_ratio = 0.0;

        for (proposal_id, score) in &proposal_scores {
            let total_weight = score.weighted_approvals + score.weighted_rejections;
            if total_weight > 0.0 {
                let ratio = score.weighted_approvals / total_weight;
                if ratio >= self.approval_threshold && ratio > best_ratio {
                    best_ratio = ratio;
                    best_proposal = Some(proposal_id);
                }
            }
        }

        if let Some(winner) = best_proposal {
            result.status = TallyStatus::ConsensusReached;
            result.winning_proposal = Some(winner.to_string());
            result.approval_ratio = Some(best_ratio);
            self.winning_proposal = Some(winner.to_string());
            self.status = RoundStatus::Agreed;
            self.ended_at = Some(Instant::now());
        } else {
            result.status = TallyStatus::NoConsensus;
            result.approval_ratio = Some(best_ratio);
        }

        result.proposal_scores = proposal_scores
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();

        result
    }

    /// Mark round as failed.
    pub fn fail(&mut self, reason: &str) {
        self.status = RoundStatus::Failed;
        self.ended_at = Some(Instant::now());
        // Store failure reason in winning_proposal field (reused)
        self.winning_proposal = Some(format!("FAILED: {}", reason));
    }

    /// Mark round as cancelled.
    pub fn cancel(&mut self) {
        self.status = RoundStatus::Cancelled;
        self.ended_at = Some(Instant::now());
    }

    /// Get duration.
    pub fn duration(&self) -> Duration {
        self.ended_at
            .map(|end| end.duration_since(self.started_at))
            .unwrap_or_else(|| self.started_at.elapsed())
    }

    /// Get voters who haven't voted yet.
    pub fn pending_voters(&self) -> Vec<&AgentId> {
        let voted: HashSet<_> = self.votes.iter().map(|v| &v.voter).collect();
        self.participants
            .iter()
            .filter(|p| !voted.contains(p))
            .collect()
    }

    /// Check if all participants have voted.
    pub fn all_voted(&self) -> bool {
        self.pending_voters().is_empty()
    }
}

/// Score for a proposal.
#[derive(Debug, Clone, Default)]
pub struct ProposalScore {
    pub approvals: u32,
    pub rejections: u32,
    pub abstentions: u32,
    pub weighted_approvals: f32,
    pub weighted_rejections: f32,
}

/// Result of vote tally.
#[derive(Debug, Clone, Default)]
pub struct TallyResult {
    pub status: TallyStatus,
    pub total_participants: usize,
    pub total_votes: usize,
    pub winning_proposal: Option<String>,
    pub approval_ratio: Option<f32>,
    pub proposal_scores: HashMap<String, ProposalScore>,
}

/// Status of vote tally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TallyStatus {
    #[default]
    InsufficientVotes,
    NoProposals,
    NoConsensus,
    ConsensusReached,
}

/// Manager for consensus state across multiple rounds.
#[derive(Debug, Default)]
pub struct ConsensusState {
    /// All rounds.
    rounds: Vec<ConsensusRound>,
    /// Current round index.
    current_round: Option<usize>,
    /// Maximum rounds allowed.
    max_rounds: u32,
    /// Default approval threshold.
    default_threshold: f32,
}

impl ConsensusState {
    pub fn new() -> Self {
        Self {
            rounds: Vec::new(),
            current_round: None,
            max_rounds: 5,
            default_threshold: 0.67,
        }
    }

    pub fn with_max_rounds(mut self, max: u32) -> Self {
        self.max_rounds = max.max(1);
        self
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.default_threshold = threshold.clamp(0.5, 1.0);
        self
    }

    /// Start a new consensus round.
    pub fn start_round(
        &mut self,
        topic: &str,
        participants: HashSet<AgentId>,
    ) -> Result<u32, String> {
        if let Some(idx) = self.current_round
            && !self.rounds[idx].status.is_terminal()
        {
            return Err("Current round is still active".to_string());
        }

        let round_num = self.rounds.len() as u32 + 1;
        if round_num > self.max_rounds {
            return Err(format!("Maximum rounds ({}) exceeded", self.max_rounds));
        }

        let round = ConsensusRound::new(round_num, topic, participants)
            .with_threshold(self.default_threshold);

        self.rounds.push(round);
        self.current_round = Some(self.rounds.len() - 1);

        Ok(round_num)
    }

    /// Get current round.
    pub fn current(&self) -> Option<&ConsensusRound> {
        self.current_round.and_then(|idx| self.rounds.get(idx))
    }

    /// Get mutable current round.
    pub fn current_mut(&mut self) -> Option<&mut ConsensusRound> {
        self.current_round.and_then(|idx| self.rounds.get_mut(idx))
    }

    /// Get a specific round.
    pub fn round(&self, num: u32) -> Option<&ConsensusRound> {
        self.rounds.get(num.saturating_sub(1) as usize)
    }

    /// Get all rounds.
    pub fn all_rounds(&self) -> &[ConsensusRound] {
        &self.rounds
    }

    /// Get round count.
    pub fn round_count(&self) -> usize {
        self.rounds.len()
    }

    /// Check if any round reached consensus.
    pub fn has_agreement(&self) -> bool {
        self.rounds.iter().any(|r| r.status == RoundStatus::Agreed)
    }

    /// Get the agreed proposal from the latest agreed round.
    pub fn agreed_proposal(&self) -> Option<(&ConsensusRound, &Proposal)> {
        for round in self.rounds.iter().rev() {
            if round.status == RoundStatus::Agreed
                && let Some(winner_id) = &round.winning_proposal
                && let Some(proposal) = round.proposals.iter().find(|p| &p.id == winner_id)
            {
                return Some((round, proposal));
            }
        }
        None
    }

    /// Check if consensus process is complete (agreed or all rounds exhausted).
    pub fn is_complete(&self) -> bool {
        if self.has_agreement() {
            return true;
        }
        if self.rounds.len() as u32 >= self.max_rounds {
            return self
                .rounds
                .last()
                .map(|r| r.status.is_terminal())
                .unwrap_or(true);
        }
        false
    }

    /// Get summary of consensus state.
    pub fn summary(&self) -> ConsensusSummary {
        ConsensusSummary {
            total_rounds: self.rounds.len(),
            max_rounds: self.max_rounds,
            has_agreement: self.has_agreement(),
            current_status: self.current().map(|r| r.status),
            winning_proposal: self.agreed_proposal().map(|(_, p)| p.id.clone()),
        }
    }
}

/// Summary of consensus state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusSummary {
    pub total_rounds: usize,
    pub max_rounds: u32,
    pub has_agreement: bool,
    pub current_status: Option<RoundStatus>,
    pub winning_proposal: Option<String>,
}

/// Simple hash function for proposal content.
fn simple_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_participants() -> HashSet<AgentId> {
        ["agent-1", "agent-2", "agent-3"]
            .into_iter()
            .map(AgentId::new)
            .collect()
    }

    #[test]
    fn test_proposal_creation() {
        let proposal = Proposal::new(
            "p1",
            AgentId::new("agent-1"),
            "Implement feature X".to_string(),
        )
        .with_rationale("This improves performance")
        .with_scope(vec!["auth".to_string(), "api".to_string()]);

        assert_eq!(proposal.id, "p1");
        assert!(proposal.rationale.is_some());
        assert_eq!(proposal.affected_scope.len(), 2);
        assert!(!proposal.content_hash.is_empty());
    }

    #[test]
    fn test_round_lifecycle() {
        let mut round = ConsensusRound::new(1, "Feature implementation", create_participants());

        assert_eq!(round.status, RoundStatus::Proposing);

        // Submit proposal
        let proposal = Proposal::new("p1", AgentId::new("agent-1"), "Plan A".to_string());
        round.submit_proposal(proposal).unwrap();

        // Start voting
        round.start_voting().unwrap();
        assert_eq!(round.status, RoundStatus::Voting);

        // Cast votes
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-1"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-2"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-3"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();

        // Tally
        let result = round.tally();
        assert_eq!(result.status, TallyStatus::ConsensusReached);
        assert_eq!(result.winning_proposal, Some("p1".to_string()));
        assert_eq!(round.status, RoundStatus::Agreed);
    }

    #[test]
    fn test_no_consensus() {
        let mut round = ConsensusRound::new(1, "Controversial topic", create_participants());

        let p1 = Proposal::new("p1", AgentId::new("agent-1"), "Plan A".to_string());
        round.submit_proposal(p1).unwrap();

        round.start_voting().unwrap();

        // Split votes: 1 approve, 2 reject = 33% approval, below 67% threshold
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-1"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-2"),
                "p1",
                VoteDecision::Reject,
            ))
            .unwrap();
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-3"),
                "p1",
                VoteDecision::Reject,
            ))
            .unwrap();

        let result = round.tally();
        assert_eq!(result.status, TallyStatus::NoConsensus);
    }

    #[test]
    fn test_consensus_state_multi_round() {
        let mut state = ConsensusState::new().with_max_rounds(3);

        let participants = create_participants();

        // Round 1 - fails
        state.start_round("Topic", participants.clone()).unwrap();
        state.current_mut().unwrap().fail("No agreement");

        // Round 2 - succeeds
        state.start_round("Topic", participants.clone()).unwrap();
        let round = state.current_mut().unwrap();

        let proposal = Proposal::new("p1", AgentId::new("agent-1"), "Final plan".to_string());
        round.submit_proposal(proposal).unwrap();
        round.start_voting().unwrap();

        for agent in &participants {
            round
                .cast_vote(Vote::new(agent.clone(), "p1", VoteDecision::Approve))
                .unwrap();
        }

        round.tally();

        assert!(state.has_agreement());
        assert!(state.is_complete());
        assert_eq!(state.round_count(), 2);
    }

    #[test]
    fn test_weighted_voting() {
        let participants = create_participants();
        let mut round = ConsensusRound::new(1, "Topic", participants).with_threshold(0.6);

        let proposal = Proposal::new("p1", AgentId::new("agent-1"), "Plan".to_string());
        round.submit_proposal(proposal).unwrap();
        round.start_voting().unwrap();

        // Expert (weight 2.0) approves
        round
            .cast_vote(
                Vote::new(AgentId::new("agent-1"), "p1", VoteDecision::Approve).with_weight(2.0),
            )
            .unwrap();
        // Regular votes reject
        round
            .cast_vote(
                Vote::new(AgentId::new("agent-2"), "p1", VoteDecision::Reject).with_weight(1.0),
            )
            .unwrap();
        round
            .cast_vote(
                Vote::new(AgentId::new("agent-3"), "p1", VoteDecision::Reject).with_weight(1.0),
            )
            .unwrap();

        let result = round.tally();
        // 2.0 approve / (2.0 + 2.0) = 0.5 < 0.6 threshold
        assert_eq!(result.status, TallyStatus::NoConsensus);
    }

    #[test]
    fn test_pending_voters() {
        let participants = create_participants();
        let mut round = ConsensusRound::new(1, "Topic", participants);

        let proposal = Proposal::new("p1", AgentId::new("agent-1"), "Plan".to_string());
        round.submit_proposal(proposal).unwrap();
        round.start_voting().unwrap();

        assert_eq!(round.pending_voters().len(), 3);

        round
            .cast_vote(Vote::new(
                AgentId::new("agent-1"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();
        assert_eq!(round.pending_voters().len(), 2);

        round
            .cast_vote(Vote::new(
                AgentId::new("agent-2"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();
        round
            .cast_vote(Vote::new(
                AgentId::new("agent-3"),
                "p1",
                VoteDecision::Approve,
            ))
            .unwrap();

        assert!(round.all_voted());
    }
}
