//! Utility functions for agent task processing.

use std::path::{Component, Path, PathBuf};

/// Validate that a file path stays within workspace boundaries.
///
/// Rejects absolute paths, `..` traversal, and paths that normalize outside the root.
/// Uses lexical normalization (no filesystem access) since files may not exist yet.
pub fn validate_workspace_path(path: &str, workspace_root: &Path) -> Option<PathBuf> {
    let p = Path::new(path);

    if p.is_absolute() {
        return None;
    }

    for component in p.components() {
        if matches!(component, Component::ParentDir) {
            return None;
        }
    }

    let full_path = workspace_root.join(p);
    let normalized = normalize_path(&full_path);

    if normalized.starts_with(workspace_root) {
        Some(normalized)
    } else {
        None
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other),
        }
    }
    result
}

/// Calculate composite priority score from value and risk.
/// Formula: value_score * 0.6 + risk_score * 0.4
pub fn calculate_priority_score(value: Option<f64>, risk: Option<f64>) -> f64 {
    let value_score = value.unwrap_or(0.5);
    let risk_score = risk.unwrap_or(0.5);
    value_score * 0.6 + risk_score * 0.4
}

/// Extract file path from a line of text.
pub fn extract_file_path(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    for part in line.split(|c: char| c.is_whitespace() || c == ',') {
        if part.is_empty() {
            continue;
        }
        let cleaned = part.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '"' | '\'' | ':' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
        });

        if cleaned.len() > 2
            && (cleaned.contains('/') || cleaned.starts_with("./"))
            && !cleaned.starts_with("http")
            && !cleaned.starts_with("//")
            && !cleaned.contains('\0')
            && cleaned.chars().all(|c| !c.is_control() || c == '\t')
        {
            return Some(cleaned.to_string());
        }
    }
    None
}

/// Extract multiple file paths from output text.
///
/// When `workspace_root` is provided, paths are validated against workspace boundaries
/// to prevent `../` traversal attacks from LLM output.
pub fn extract_files_from_output(
    output: &str,
    limit: usize,
    workspace_root: Option<&Path>,
) -> Vec<String> {
    if output.is_empty() || limit == 0 {
        return Vec::new();
    }

    let mut files = Vec::new();

    for line in output.lines() {
        if line.len() > 10_000 {
            continue;
        }

        for part in line.split_whitespace() {
            let cleaned = part.trim_matches(|c: char| {
                matches!(
                    c,
                    '`' | '"'
                        | '\''
                        | ','
                        | ':'
                        | ';'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                )
            });

            let looks_like_path = cleaned.len() > 2
                && cleaned.len() < 4096
                && (cleaned.contains('/') || (cleaned.contains('.') && !cleaned.starts_with('.')))
                && !cleaned.starts_with("http")
                && !cleaned.starts_with("//")
                && !cleaned.contains("://")
                && !cleaned.contains('\0')
                && cleaned.chars().all(|c| !c.is_control() || c == '\t');

            if looks_like_path {
                if let Some(root) = workspace_root
                    && validate_workspace_path(cleaned, root).is_none()
                {
                    continue;
                }

                let path_str = cleaned.to_string();
                if !files.contains(&path_str) {
                    files.push(path_str);
                    if files.len() >= limit {
                        return files;
                    }
                }
            }
        }
    }

    files
}

