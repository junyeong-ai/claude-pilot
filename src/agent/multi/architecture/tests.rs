use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use modmap::{DetectedLanguage, Module, ModuleDependency, ModuleMetrics, WorkspaceType};

use super::*;
use crate::domain::Severity;

fn create_test_modules() -> (Vec<Module>, Vec<DetectedLanguage>, WorkspaceType) {
    let modules = vec![
        Module {
            id: "auth".into(),
            name: "auth".into(),
            paths: vec!["src/auth/".into()],
            key_files: vec![],
            dependencies: vec![ModuleDependency::new("db")],
            dependents: vec![],
            responsibility: "Authentication".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::default(),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        },
        Module {
            id: "db".into(),
            name: "db".into(),
            paths: vec!["src/db/".into()],
            key_files: vec![],
            dependencies: vec![],
            dependents: vec![],
            responsibility: "Database".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::default(),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        },
        Module {
            id: "api".into(),
            name: "api".into(),
            paths: vec!["src/api/".into()],
            key_files: vec![],
            dependencies: vec![ModuleDependency::new("auth"), ModuleDependency::new("db")],
            dependents: vec![],
            responsibility: "API Layer".into(),
            primary_language: "Rust".into(),
            metrics: ModuleMetrics::default(),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        },
    ];
    let languages =
        vec![DetectedLanguage::new("Rust").with_marker_files(vec!["Cargo.toml".into()])];
    (modules, languages, WorkspaceType::SinglePackage)
}

fn create_test_agent() -> BoundaryEnforcementAgent {
    let (modules, languages, ws_type) = create_test_modules();
    let task_agent = Arc::new(crate::agent::TaskAgent::new(Default::default()));
    BoundaryEnforcementAgent::from_modules(modules, languages, ws_type, task_agent)
}

#[test]
fn test_architecture_agent_role() {
    let task_agent = Arc::new(crate::agent::TaskAgent::new(Default::default()));
    let agent = BoundaryEnforcementAgent::from_modules(
        vec![],
        vec![],
        WorkspaceType::SinglePackage,
        task_agent,
    );
    assert_eq!(agent.role(), &AgentRole::architect());
}

#[test]
fn test_validate_changes_no_violations() {
    let agent = create_test_agent();

    let changes = vec![ArchitecturalFileChange {
        path: PathBuf::from("src/auth/login.rs"),
        change_type: ChangeType::Modified,
        module: Some("auth".into()),
    }];

    let validation = agent.validate_changes(&changes);
    assert!(validation.passed);
    assert!(validation.violations.is_empty());
}

#[test]
fn test_validate_changes_boundary_violation() {
    let agent = create_test_agent();

    let changes = vec![ArchitecturalFileChange {
        path: PathBuf::from("src/db/pool.rs"),
        change_type: ChangeType::Modified,
        module: Some("auth".into()),
    }];

    let validation = agent.validate_changes(&changes);
    assert!(!validation.passed);
    assert!(!validation.violations.is_empty());
    assert_eq!(
        validation.violations[0].violation_type,
        ViolationType::BoundaryViolation
    );
}

#[test]
fn test_validate_changes_unauthorized_dependency() {
    let agent = create_test_agent();

    let changes = vec![ArchitecturalFileChange {
        path: PathBuf::from("src/api/routes.rs"),
        change_type: ChangeType::Modified,
        module: Some("db".into()),
    }];

    let validation = agent.validate_changes(&changes);
    assert!(!validation.passed);

    let unauthorized: Vec<_> = validation
        .violations
        .iter()
        .filter(|v| v.violation_type == ViolationType::UnauthorizedDependency)
        .collect();
    assert!(!unauthorized.is_empty());
}

#[test]
fn test_cross_module_detection() {
    let agent = create_test_agent();

    let changes = vec![
        ArchitecturalFileChange {
            path: PathBuf::from("src/auth/login.rs"),
            change_type: ChangeType::Modified,
            module: Some("auth".into()),
        },
        ArchitecturalFileChange {
            path: PathBuf::from("src/db/pool.rs"),
            change_type: ChangeType::Modified,
            module: Some("db".into()),
        },
    ];

    let validation = agent.validate_changes(&changes);
    assert!(validation.warnings.iter().any(|w| w.contains("span")));
}

