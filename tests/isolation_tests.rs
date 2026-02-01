use claude_pilot::config::PilotConfig;
use claude_pilot::isolation::{IsolationPlanner, ScopeAnalysis};
use claude_pilot::mission::{IsolationMode, Mission, MissionFlags, RiskLevel};

/// ScopeAnalysis::analyze returns neutral defaults because keyword-based
/// estimation was unreliable. Actual scope is determined by EvidenceGatherer
/// with real file counts.
#[test]
fn test_scope_analysis_returns_neutral_defaults() {
    // All descriptions return the same neutral defaults
    let small = ScopeAnalysis::analyze("Fix typo in README");
    let large = ScopeAnalysis::analyze("Refactor the entire authentication system");
    let risky = ScopeAnalysis::analyze("Update database schema for users");

    // All return neutral defaults (3 files, Medium risk)
    assert_eq!(small.estimated_files, 3);
    assert_eq!(large.estimated_files, 3);
    assert_eq!(risky.estimated_files, 3);
    assert!(matches!(small.risk_level, RiskLevel::Medium));
    assert!(matches!(large.risk_level, RiskLevel::Medium));
    assert!(matches!(risky.risk_level, RiskLevel::Medium));
}

#[test]
fn test_isolation_planner_direct_flag() {
    let config = PilotConfig::default();
    let planner = IsolationPlanner::new(&config, vec![]);

    let mut mission = Mission::new("m-001", "Small fix");
    mission.flags = MissionFlags {
        direct: true,
        ..Default::default()
    };

    let isolation = planner.determine_isolation(&mission, 5);
    assert_eq!(isolation, IsolationMode::None);
}

#[test]
fn test_isolation_planner_isolated_flag() {
    let config = PilotConfig::default();
    let planner = IsolationPlanner::new(&config, vec![]);

    let mut mission = Mission::new("m-001", "Some task");
    mission.flags = MissionFlags {
        isolated: true,
        ..Default::default()
    };

    let isolation = planner.determine_isolation(&mission, 2);
    assert_eq!(isolation, IsolationMode::Worktree);
}

#[test]
fn test_isolation_planner_by_scope() {
    let config = PilotConfig::default();
    let planner = IsolationPlanner::new(&config, vec![]);

    let mission = Mission::new("m-001", "Task");

    assert_eq!(
        planner.determine_isolation(&mission, 1),
        IsolationMode::None
    );
    assert_eq!(
        planner.determine_isolation(&mission, 2),
        IsolationMode::None
    );
    assert_eq!(
        planner.determine_isolation(&mission, 5),
        IsolationMode::Branch
    );
    assert_eq!(
        planner.determine_isolation(&mission, 15),
        IsolationMode::Worktree
    );
}

#[test]
fn test_isolation_with_active_missions() {
    let config = PilotConfig::default();

    let mut active_mission = Mission::new("m-000", "Running task");
    active_mission.status = claude_pilot::MissionState::Running;

    let planner = IsolationPlanner::new(&config, vec![&active_mission]);
    let mission = Mission::new("m-001", "New task");

    let isolation = planner.determine_isolation(&mission, 1);
    assert_eq!(isolation, IsolationMode::Worktree);
}
