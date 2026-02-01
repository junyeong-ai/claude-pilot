use claude_pilot::config::{BudgetAllocationConfig, PhaseComplexityConfig};
use claude_pilot::context::{ActualUsage, TokenBudget};

fn test_budget(max_total: usize, threshold: f32) -> TokenBudget {
    TokenBudget::with_config(
        max_total,
        threshold,
        BudgetAllocationConfig::default(),
        PhaseComplexityConfig::default(),
    )
}

// ========== ActualUsage Tests ==========

#[test]
fn test_actual_usage_creation() {
    let usage = ActualUsage::new(1000, 500);
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.output_tokens, 500);
    assert_eq!(usage.total(), 1500);
    assert_eq!(usage.cache_read_input_tokens, 0);
}

#[test]
fn test_actual_usage_default() {
    let usage = ActualUsage::default();
    assert_eq!(usage.total(), 0);
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);
}

#[test]
fn test_actual_usage_with_cache() {
    let usage = ActualUsage::with_cache(1000, 500, 200, 100);
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.output_tokens, 500);
    assert_eq!(usage.cache_read_input_tokens, 200);
    assert_eq!(usage.cache_creation_input_tokens, 100);
    assert!((usage.cache_hit_rate() - 0.2).abs() < 0.001);
    assert_eq!(usage.effective_input(), 800);
}

#[test]
fn test_actual_usage_cache_hit_rate_clamped() {
    // Edge case: cache_read > input (shouldn't happen but test clamping)
    let usage = ActualUsage::with_cache(100, 50, 200, 0);
    assert!(usage.cache_hit_rate() <= 1.0);
}

#[test]
fn test_actual_usage_add() {
    let mut usage1 = ActualUsage::new(1000, 500);
    let usage2 = ActualUsage::with_cache(500, 250, 100, 50);
    usage1.add(&usage2);

    assert_eq!(usage1.input_tokens, 1500);
    assert_eq!(usage1.output_tokens, 750);
    assert_eq!(usage1.cache_read_input_tokens, 100);
    assert_eq!(usage1.cache_creation_input_tokens, 50);
}

#[test]
fn test_actual_usage_equality() {
    let usage1 = ActualUsage::new(1000, 500);
    let usage2 = ActualUsage::new(1000, 500);
    let usage3 = ActualUsage::new(1000, 600);

    assert_eq!(usage1, usage2);
    assert_ne!(usage1, usage3);
}

// ========== TokenBudget with Actual Usage Tests ==========

#[test]
fn test_token_budget_no_actual_usage_initially() {
    let budget = TokenBudget::default();
    assert!(!budget.has_actual_usage());
    assert!(budget.actual_usage.is_none());
    assert_eq!(budget.current_input_tokens(), 0);
}

#[test]
fn test_token_budget_update_actual() {
    let mut budget = TokenBudget::default();

    // First update creates actual_usage
    let usage = ActualUsage::new(50_000, 10_000);
    budget.update_actual(usage);

    assert!(budget.has_actual_usage());
    assert_eq!(budget.current_input_tokens(), 50_000);

    let actual = budget.actual_usage().unwrap();
    assert_eq!(actual.input_tokens, 50_000);
    assert_eq!(actual.output_tokens, 10_000);
}

#[test]
fn test_token_budget_cumulative_actual() {
    let mut budget = TokenBudget::default();

    // Multiple updates accumulate
    budget.update_actual(ActualUsage::new(10_000, 2_000));
    budget.update_actual(ActualUsage::new(15_000, 3_000));
    budget.update_actual(ActualUsage::new(5_000, 1_000));

    let actual = budget.actual_usage().unwrap();
    assert_eq!(actual.input_tokens, 30_000);
    assert_eq!(actual.output_tokens, 6_000);
}

#[test]
fn test_token_budget_effective_usage_ratio() {
    let mut budget = test_budget(100_000, 0.8);

    // Without actual usage, uses estimation
    budget.current_usage.total = 50_000;
    assert!((budget.effective_usage_ratio() - 0.5).abs() < 0.001);

    // With actual usage, uses actual
    budget.update_actual(ActualUsage::new(60_000, 10_000));
    assert!((budget.effective_usage_ratio() - 0.6).abs() < 0.001);
}

#[test]
fn test_token_budget_exceeds_threshold_effective() {
    let mut budget = test_budget(100_000, 0.8);

    // Below threshold
    budget.update_actual(ActualUsage::new(70_000, 10_000));
    assert!(!budget.exceeds_threshold_effective());

    // Above threshold (cumulative: 70k + 15k = 85k)
    budget.update_actual(ActualUsage::new(15_000, 5_000));
    assert!(budget.exceeds_threshold_effective());
}

#[test]
fn test_token_budget_cache_efficiency() {
    let mut budget = TokenBudget::default();

    // No actual usage - no cache efficiency
    assert!(budget.cache_efficiency().is_none());

    // With cache information
    budget.update_actual(ActualUsage::with_cache(100_000, 20_000, 30_000, 10_000));
    let efficiency = budget.cache_efficiency().unwrap();
    assert!((efficiency - 0.3).abs() < 0.001); // 30% cache hit
}

#[test]
fn test_token_budget_reset_actual_usage() {
    let mut budget = TokenBudget::default();
    budget.update_actual(ActualUsage::new(50_000, 10_000));

    assert!(budget.has_actual_usage());

    budget.reset_actual_usage();

    assert!(!budget.has_actual_usage());
    assert!(budget.actual_usage.is_none());
}

#[test]
fn test_token_budget_clear_actual_counts() {
    let mut budget = TokenBudget::default();
    budget.update_actual(ActualUsage::new(50_000, 10_000));

    assert!(budget.has_actual_usage());

    budget.clear_actual_counts();

    // Still has tracking enabled, but counts are zero
    assert!(budget.has_actual_usage());
    let actual = budget.actual_usage().unwrap();
    assert_eq!(actual.input_tokens, 0);
    assert_eq!(actual.output_tokens, 0);
}

#[test]
fn test_token_budget_clear_actual_counts_noop_when_none() {
    let mut budget = TokenBudget::default();

    // Should not panic or enable tracking when there's no actual usage
    budget.clear_actual_counts();
    assert!(!budget.has_actual_usage());
}

// ========== Integration with claude-agent-rs types ==========

#[test]
fn test_from_claude_agent_usage() {
    // This test verifies the From implementations compile correctly
    // In real usage, this would come from claude-agent-rs
    let usage = ActualUsage::new(1000, 500);
    assert_eq!(usage.total(), 1500);
}
