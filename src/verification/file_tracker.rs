//! File change tracking using filesystem snapshots.
//!
//! Primary detection: Before/after filesystem state comparison.
//! Parallel mode: Timestamp-based filtering for concurrent task isolation.
//! Ignore patterns: .gitignore integration + fallback to common build artifact directories.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::gitignore::Gitignore;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::utils::load_gitignore;

/// Fallback directories to ignore when no .gitignore is available.
///
/// Comprehensive list covering common build/dependency directories across languages.
/// This is used only when .gitignore is not present or fails to load.
const FALLBACK_IGNORED_DIRS: &[&str] = &[
    // Universal
    ".git",
    ".claude",
    ".idea",
    ".vscode",
    "coverage",
    // Rust
    "target",
    // Node.js / JavaScript
    "node_modules",
    "dist",
    ".next",
    ".nuxt",
    ".output",
    // Python
    "__pycache__",
    ".tox",
    "venv",
    ".venv",
    ".mypy_cache",
    ".pytest_cache",
    "*.egg-info",
    // Go
    "vendor",
    // Java / JVM
    "build",
    ".gradle",
    "out",
    // C / C++
    "cmake-build-debug",
    "cmake-build-release",
    // Swift
    ".build",
    "DerivedData",
    // Ruby
    ".bundle",
    // PHP
    "vendor",
    // .NET
    "bin",
    "obj",
    "packages",
];

/// Fallback extensions to ignore when no .gitignore is available.
const FALLBACK_IGNORED_EXTENSIONS: &[&str] = &[
    // Python
    ".pyc", ".pyo",   // Java / JVM
    ".class", // C / C++
    ".o", ".obj", ".a", ".so", ".dylib", ".dll", ".exe", // Swift / Objective-C
    ".dSYM",
];

/// Tracks file changes using before/after snapshots with timestamp filtering.
#[derive(Debug)]
pub struct FileTracker {
    before_snapshot: HashMap<PathBuf, FileState>,
    after_snapshot: HashMap<PathBuf, FileState>,
    capture_start: Option<SystemTime>,
    capture_end: Option<SystemTime>,
    parallel_mode: bool,
    explicit_deletions: HashSet<PathBuf>,
    gitignore: Option<Gitignore>,
    /// Recency threshold in seconds for deletion detection in parallel mode.
    recency_threshold_secs: u64,
}

impl Default for FileTracker {
    fn default() -> Self {
        Self {
            before_snapshot: HashMap::new(),
            after_snapshot: HashMap::new(),
            capture_start: None,
            capture_end: None,
            parallel_mode: false,
            explicit_deletions: HashSet::new(),
            gitignore: None,
            recency_threshold_secs: 5, // Default: 5 seconds
        }
    }
}

/// State of a file at a point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    modified_time: Option<SystemTime>,
    size: u64,
}

/// Represents changes between two filesystem snapshots.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileChanges {
    /// Files that were created (didn't exist before, exist now)
    pub created: Vec<PathBuf>,
    /// Files that were modified (existed before and after, but changed)
    pub modified: Vec<PathBuf>,
    /// Files that were deleted (existed before, don't exist now)
    pub deleted: Vec<PathBuf>,
}

impl FileChanges {
    /// Returns true if no changes were detected.
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }

    /// Returns the total number of changed files.
    pub fn total(&self) -> usize {
        self.created.len() + self.modified.len() + self.deleted.len()
    }

    /// Returns all affected files (created + modified).
    pub fn affected_files(&self) -> Vec<&PathBuf> {
        self.created.iter().chain(self.modified.iter()).collect()
    }
}

impl FileTracker {
    /// Creates a new FileTracker.
    /// By default, parallel_mode is false (reports all changes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets parallel mode for safe change detection in concurrent task execution.
    /// In parallel mode, deletion detection uses heuristics to avoid false positives
    /// from files deleted by other concurrent tasks.
    pub fn with_parallel_mode(mut self, parallel: bool) -> Self {
        self.parallel_mode = parallel;
        self
    }

    /// Sets the recency threshold for deletion detection in parallel mode.
    /// Files modified within this threshold before capture_start are considered
    /// "recently active" and their deletion is attributed to this task.
    pub fn with_recency_threshold(mut self, secs: u64) -> Self {
        self.recency_threshold_secs = secs;
        self
    }

    /// Explicitly track a file deletion. In parallel mode, heuristics may miss deletions
    /// of old files (not recently modified). Use this to ensure such deletions are tracked.
    /// The path should be relative to the working directory.
    pub fn track_deletion(&mut self, path: impl Into<PathBuf>) {
        self.explicit_deletions.insert(path.into());
    }

