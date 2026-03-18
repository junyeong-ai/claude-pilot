use super::types::AdaptiveConsensusState;
use super::super::shared::ConsensusSessionStatus;

#[test]
fn test_consensus_state_new() {
    let state = AdaptiveConsensusState::new("session-1".to_string());
    assert_eq!(state.session_id, "session-1");
    assert_eq!(state.current_round, 0);
    assert!(state.current_tier.is_none());
    assert!(state.completed_tiers.is_empty());
    assert!(state.tier_results.is_empty());
    assert_eq!(state.status, ConsensusSessionStatus::Running);
}

#[test]
fn test_consensus_state_hash_deterministic() {
    let state1 = AdaptiveConsensusState::new("session-1".to_string());
    let state2 = AdaptiveConsensusState::new("session-2".to_string());

    assert_ne!(state1.content_hash(), state2.content_hash());

    let state1_clone = AdaptiveConsensusState::new("session-1".to_string());
    assert_eq!(state1.content_hash(), state1_clone.content_hash());
}

#[test]
fn test_consensus_state_hash_includes_round() {
    let mut state = AdaptiveConsensusState::new("session-1".to_string());
    let hash_before = state.content_hash();

    state.current_round = 5;
    let hash_after = state.content_hash();

    assert_ne!(hash_before, hash_after);
}