/// Extract a field value from a key-value formatted line.
pub fn extract_field(line: &str, key: &str) -> Option<String> {
    if line.is_empty() || key.is_empty() {
        return None;
    }

    if line.len() > 10_000 || key.len() > 256 {
        return None;
    }

    let base_key = key.trim_end_matches(':').trim_end_matches('=');
    if base_key.is_empty() {
        return None;
    }

    let key_colon = format!("{}:", base_key);
    let key_eq = format!("{}=", base_key);

    let (start_pos, key_len) = if let Some(pos) = line.find(&key_colon) {
        (pos, key_colon.len())
    } else if let Some(pos) = line.find(&key_eq) {
        (pos, key_eq.len())
    } else if let Some(pos) = line.find(key) {
        (pos, key.len())
    } else {
        return None;
    };

    if start_pos + key_len > line.len() {
        return None;
    }

    let rest = line[start_pos + key_len..].trim_start();

    if let Some(value) = rest
        .strip_prefix('"')
        .and_then(|inner| inner.find('"').map(|end| inner[..end].to_string()))
    {
        if !value.is_empty() && !value.contains('\0') {
            return Some(value);
        } else {
            return None;
        }
    }

    let value = if let Some(pipe_pos) = rest.find('|') {
        &rest[..pipe_pos]
    } else {
        let mut end_pos = rest.len();
        for (i, c) in rest.char_indices() {
            if (c == '=' || c == ':')
                && let Some(space_pos) = rest[..i].rfind(char::is_whitespace)
            {
                end_pos = space_pos;
                break;
            }
        }
        &rest[..end_pos]
    };

    let value = value.trim().trim_matches('"');

    if value.is_empty() || value.contains('\0') {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_field_quoted() {
        let line = r#"task: "implement auth" | module: "auth" | deps: "db""#;
        assert_eq!(extract_field(line, "task:"), Some("implement auth".into()));
        assert_eq!(extract_field(line, "module:"), Some("auth".into()));
        assert_eq!(extract_field(line, "deps:"), Some("db".into()));
    }

    #[test]
    fn test_extract_field_equals_format() {
        let line = r#"type="boundary_violation" | severity="error" | file="path.rs""#;
        assert_eq!(
            extract_field(line, "type:"),
            Some("boundary_violation".into())
        );
        assert_eq!(extract_field(line, "severity="), Some("error".into()));
        assert_eq!(extract_field(line, "file"), Some("path.rs".into()));
    }

    #[test]
    fn test_extract_file_path_basic() {
        assert_eq!(
            extract_file_path("src/main.rs"),
            Some("src/main.rs".to_string())
        );
        assert_eq!(
            extract_file_path("./config.toml"),
            Some("./config.toml".to_string())
        );
    }

    #[test]
    fn test_extract_file_path_with_quotes() {
        assert_eq!(
            extract_file_path(r#""src/lib.rs""#),
            Some("src/lib.rs".to_string())
        );
    }

    #[test]
    fn test_extract_files_from_output_multiple() {
        let output = "Modified:\n  src/main.rs\n  src/lib.rs\n  tests/test.rs";
        let files = extract_files_from_output(output, 10, None);
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(files.contains(&"tests/test.rs".to_string()));
    }

    #[test]
    fn test_extract_files_from_output_limit() {
        let output = "src/a.rs src/b.rs src/c.rs src/d.rs";
        let files = extract_files_from_output(output, 2, None);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_validate_workspace_path_valid() {
        let root = Path::new("/project");
        assert!(validate_workspace_path("src/main.rs", root).is_some());
        assert!(validate_workspace_path("./src/lib.rs", root).is_some());
        assert!(validate_workspace_path("tests/test.rs", root).is_some());
    }

    #[test]
    fn test_validate_workspace_path_rejects_traversal() {
        let root = Path::new("/project");
        assert!(validate_workspace_path("../etc/passwd", root).is_none());
        assert!(validate_workspace_path("src/../../etc/hosts", root).is_none());
        assert!(validate_workspace_path("../../../secret", root).is_none());
    }

    #[test]
    fn test_validate_workspace_path_rejects_absolute() {
        let root = Path::new("/project");
        assert!(validate_workspace_path("/etc/passwd", root).is_none());
        assert!(validate_workspace_path("/usr/bin/ls", root).is_none());
    }

    #[test]
    fn test_extract_files_with_workspace_validation() {
        let root = Path::new("/project");
        let output = "src/main.rs ../etc/passwd src/lib.rs ../../secret.key";
        let files = extract_files_from_output(output, 10, Some(root));
        assert!(files.contains(&"src/main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(!files.iter().any(|f| f.contains("etc/passwd")));
        assert!(!files.iter().any(|f| f.contains("secret")));
    }
}
