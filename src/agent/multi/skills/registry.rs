//! Skill registry for loading the 5 fixed skills.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::fs;
use tracing::debug;

use super::types::{Skill, SkillType};
use crate::error::Result;

/// Registry for loading and accessing the 5 fixed skills.
///
/// Expected directory structure:
/// ```text
/// .claude/skills/
/// ├── code-review/
/// │   └── SKILL.md
/// ├── implement/
/// │   └── SKILL.md
/// ├── plan/
/// │   └── SKILL.md
/// ├── debug/
/// │   └── SKILL.md
/// └── refactor/
///     └── SKILL.md
/// ```
pub struct SkillRegistry {
    skills: HashMap<SkillType, Skill>,
    skills_dir: PathBuf,
}

impl SkillRegistry {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills: HashMap::new(),
            skills_dir,
        }
    }

    /// Load all skills from the skills directory.
    /// Falls back to default methodologies if files don't exist.
    pub async fn load(&mut self) -> Result<()> {
        for skill_type in SkillType::all() {
            let skill = self.load_skill(*skill_type).await;
            self.skills.insert(*skill_type, skill);
        }

        debug!(count = self.skills.len(), "Loaded skills");
        Ok(())
    }

    /// Load a single skill, falling back to default if not found.
    async fn load_skill(&self, skill_type: SkillType) -> Skill {
        let skill_dir = self.skills_dir.join(skill_type.directory());
        let skill_file = skill_dir.join("SKILL.md");

        if skill_file.exists()
            && let Ok(content) = fs::read_to_string(&skill_file).await
        {
            return Skill::new(skill_type, content, skill_file);
        }

        Skill::default_for(skill_type)
    }

    /// Get a skill by type.
    pub fn get(&self, skill_type: SkillType) -> &Skill {
        self.skills.get(&skill_type).unwrap_or_else(|| {
            static DEFAULT_CODE_REVIEW: std::sync::OnceLock<Skill> = std::sync::OnceLock::new();
            static DEFAULT_IMPLEMENT: std::sync::OnceLock<Skill> = std::sync::OnceLock::new();
            static DEFAULT_PLAN: std::sync::OnceLock<Skill> = std::sync::OnceLock::new();
            static DEFAULT_DEBUG: std::sync::OnceLock<Skill> = std::sync::OnceLock::new();
            static DEFAULT_REFACTOR: std::sync::OnceLock<Skill> = std::sync::OnceLock::new();

            match skill_type {
                SkillType::CodeReview => {
                    DEFAULT_CODE_REVIEW.get_or_init(|| Skill::default_for(SkillType::CodeReview))
                }
                SkillType::Implement => {
                    DEFAULT_IMPLEMENT.get_or_init(|| Skill::default_for(SkillType::Implement))
                }
                SkillType::Plan => DEFAULT_PLAN.get_or_init(|| Skill::default_for(SkillType::Plan)),
                SkillType::Debug => {
                    DEFAULT_DEBUG.get_or_init(|| Skill::default_for(SkillType::Debug))
                }
                SkillType::Refactor => {
                    DEFAULT_REFACTOR.get_or_init(|| Skill::default_for(SkillType::Refactor))
                }
            }
        })
    }

    /// Get the code review skill.
    pub fn code_review(&self) -> &Skill {
        self.get(SkillType::CodeReview)
    }

    /// Get the implement skill.
    pub fn implement(&self) -> &Skill {
        self.get(SkillType::Implement)
    }

    /// Get the plan skill.
    pub fn plan(&self) -> &Skill {
        self.get(SkillType::Plan)
    }

    /// Get the debug skill.
    pub fn debug(&self) -> &Skill {
        self.get(SkillType::Debug)
    }

    /// Get the refactor skill.
    pub fn refactor(&self) -> &Skill {
        self.get(SkillType::Refactor)
    }

    /// Check if registry has loaded skills.
    pub fn is_loaded(&self) -> bool {
        !self.skills.is_empty()
    }

    /// Get all loaded skills.
    pub fn all(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values()
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        let mut registry = Self::new(PathBuf::new());
        for skill_type in SkillType::all() {
            registry
                .skills
                .insert(*skill_type, Skill::default_for(*skill_type));
        }
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_load_default_skills() {
        let temp = TempDir::new().unwrap();
        let mut registry = SkillRegistry::new(temp.path().to_path_buf());
        registry.load().await.unwrap();

        assert!(registry.is_loaded());
        assert_eq!(registry.skills.len(), 5);
    }

    #[tokio::test]
    async fn test_load_custom_skill() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path();

        let review_dir = skills_dir.join("code-review");
        fs::create_dir_all(&review_dir).await.unwrap();
        fs::write(
            review_dir.join("SKILL.md"),
            "# Custom Review\n\nCustom content.",
        )
        .await
        .unwrap();

        let mut registry = SkillRegistry::new(skills_dir.to_path_buf());
        registry.load().await.unwrap();

        let skill = registry.code_review();
        assert!(skill.methodology.contains("Custom Review"));
    }

    #[test]
    fn test_default_registry() {
        let registry = SkillRegistry::default();

        assert!(registry.is_loaded());
        assert!(registry.code_review().methodology.contains("Code Review"));
        assert!(registry.implement().methodology.contains("Implementation"));
        assert!(registry.plan().methodology.contains("Planning"));
        assert!(registry.debug().methodology.contains("Debug"));
        assert!(registry.refactor().methodology.contains("Refactor"));
    }

    #[test]
    fn test_get_skill_fallback() {
        let registry = SkillRegistry::new(PathBuf::new());

        // Even without loading, get() should return defaults
        let skill = registry.get(SkillType::CodeReview);
        assert!(skill.methodology.contains("Code Review"));
    }
}