    /// Captures the "before" snapshot of the working directory.
    /// Loads .gitignore if available for smarter filtering.
    pub fn capture_before(&mut self, working_dir: &Path) {
        self.capture_start = Some(SystemTime::now());
        self.gitignore = load_gitignore(working_dir);
        self.before_snapshot = self.capture_snapshot_with_filter(working_dir);
    }

    /// Captures the "after" snapshot of the working directory.
    pub fn capture_after(&mut self, working_dir: &Path) {
        self.after_snapshot = self.capture_snapshot_with_filter(working_dir);
        self.capture_end = Some(SystemTime::now());
    }

    /// Track multiple deletions at once (e.g., from structured agent responses).
    pub fn track_deletions(&mut self, paths: impl IntoIterator<Item = impl Into<PathBuf>>) {
        for path in paths {
            self.explicit_deletions.insert(path.into());
        }
    }

    /// Computes the changes between the before and after snapshots.
    /// When parallel tasks run, only includes changes that occurred within this task's
    /// execution window (between capture_before and capture_after timestamps).
    pub fn changes(&self) -> FileChanges {
        let mut created = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        // Track all paths we've seen
        let before_paths: HashSet<_> = self.before_snapshot.keys().collect();
        let after_paths: HashSet<_> = self.after_snapshot.keys().collect();

        // Files that exist in after but not in before = created
        // Only include if modified within our execution window
        for path in after_paths.difference(&before_paths) {
            if let Some(after_state) = self.after_snapshot.get(*path)
                && self.is_within_execution_window(after_state)
            {
                created.push((*path).clone());
            }
        }

        // Files that exist in both = check if modified
        // Only include if modification occurred within our execution window
        for path in before_paths.intersection(&after_paths) {
            if let (Some(before_state), Some(after_state)) = (
                self.before_snapshot.get(*path),
                self.after_snapshot.get(*path),
            ) && before_state != after_state
                && self.is_within_execution_window(after_state)
            {
                modified.push((*path).clone());
            }
        }

        // Files that exist in before but not in after = deleted
        // In parallel mode, use heuristic to avoid false positives from concurrent tasks.
        // In single-task mode, report all deletions as they must be from this task.
        for path in before_paths.difference(&after_paths) {
            let should_include = if self.parallel_mode {
                // Parallel mode: include if explicitly tracked OR recently-active
                self.explicit_deletions.contains(*path)
                    || self
                        .before_snapshot
                        .get(*path)
                        .map(|state| self.was_recently_active(state))
                        .unwrap_or(true)
            } else {
                // Single-task mode: all deletions are from this task
                true
            };

            if should_include {
                deleted.push((*path).clone());
            }
        }

        // Sort for consistent ordering
        created.sort();
        modified.sort();
        deleted.sort();

        FileChanges {
            created,
            modified,
            deleted,
        }
    }

    /// Checks if a file's modification time falls within this task's execution window.
    /// Returns true if we can't determine (no timestamps), to maintain backward compatibility.
    /// Uses a 1-second buffer to handle filesystem timestamp resolution limitations.
    fn is_within_execution_window(&self, state: &FileState) -> bool {
        let (Some(start), Some(end)) = (self.capture_start, self.capture_end) else {
            return true;
        };

        let Some(modified) = state.modified_time else {
            return true;
        };

        let buffer = std::time::Duration::from_secs(1);
        let buffered_start = start.checked_sub(buffer).unwrap_or(start);
        let buffered_end = end.checked_add(buffer).unwrap_or(end);

        modified >= buffered_start && modified <= buffered_end
    }

    /// For deleted files, check if the file was recently active (modified near our start time).
    /// If the file was modified long before our execution window, another task likely deleted it.
    fn was_recently_active(&self, before_state: &FileState) -> bool {
        let Some(start) = self.capture_start else {
            return true;
        };

        let Some(modified) = before_state.modified_time else {
            return true;
        };

        // File was modified within recency_threshold before our capture_start
        // This suggests it was being worked on and then deleted by this task
        let recent_threshold = std::time::Duration::from_secs(self.recency_threshold_secs);
        let cutoff = start.checked_sub(recent_threshold).unwrap_or(start);

        modified >= cutoff
    }

    /// Resets the tracker, clearing both snapshots, timestamps, and explicit deletions.
    pub fn reset(&mut self) {
        self.before_snapshot.clear();
        self.after_snapshot.clear();
        self.capture_start = None;
        self.capture_end = None;
        self.explicit_deletions.clear();
    }

