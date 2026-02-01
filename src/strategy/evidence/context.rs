//! Gathering context that accumulates across escalation levels.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::planning::Evidence;

use super::EvidenceGap;

/// Context that accumulates across evidence gathering attempts.
#[derive(Debug, Clone)]
pub struct GatheringContext {
    pub description: String,
    pub accumulated_evidence: Option<Evidence>,
    pub identified_gaps: Vec<EvidenceGap>,
    pub attempted_levels: HashSet<u8>,
    pub search_hints: Vec<SearchHint>,
    pub failure_reasons: Vec<String>,
}

impl GatheringContext {
    pub fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            accumulated_evidence: None,
            identified_gaps: Vec::new(),
            attempted_levels: HashSet::new(),
            search_hints: Vec::new(),
            failure_reasons: Vec::new(),
        }
    }

    pub fn add_gaps(&mut self, gaps: Vec<EvidenceGap>) {
        for gap in gaps {
            if !self.identified_gaps.iter().any(|g| g.area == gap.area) {
                self.identified_gaps.push(gap);
            }
        }
    }

    pub fn merge_evidence(&mut self, evidence: Evidence) {
        if let Some(existing) = &mut self.accumulated_evidence {
            existing.merge(evidence);
        } else {
            self.accumulated_evidence = Some(evidence);
        }
    }

    pub fn mark_level_attempted(&mut self, level: u8) {
        self.attempted_levels.insert(level);
    }

    pub fn has_attempted(&self, level: u8) -> bool {
        self.attempted_levels.contains(&level)
    }

    pub fn add_search_hint(&mut self, hint: SearchHint) {
        self.search_hints.push(hint);
    }

    pub fn add_failure(&mut self, reason: impl Into<String>) {
        self.failure_reasons.push(reason.into());
    }

    pub fn gaps(&self) -> &[EvidenceGap] {
        &self.identified_gaps
    }

    pub fn high_priority_gaps(&self) -> Vec<&EvidenceGap> {
        self.identified_gaps
            .iter()
            .filter(|g| {
                matches!(
                    g.importance,
                    super::GapImportance::High | super::GapImportance::Critical
                )
            })
            .collect()
    }
}

/// Hint for targeted search in next level.
#[derive(Debug, Clone)]
pub struct SearchHint {
    pub hint_type: SearchHintType,
    pub value: String,
    pub source_level: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchHintType {
    /// Look for a specific file pattern.
    FilePattern,
    /// Look for a specific symbol/type.
    Symbol,
    /// Look in a specific directory.
    Directory,
    /// Search for content pattern.
    ContentPattern,
}

impl SearchHint {
    pub fn file_pattern(pattern: impl Into<String>, source_level: u8) -> Self {
        Self {
            hint_type: SearchHintType::FilePattern,
            value: pattern.into(),
            source_level,
        }
    }

    pub fn symbol(name: impl Into<String>, source_level: u8) -> Self {
        Self {
            hint_type: SearchHintType::Symbol,
            value: name.into(),
            source_level,
        }
    }

    pub fn directory(path: impl Into<String>, source_level: u8) -> Self {
        Self {
            hint_type: SearchHintType::Directory,
            value: path.into(),
            source_level,
        }
    }

    pub fn to_path(&self) -> Option<PathBuf> {
        match self.hint_type {
            SearchHintType::Directory => Some(PathBuf::from(&self.value)),
            _ => None,
        }
    }
}
