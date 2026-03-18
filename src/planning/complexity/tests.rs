use super::*;
use super::tiers::{ConfidenceStats, FileScope};

#[test]
fn test_complexity_tier() {
    assert!(ComplexityTier::Trivial.is_trivial());
    assert!(!ComplexityTier::Trivial.needs_planning());

    assert!(ComplexityTier::Simple.is_simple());
    assert!(!ComplexityTier::Simple.needs_planning());

    assert!(!ComplexityTier::Complex.is_simple());
    assert!(ComplexityTier::Complex.needs_planning());
}

#[test]
fn test_gate_constructors() {
    let trivial = ComplexityGate::trivial("Fix typo");
    assert_eq!(trivial.tier, ComplexityTier::Trivial);
    assert!(trivial.is_simple);

    let simple = ComplexityGate::simple("Add field");
    assert_eq!(simple.tier, ComplexityTier::Simple);
    assert!(simple.is_simple);

    let complex = ComplexityGate::complex("New feature");
    assert_eq!(complex.tier, ComplexityTier::Complex);
    assert!(!complex.is_simple);
}

#[test]
fn test_complexity_tier_ordering() {
    assert!(ComplexityTier::Trivial < ComplexityTier::Simple);
    assert!(ComplexityTier::Simple < ComplexityTier::Complex);
}

#[test]
fn test_file_scope_small() {
    let paths: Vec<String> = (0..5).map(|i| format!("src/file{}.rs", i)).collect();
    let scope = FileScope::from_paths(&paths);

    assert_eq!(scope.total_count, 5);
    assert_eq!(scope.all_paths.len(), 5);
    assert!(scope.directory_distribution.is_empty());

    let formatted = scope.format_for_llm();
    assert!(formatted.contains("5 total"), "Got: {}", formatted);
    assert!(formatted.contains("src/file0.rs"));
}

#[test]
fn test_file_scope_large() {
    let mut paths: Vec<String> = Vec::new();
    for i in 0..15 {
        paths.push(format!("src/module_a/file{}.rs", i));
    }
    for i in 0..10 {
        paths.push(format!("tests/file{}.rs", i));
    }

    let scope = FileScope::from_paths(&paths);

    assert_eq!(scope.total_count, 25);
    assert!(scope.all_paths.is_empty());
    assert!(!scope.directory_distribution.is_empty());

    let formatted = scope.format_for_llm();
    assert!(formatted.contains("25 total"), "Got: {}", formatted);
    assert!(formatted.contains("distribution"), "Got: {}", formatted);
    assert!(formatted.contains("src/"));
    assert!(formatted.contains("tests/"));
}

#[test]
fn test_file_scope_threshold() {
    let paths: Vec<String> = (0..20).map(|i| format!("src/file{}.rs", i)).collect();
    let scope = FileScope::from_paths(&paths);

    assert_eq!(scope.total_count, 20);
    assert_eq!(scope.all_paths.len(), 20);
    assert!(scope.directory_distribution.is_empty());

    let paths: Vec<String> = (0..21).map(|i| format!("src/file{}.rs", i)).collect();
    let scope = FileScope::from_paths(&paths);

    assert_eq!(scope.total_count, 21);
    assert!(scope.all_paths.is_empty());
    assert!(!scope.directory_distribution.is_empty());
}

#[test]
fn test_file_scope_with_confidence() {
    let paths_with_conf: Vec<(String, f32)> = vec![
        ("src/main.rs".into(), 0.9),
        ("src/lib.rs".into(), 0.8),
        ("src/config.rs".into(), 0.3),
    ];
    let scope = FileScope::from_paths_with_confidence(&paths_with_conf);

    assert_eq!(scope.total_count, 3);
    assert_eq!(scope.confidence_stats.min, 0.3);
    assert_eq!(scope.confidence_stats.max, 0.9);
    assert_eq!(scope.confidence_stats.high_confidence_count, 2);
    assert_eq!(scope.confidence_stats.low_confidence_count, 1);

    let formatted = scope.format_for_llm();
    assert!(
        formatted.contains("90%"),
        "Should show confidence, got: {}",
        formatted
    );
}

#[test]
fn test_file_scope_empty() {
    let scope = FileScope::from_paths(&[]);
    assert_eq!(scope.total_count, 0);
    assert!(scope.format_for_llm().contains("No relevant files"));
}

#[test]
fn test_confidence_stats() {
    let values = vec![0.1, 0.5, 0.9];
    let stats = ConfidenceStats::from_values(&values);

    assert_eq!(stats.min, 0.1);
    assert_eq!(stats.max, 0.9);
    assert_eq!(stats.median, 0.5);
    assert_eq!(stats.high_confidence_count, 1);
    assert_eq!(stats.low_confidence_count, 1);
}