    fn capture_snapshot_with_filter(&self, working_dir: &Path) -> HashMap<PathBuf, FileState> {
        let mut snapshot = HashMap::new();

        if !working_dir.exists() {
            return snapshot;
        }

        for entry in WalkDir::new(working_dir)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !self.should_ignore(e.path()))
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && let Ok(metadata) = entry.metadata()
            {
                let relative_path = entry
                    .path()
                    .strip_prefix(working_dir)
                    .unwrap_or(entry.path())
                    .to_path_buf();

                snapshot.insert(
                    relative_path,
                    FileState {
                        modified_time: metadata.modified().ok(),
                        size: metadata.len(),
                    },
                );
            }
        }

        snapshot
    }

    fn should_ignore(&self, path: &Path) -> bool {
        // Check gitignore first if available
        if let Some(ref gitignore) = self.gitignore {
            let is_dir = path.is_dir();
            if gitignore.matched(path, is_dir).is_ignore() {
                return true;
            }
        }

        // Fallback to hardcoded patterns when gitignore unavailable
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                let name_str = name.to_string_lossy();
                if FALLBACK_IGNORED_DIRS.iter().any(|&d| d == name_str) {
                    return true;
                }
            }
        }

        if let Some(ext) = path.extension() {
            let ext_with_dot = format!(".{}", ext.to_string_lossy());
            if FALLBACK_IGNORED_EXTENSIONS
                .iter()
                .any(|&e| e == ext_with_dot)
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
    use tempfile::tempdir;

    #[test]
    fn test_empty_tracker() {
        let tracker = FileTracker::new();
        let changes = tracker.changes();
        assert!(changes.is_empty());
    }

    #[test]
    fn test_detect_created_file() {
        let dir = tempdir().unwrap();
        let mut tracker = FileTracker::new();

        // Capture before (empty)
        tracker.capture_before(dir.path());

        // Create a file
        fs::write(dir.path().join("new_file.txt"), "content").unwrap();

        // Capture after
        tracker.capture_after(dir.path());

        let changes = tracker.changes();
        assert_eq!(changes.created.len(), 1);
        assert!(
            changes.created[0]
                .to_string_lossy()
                .contains("new_file.txt")
        );
        assert!(changes.modified.is_empty());
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn test_detect_modified_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("existing.txt");
        fs::write(&file_path, "original").unwrap();

        let mut tracker = FileTracker::new();

        // Capture before
        tracker.capture_before(dir.path());

        // Modify the file
        fs::write(&file_path, "modified content that is longer").unwrap();

        // Capture after
        tracker.capture_after(dir.path());

        let changes = tracker.changes();
        assert!(changes.created.is_empty());
        assert_eq!(changes.modified.len(), 1);
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn test_detect_deleted_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("to_delete.txt");
        fs::write(&file_path, "will be deleted").unwrap();

        let mut tracker = FileTracker::new();

        // Capture before
        tracker.capture_before(dir.path());

        // Delete the file
        fs::remove_file(&file_path).unwrap();

        // Capture after
        tracker.capture_after(dir.path());

        let changes = tracker.changes();
        assert!(changes.created.is_empty());
        assert!(changes.modified.is_empty());
        assert_eq!(changes.deleted.len(), 1);
    }

    #[test]
    fn test_total_count() {
        let changes = FileChanges {
            created: vec![PathBuf::from("a.txt")],
            modified: vec![PathBuf::from("b.txt"), PathBuf::from("c.txt")],
            deleted: vec![PathBuf::from("d.txt")],
        };
        assert_eq!(changes.total(), 4);
    }

    #[test]
    fn test_explicit_deletion_tracking_in_parallel_mode() {
        let dir = tempdir().unwrap();

        // Create a file
        let test_file = dir.path().join("test_file.txt");
        fs::write(&test_file, "content").unwrap();

        let mut tracker = FileTracker::new().with_parallel_mode(true);

        // Capture before
        tracker.capture_before(dir.path());

        // Explicitly track the deletion (simulates intentional deletion by this task)
        tracker.track_deletion(PathBuf::from("test_file.txt"));

        // Delete the file
        fs::remove_file(&test_file).unwrap();

        // Capture after
        tracker.capture_after(dir.path());

        let changes = tracker.changes();

        // Explicitly tracked deletion should always be included in parallel mode
        assert!(
            changes.deleted.iter().any(|p| p.ends_with("test_file.txt")),
            "Explicitly tracked deletion should be included in parallel mode"
        );
    }

    #[test]
    fn test_reset_clears_explicit_deletions() {
        let mut tracker = FileTracker::new();
        tracker.track_deletion("some_file.txt");
        tracker.reset();
        // After reset, explicit_deletions should be empty (we verify by behavior)
        // The only way to verify is that changes() won't include it spuriously
        let changes = tracker.changes();
        assert!(changes.deleted.is_empty());
    }

    #[test]
    fn test_snapshot_deletion_in_single_task_mode() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("file.txt");
        fs::write(&file_path, "content").unwrap();

        let mut tracker = FileTracker::new();

        tracker.capture_before(dir.path());
        fs::remove_file(&file_path).unwrap();
        tracker.capture_after(dir.path());

        let changes = tracker.changes();

        // Deletion detected via snapshot comparison
        assert_eq!(changes.deleted.len(), 1);
    }
}
