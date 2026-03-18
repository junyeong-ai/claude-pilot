use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::*;

#[test]
fn test_basic_ownership() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("src/main.rs");

    let result = manager.acquire(&file, "agent-1", None, AccessType::Exclusive, "test");
    assert!(matches!(result, AcquisitionResult::Granted { .. }));

    let result = manager.acquire(&file, "agent-2", None, AccessType::Exclusive, "test");
    assert!(matches!(result, AcquisitionResult::Queued { .. }));
}

#[test]
fn test_shared_read() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("src/lib.rs");

    let r1 = manager.acquire(&file, "agent-1", None, AccessType::SharedRead, "read");
    assert!(matches!(r1, AcquisitionResult::Granted { .. }));

    let r2 = manager.acquire(&file, "agent-2", None, AccessType::SharedRead, "read");
    assert!(matches!(r2, AcquisitionResult::Granted { .. }));
}

#[test]
fn test_module_boundary() {
    let mut module_files = HashMap::new();
    module_files.insert("auth".to_string(), vec![PathBuf::from("src/auth/")]);

    let manager = Arc::new(FileOwnershipManager::with_module_map(
        module_files,
        HashMap::new(),
    ));

    let file = PathBuf::from("src/auth/login.rs");
    let result = manager.acquire(&file, "agent", Some("api"), AccessType::Exclusive, "test");
    assert!(matches!(result, AcquisitionResult::Denied { .. }));

    let result = manager.acquire(&file, "agent", Some("auth"), AccessType::Exclusive, "test");
    assert!(matches!(result, AcquisitionResult::Granted { .. }));
}

#[test]
fn test_module_dependency() {
    let mut module_files = HashMap::new();
    module_files.insert("db".to_string(), vec![PathBuf::from("src/db/")]);

    let mut deps = HashMap::new();
    deps.insert("auth".to_string(), vec!["db".to_string()]);

    let manager = Arc::new(FileOwnershipManager::with_module_map(module_files, deps));

    let file = PathBuf::from("src/db/schema.rs");
    let result = manager.acquire(&file, "agent", Some("auth"), AccessType::Exclusive, "test");
    assert!(matches!(result, AcquisitionResult::Granted { .. }));
}

#[test]
fn test_lease_drop_releases() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("src/test.rs");

    {
        let result = manager.acquire(&file, "agent-1", None, AccessType::Exclusive, "test");
        assert!(matches!(result, AcquisitionResult::Granted { .. }));
        assert!(manager.current_owner(&file).is_some());
    }
}

#[test]
fn test_ordered_acquisition_prevents_deadlock() {
    let manager = Arc::new(FileOwnershipManager::new());

    let files = vec![
        PathBuf::from("c.rs"),
        PathBuf::from("a.rs"),
        PathBuf::from("b.rs"),
    ];

    let result =
        manager.acquire_ordered(&files, "agent-1", None, AccessType::Exclusive, "test");
    assert!(result.is_ok());
    let guard = result.unwrap();
    assert_eq!(guard.files().len(), 3);
}

#[test]
fn test_fair_queue_priority() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("contested.rs");

    manager.add_to_wait_queue(&file, "low-priority", None, 1);
    manager.add_to_wait_queue(&file, "high-priority", None, 10);
    manager.add_to_wait_queue(&file, "medium-priority", None, 5);

    assert!(manager.is_next_in_queue(&file, "high-priority"));
    assert_eq!(manager.queue_position(&file, "medium-priority"), Some(1));
    assert_eq!(manager.queue_position(&file, "low-priority"), Some(2));
}

#[test]
fn test_fair_queue_respects_order() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("fair.rs");

    let result = manager.acquire(&file, "owner", None, AccessType::Exclusive, "holding");
    assert!(matches!(result, AcquisitionResult::Granted { .. }));

    manager.add_to_wait_queue(&file, "waiter-1", None, 0);

    let result = manager.acquire_with_fair_queue(
        &file,
        "waiter-2",
        None,
        AccessType::Exclusive,
        "want file",
    );
    assert!(matches!(result, AcquisitionResult::Queued { .. }));

    assert!(manager.is_next_in_queue(&file, "waiter-1"));
}

#[test]
fn test_remove_from_queue() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("remove.rs");

    manager.add_to_wait_queue(&file, "agent-1", None, 0);
    manager.add_to_wait_queue(&file, "agent-2", None, 0);

    assert_eq!(manager.queue_position(&file, "agent-1"), Some(0));
    assert_eq!(manager.queue_position(&file, "agent-2"), Some(1));

    manager.remove_from_wait_queue(&file, "agent-1");

    assert!(manager.queue_position(&file, "agent-1").is_none());
    assert_eq!(manager.queue_position(&file, "agent-2"), Some(0));
}

#[test]
fn test_cross_module_conflict_detection() {
    let manager = Arc::new(FileOwnershipManager::new());
    let file = PathBuf::from("shared_config.rs");

    let result = manager.acquire(
        &file,
        "agent-1",
        Some("auth"),
        AccessType::Exclusive,
        "modify",
    );
    assert!(matches!(result, AcquisitionResult::Granted { .. }));

    manager.add_to_wait_queue(&file, "agent-2", Some("api"), 0);

    let conflicts = manager.detect_cross_module_conflict(&file, Some("database"));

    assert!(conflicts.is_some());
    let modules = conflicts.unwrap();
    assert_eq!(modules.len(), 3);
    assert!(modules.contains(&"auth".to_string()));
    assert!(modules.contains(&"api".to_string()));
    assert!(modules.contains(&"database".to_string()));
}

#[test]
fn test_add_modules_extends_manager() {
    let mut primary_files = HashMap::new();
    primary_files.insert("auth".to_string(), vec![PathBuf::from("src/auth/mod.rs")]);

    let mut primary_deps = HashMap::new();
    primary_deps.insert("auth".to_string(), vec!["db".to_string()]);

    let manager = Arc::new(FileOwnershipManager::with_module_map(
        primary_files,
        primary_deps,
    ));

    let mut additional_files = HashMap::new();
    additional_files.insert(
        "frontend::ui".to_string(),
        vec![PathBuf::from("frontend/src/ui/mod.rs")],
    );

    let mut additional_deps = HashMap::new();
    additional_deps.insert(
        "frontend::ui".to_string(),
        vec!["frontend::api".to_string()],
    );

    manager.add_modules(additional_files, additional_deps);

    let primary_owner = manager.get_default_owner(&PathBuf::from("src/auth/mod.rs"));
    assert_eq!(primary_owner, Some("auth".to_string()));

    let additional_owner = manager.get_default_owner(&PathBuf::from("frontend/src/ui/mod.rs"));
    assert_eq!(additional_owner, Some("frontend::ui".to_string()));
}

#[test]
fn test_add_modules_with_duplicate_overwrites() {
    let manager = Arc::new(FileOwnershipManager::new());

    let mut first = HashMap::new();
    first.insert("auth".to_string(), vec![PathBuf::from("src/auth.rs")]);
    manager.add_modules(first, HashMap::new());

    let mut second = HashMap::new();
    second.insert("auth".to_string(), vec![PathBuf::from("src/new_auth.rs")]);
    manager.add_modules(second, HashMap::new());

    let new_owner = manager.get_default_owner(&PathBuf::from("src/new_auth.rs"));
    assert_eq!(new_owner, Some("auth".to_string()));
}
