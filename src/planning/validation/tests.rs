use std::path::{Path, PathBuf};

use super::rules::is_safe_relative_path;

fn normalize_path(path: &Path) -> Option<PathBuf> {
    let mut components = Vec::new();
    let mut leading_parents = 0usize;
    let mut saw_normal = false;

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if saw_normal {
                    if components.is_empty() {
                        return None;
                    }
                    components.pop();
                } else {
                    leading_parents += 1;
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(c) => {
                if leading_parents > 0 {
                    return None;
                }
                saw_normal = true;
                components.push(c);
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return None;
            }
        }
    }

    if components.is_empty() {
        return None;
    }

    Some(components.iter().collect())
}

fn is_new_path_safe(path: &Path, working_dir: &Path) -> bool {
    let Some(normalized) = normalize_path(path) else {
        return false;
    };

    let full_path = working_dir.join(&normalized);
    let Ok(canonical_working) = working_dir.canonicalize() else {
        return false;
    };

    let mut current = full_path.as_path();
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(meta) => {
                if meta.is_symlink() {
                    match current.canonicalize() {
                        Ok(resolved) if resolved.starts_with(&canonical_working) => {
                            if current != full_path && !resolved.is_dir() {
                                return false;
                            }
                            return true;
                        }
                        _ => return false,
                    }
                }
                if meta.is_file() && current != full_path {
                    return false;
                }
                match current.canonicalize() {
                    Ok(resolved) => return resolved.starts_with(&canonical_working),
                    Err(_) => return false,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => current = parent,
                _ => return false,
            },
            Err(_) => return false,
        }
    }
}

#[test]
fn test_normalize_path() {
    assert_eq!(
        normalize_path(Path::new("src/main.rs")),
        Some(PathBuf::from("src/main.rs"))
    );
    assert_eq!(
        normalize_path(Path::new("./src/lib.rs")),
        Some(PathBuf::from("src/lib.rs"))
    );
    assert_eq!(
        normalize_path(Path::new("src/./foo/./bar.rs")),
        Some(PathBuf::from("src/foo/bar.rs"))
    );
    assert_eq!(
        normalize_path(Path::new("src/foo/../bar.rs")),
        Some(PathBuf::from("src/bar.rs"))
    );
    assert_eq!(normalize_path(Path::new("../outside.rs")), None);
    assert_eq!(normalize_path(Path::new("src/../../outside.rs")), None);
    assert_eq!(normalize_path(Path::new("/etc/passwd")), None);
    assert_eq!(normalize_path(Path::new(".")), None);
    assert_eq!(normalize_path(Path::new("")), None);
}

#[test]
fn test_is_safe_relative_path_accepts_valid() {
    assert!(is_safe_relative_path(Path::new("src/main.rs")));
    assert!(is_safe_relative_path(Path::new("foo/bar/baz.txt")));
    assert!(is_safe_relative_path(Path::new("file.txt")));
    assert!(is_safe_relative_path(Path::new("./src/lib.rs")));
    assert!(is_safe_relative_path(Path::new("src/foo/../bar.rs")));
}

#[test]
fn test_is_safe_relative_path_rejects_traversal() {
    assert!(!is_safe_relative_path(Path::new("../outside.rs")));
    assert!(!is_safe_relative_path(Path::new("src/../../outside.rs")));
    assert!(!is_safe_relative_path(Path::new("..")));
    assert!(!is_safe_relative_path(Path::new("./../../etc/passwd")));
}

#[test]
fn test_is_safe_relative_path_rejects_absolute() {
    assert!(!is_safe_relative_path(Path::new("/etc/passwd")));
    assert!(!is_safe_relative_path(Path::new("/usr/bin/sh")));
}

#[test]
fn test_is_safe_relative_path_rejects_empty() {
    assert!(!is_safe_relative_path(Path::new("")));
    assert!(!is_safe_relative_path(Path::new(".")));
}

#[test]
fn test_is_new_path_safe_within_working_dir() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let working_dir = dir.path();

    std::fs::create_dir(working_dir.join("src")).unwrap();

    assert!(is_new_path_safe(Path::new("src/new_file.rs"), working_dir));
    assert!(is_new_path_safe(
        Path::new("src/subdir/new_file.rs"),
        working_dir
    ));
    assert!(is_new_path_safe(Path::new("new_file.rs"), working_dir));
}

#[cfg(unix)]
#[test]
fn test_is_new_path_safe_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let working_dir = tempdir().unwrap();
    let outside_dir = tempdir().unwrap();

    let symlink_path = working_dir.path().join("escape_link");
    symlink(outside_dir.path(), &symlink_path).unwrap();

    assert!(!is_new_path_safe(
        Path::new("escape_link/malicious.rs"),
        working_dir.path()
    ));
}

#[test]
fn test_is_new_path_safe_rejects_path_under_file() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let working_dir = dir.path();

    std::fs::write(working_dir.join("main.rs"), "fn main() {}").unwrap();

    assert!(!is_new_path_safe(
        Path::new("main.rs/subfile.rs"),
        working_dir
    ));

    assert!(is_new_path_safe(Path::new("lib.rs"), working_dir));

    std::fs::create_dir(working_dir.join("src")).unwrap();
    assert!(is_new_path_safe(Path::new("src/mod.rs"), working_dir));
}

#[cfg(unix)]
#[test]
fn test_is_new_path_safe_rejects_symlink_to_file() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let working_dir = dir.path();

    let target_file = working_dir.join("target.rs");
    std::fs::write(&target_file, "fn foo() {}").unwrap();

    let symlink_path = working_dir.join("link_to_file");
    symlink(&target_file, &symlink_path).unwrap();

    assert!(!is_new_path_safe(
        Path::new("link_to_file/new.rs"),
        working_dir
    ));

    assert!(is_new_path_safe(Path::new("link_to_file"), working_dir));
}

#[cfg(unix)]
#[test]
fn test_is_new_path_safe_allows_symlink_to_directory() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let working_dir = dir.path();

    let target_dir = working_dir.join("target_dir");
    std::fs::create_dir(&target_dir).unwrap();

    let symlink_path = working_dir.join("link_to_dir");
    symlink(&target_dir, &symlink_path).unwrap();

    assert!(is_new_path_safe(
        Path::new("link_to_dir/new.rs"),
        working_dir
    ));
}
