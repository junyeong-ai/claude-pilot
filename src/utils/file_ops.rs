use crate::error::Result;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Load and parse .gitignore from a directory.
/// Returns None if .gitignore doesn't exist or fails to parse.
pub fn load_gitignore(working_dir: &Path) -> Option<Gitignore> {
    let gitignore_path = working_dir.join(".gitignore");
    if !gitignore_path.exists() {
        return None;
    }

    let mut builder = GitignoreBuilder::new(working_dir);
    if builder.add(&gitignore_path).is_some() {
        return None;
    }
    builder.build().ok()
}

/// Counts lines in a file. Handles Unix (LF), Windows (CRLF), and mixed line endings.
pub fn count_lines_in_file<P: AsRef<Path>>(path: P) -> Result<usize> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let line_count = reader.lines().count();
    Ok(line_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // T005: Basic tests - existing file, empty file, non-existent file

    #[test]
    fn test_count_lines_normal_file() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "line 1").unwrap();
        writeln!(file, "line 2").unwrap();
        writeln!(file, "line 3").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_lines_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_count_lines_nonexistent_file() {
        let result = count_lines_in_file("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
    }

    // T006: Unix (LF) and Windows (CRLF) line endings

    #[test]
    fn test_count_lines_unix_endings() {
        let mut file = NamedTempFile::new().unwrap();
        // Write with explicit LF
        file.write_all(b"line1\nline2\nline3\n").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_lines_windows_endings() {
        let mut file = NamedTempFile::new().unwrap();
        // Write with explicit CRLF
        file.write_all(b"line1\r\nline2\r\nline3\r\n").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 3);
    }

    // T007: Mixed line endings

    #[test]
    fn test_count_lines_mixed_endings() {
        let mut file = NamedTempFile::new().unwrap();
        // Mix of LF and CRLF
        file.write_all(b"line1\nline2\r\nline3\nline4\r\n").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 4);
    }

    // T008: No trailing newline

    #[test]
    fn test_count_lines_no_trailing_newline() {
        let mut file = NamedTempFile::new().unwrap();
        // No newline at end
        file.write_all(b"line1\nline2\nline3").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 3);
    }

    // T009: Whitespace only lines

    #[test]
    fn test_count_lines_whitespace_only() {
        let mut file = NamedTempFile::new().unwrap();
        // Lines with only whitespace should still count as lines
        file.write_all(b"   \n\t\t\n  \t  \n").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_count_lines_single_line() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"single line with newline\n").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_count_lines_single_line_no_newline() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"single line without newline").unwrap();
        file.flush().unwrap();

        let count = count_lines_in_file(file.path()).unwrap();
        assert_eq!(count, 1);
    }
}