#[test]
fn test_parse_violations_from_output() {
    let output = r#"
Found issues:
VIOLATION: type="boundary_violation" | severity="error" | file="src/auth/login.rs" | description="Auth modified db file" | fix="Move to db module"
VIOLATION: type="convention_violation" | severity="warning" | file="src/api/routes.rs" | description="Missing error handling"
"#;

    let violations = BoundaryEnforcementAgent::parse_violations_from_output(output);
    assert_eq!(violations.len(), 2);
    assert_eq!(
        violations[0].violation_type,
        ViolationType::BoundaryViolation
    );
    assert_eq!(violations[0].severity, Severity::Error);
    assert_eq!(violations[1].severity, Severity::Warning);
}

#[test]
fn test_system_prompt_contains_key_sections() {
    let agent = create_test_agent();
    let prompt = agent.system_prompt();

    assert!(prompt.contains("Boundary Enforcement"));
    assert!(prompt.contains("Dependency Direction"));
    assert!(prompt.contains("VIOLATION:"));
    assert!(prompt.contains("auth"));
    assert!(prompt.contains("db"));
    assert!(prompt.contains("api"));
}

#[test]
fn test_is_file_in_boundary() {
    let agent = create_test_agent();
    let auth_boundary = &agent.module_boundaries["auth"];

    assert!(agent.is_file_in_boundary(Path::new("src/auth/login.rs"), auth_boundary));
    assert!(agent.is_file_in_boundary(Path::new("src/auth/session/token.rs"), auth_boundary));
    assert!(!agent.is_file_in_boundary(Path::new("src/db/pool.rs"), auth_boundary));
}

#[test]
fn test_is_file_in_boundary_empty_files() {
    let agent = create_test_agent();

    let empty_boundary = ModuleBoundary {
        files: HashSet::new(),
        allowed_dependencies: HashSet::new(),
    };

    assert!(!agent.is_file_in_boundary(Path::new("src/auth/login.rs"), &empty_boundary));
    assert!(!agent.is_file_in_boundary(Path::new("src/db/pool.rs"), &empty_boundary));
    assert!(!agent.is_file_in_boundary(Path::new("any/random/path.rs"), &empty_boundary));
}

#[test]
fn test_detect_circular_dependency_no_cycle() {
    let agent = create_test_agent();

    let changes = vec![ArchitecturalFileChange {
        path: PathBuf::from("src/auth/login.rs"),
        change_type: ChangeType::Modified,
        module: Some("auth".into()),
    }];

    let cycle = agent.detect_circular_dependencies(&changes);
    assert!(
        cycle.is_none(),
        "Should not detect cycle in valid dependency graph"
    );
}

#[test]
fn test_detect_circular_dependency_with_inferred_cycle() {
    let agent = create_test_agent();

    let changes = vec![ArchitecturalFileChange {
        path: PathBuf::from("src/auth/session.rs"),
        change_type: ChangeType::Modified,
        module: Some("db".into()),
    }];

    let cycle = agent.detect_circular_dependencies(&changes);
    assert!(
        cycle.is_some(),
        "Should detect cycle when db modifies auth (creating auth -> db -> auth)"
    );

    let violation = cycle.unwrap();
    assert_eq!(violation.violation_type, ViolationType::CircularDependency);
    assert_eq!(violation.severity, Severity::Critical);
    assert!(violation.description.contains("introduce"));
}

#[test]
fn test_has_preexisting_cycle() {
    let agent = create_test_agent();

    let mut graph: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (name, boundary) in &agent.module_boundaries {
        let deps: HashSet<&str> = boundary
            .allowed_dependencies
            .iter()
            .map(|s| s.as_str())
            .collect();
        graph.insert(name.as_str(), deps);
    }

    let path_exists = vec!["auth".to_string(), "db".to_string()];
    assert!(agent.has_preexisting_cycle(&graph, &path_exists));

    let path_not_exists = vec!["db".to_string(), "auth".to_string()];
    assert!(!agent.has_preexisting_cycle(&graph, &path_not_exists));
}
