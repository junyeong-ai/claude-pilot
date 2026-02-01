use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ignore::gitignore::Gitignore;
use tracing::debug;

use crate::utils::load_gitignore;

pub struct EvidenceFilter {
    working_dir: PathBuf,
    exclude_dirs: HashSet<String>,
    gitignore: Option<Gitignore>,
}

impl EvidenceFilter {
    pub fn new(working_dir: &Path, exclude_dirs: &[String]) -> Self {
        let gitignore = load_gitignore(working_dir);

        Self {
            working_dir: working_dir.to_path_buf(),
            exclude_dirs: exclude_dirs.iter().cloned().collect(),
            gitignore,
        }
    }

    pub fn is_path_allowed(&self, path: &Path) -> bool {
        // Check exclude_dirs using path segment matching to avoid false positives
        // (e.g., "targeted/" should NOT match exclude pattern "target")
        for exclude in &self.exclude_dirs {
            if Self::path_contains_segment(path, exclude) {
                debug!(path = %path.display(), exclude, "Excluded by exclude_dirs");
                return false;
            }
        }

        if let Some(ref gitignore) = self.gitignore {
            let full_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                self.working_dir.join(path)
            };

            let is_dir = full_path.is_dir();
            if gitignore.matched(&full_path, is_dir).is_ignore() {
                debug!(path = %path.display(), "Excluded by gitignore");
                return false;
            }
        }

        true
    }

    /// Checks if any path segment exactly matches the exclude pattern.
    /// This prevents false positives like "targeted/" matching "target".
    fn path_contains_segment(path: &Path, segment: &str) -> bool {
        for component in path.components() {
            if let std::path::Component::Normal(os_str) = component
                && os_str.to_string_lossy() == segment
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_exclude_dirs_filter() {
        let temp = TempDir::new().unwrap();
        let filter = EvidenceFilter::new(temp.path(), &["node_modules".into(), "target".into()]);

        assert!(filter.is_path_allowed(Path::new("src/main.rs")));
        assert!(!filter.is_path_allowed(Path::new("node_modules/pkg/index.js")));
        assert!(!filter.is_path_allowed(Path::new("target/debug/binary")));
    }

    #[test]
    fn test_gitignore_integration() {
        let temp = TempDir::new().unwrap();

        fs::write(temp.path().join(".gitignore"), "*.log\nbuild/\n").unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(temp.path().join("debug.log"), "log content").unwrap();

        let filter = EvidenceFilter::new(temp.path(), &[]);

        assert!(filter.is_path_allowed(Path::new("src/main.rs")));
        assert!(!filter.is_path_allowed(Path::new("debug.log")));
    }

    #[test]
    fn test_exclude_dirs_exact_segment_match() {
        let temp = TempDir::new().unwrap();
        let filter = EvidenceFilter::new(temp.path(), &["target".into()]);

        // Should exclude paths containing "target" as exact segment
        assert!(!filter.is_path_allowed(Path::new("target/debug/binary")));
        assert!(!filter.is_path_allowed(Path::new("crate/target/release")));

        // Should NOT exclude paths where "target" is a substring of segment
        assert!(filter.is_path_allowed(Path::new("targeted/foo.rs")));
        assert!(filter.is_path_allowed(Path::new("src/targeting.rs")));
        assert!(filter.is_path_allowed(Path::new("retarget/main.rs")));
    }

    #[test]
    fn test_path_contains_segment() {
        assert!(EvidenceFilter::path_contains_segment(
            Path::new("a/target/b"),
            "target"
        ));
        assert!(EvidenceFilter::path_contains_segment(
            Path::new("target/foo"),
            "target"
        ));
        assert!(EvidenceFilter::path_contains_segment(
            Path::new("foo/target"),
            "target"
        ));
        assert!(!EvidenceFilter::path_contains_segment(
            Path::new("targeted/foo"),
            "target"
        ));
        assert!(!EvidenceFilter::path_contains_segment(
            Path::new("foo/targeting"),
            "target"
        ));
    }
}
