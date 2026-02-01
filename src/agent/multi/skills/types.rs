//! Skill system types for task methodology definition.
//!
//! Skills represent HOW to perform a task. Unlike rules (WHAT knowledge),
//! skills are explicitly invoked and define the methodology for specific operations.
//!
//! The v3.0 architecture defines 5 fixed skills:
//! - code-review: Code quality review methodology
//! - implement: Feature implementation methodology
//! - plan: Design and planning methodology
//! - debug: Issue investigation methodology
//! - refactor: Code improvement methodology

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The 5 fixed skill types defined by artifact-architecture-v3.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillType {
    CodeReview,
    Implement,
    Plan,
    Debug,
    Refactor,
}

impl SkillType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::CodeReview => "code-review",
            Self::Implement => "implement",
            Self::Plan => "plan",
            Self::Debug => "debug",
            Self::Refactor => "refactor",
        }
    }

    pub fn directory(&self) -> &'static str {
        self.name()
    }

    pub fn all() -> &'static [SkillType] {
        &[
            Self::CodeReview,
            Self::Implement,
            Self::Plan,
            Self::Debug,
            Self::Refactor,
        ]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "code-review" | "codereview" | "review" => Some(Self::CodeReview),
            "implement" | "implementation" | "code" => Some(Self::Implement),
            "plan" | "planning" | "design" => Some(Self::Plan),
            "debug" | "debugging" | "investigate" => Some(Self::Debug),
            "refactor" | "refactoring" | "improve" => Some(Self::Refactor),
            _ => None,
        }
    }
}

impl std::fmt::Display for SkillType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A skill definition loaded from the filesystem.
#[derive(Debug, Clone)]
pub struct Skill {
    pub skill_type: SkillType,
    pub methodology: String,
    pub source_path: PathBuf,
}

impl Skill {
    pub fn new(skill_type: SkillType, methodology: String, source_path: PathBuf) -> Self {
        Self {
            skill_type,
            methodology,
            source_path,
        }
    }

    pub fn name(&self) -> &'static str {
        self.skill_type.name()
    }

    /// Create a default skill with built-in methodology.
    pub fn default_for(skill_type: SkillType) -> Self {
        let methodology = match skill_type {
            SkillType::CodeReview => DEFAULT_CODE_REVIEW_METHODOLOGY,
            SkillType::Implement => DEFAULT_IMPLEMENT_METHODOLOGY,
            SkillType::Plan => DEFAULT_PLAN_METHODOLOGY,
            SkillType::Debug => DEFAULT_DEBUG_METHODOLOGY,
            SkillType::Refactor => DEFAULT_REFACTOR_METHODOLOGY,
        };

        Self {
            skill_type,
            methodology: methodology.to_string(),
            source_path: PathBuf::new(),
        }
    }
}

const DEFAULT_CODE_REVIEW_METHODOLOGY: &str = r"# Code Review Methodology

## Objective
Systematically review code changes for correctness, security, and maintainability.

## Process
1. **Understand Context**: Read the task description and identify the scope of changes.
2. **Analyze Changes**: Review each modified file for:
   - Logic correctness and edge cases
   - Security vulnerabilities (injection, secrets, validation)
   - Error handling completeness
   - Code clarity and maintainability
   - Adherence to project conventions
3. **Report Findings**: Output structured feedback with file:line references.

## Output Format
- `PASS` - No issues found
- `ISSUES` - List each issue: `[SEVERITY] file:line - description`

Severity levels: CRITICAL, HIGH, MEDIUM, LOW
";

const DEFAULT_IMPLEMENT_METHODOLOGY: &str = r"# Implementation Methodology

## Objective
Implement features or fixes with clean, maintainable code.

## Process
1. **Understand Requirements**: Parse the task description for acceptance criteria.
2. **Plan Changes**: Identify files to modify and changes needed.
3. **Implement**:
   - Follow existing code patterns and conventions
   - Handle error cases appropriately
   - Keep changes minimal and focused
4. **Verify**: Ensure changes compile and basic functionality works.

## Principles
- Prefer editing existing files over creating new ones
- Don't add unnecessary abstractions
- Match existing code style
- Handle errors at appropriate boundaries
";

const DEFAULT_PLAN_METHODOLOGY: &str = r"# Planning Methodology

## Objective
Create actionable implementation plans for features or changes.

## Process
1. **Analyze Scope**: Understand what needs to be built/changed.
2. **Identify Components**: List affected modules, files, and dependencies.
3. **Sequence Tasks**: Order tasks by dependencies.
4. **Estimate Complexity**: Categorize each task's complexity.
5. **Document Risks**: Note potential blockers or unknowns.

## Output Format
- Clear task list with dependencies
- Affected files per task
- Risk factors and mitigations
";

const DEFAULT_DEBUG_METHODOLOGY: &str = r"# Debug Methodology

## Objective
Investigate and resolve issues systematically.

## Process
1. **Reproduce**: Understand how to trigger the issue.
2. **Isolate**: Narrow down the scope of the problem.
3. **Analyze**: Examine code paths, logs, and error messages.
4. **Hypothesize**: Form theories about root cause.
5. **Verify**: Test hypotheses with targeted investigations.
6. **Fix**: Implement minimal fix addressing root cause.

## Principles
- Focus on root cause, not symptoms
- Avoid side-effect fixes that mask the real issue
- Document findings for future reference
";

const DEFAULT_REFACTOR_METHODOLOGY: &str = r"# Refactor Methodology

## Objective
Improve code structure without changing behavior.

## Process
1. **Identify Targets**: Find code smells or improvement opportunities.
2. **Plan Changes**: Design the refactored structure.
3. **Incremental Steps**: Make small, verifiable changes.
4. **Verify Behavior**: Ensure functionality remains unchanged.

## Principles
- Preserve existing behavior exactly
- Make tests pass at each step
- Keep refactoring separate from feature changes
- Improve clarity without over-engineering
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_type_names() {
        assert_eq!(SkillType::CodeReview.name(), "code-review");
        assert_eq!(SkillType::Implement.name(), "implement");
        assert_eq!(SkillType::Plan.name(), "plan");
        assert_eq!(SkillType::Debug.name(), "debug");
        assert_eq!(SkillType::Refactor.name(), "refactor");
    }

    #[test]
    fn test_skill_type_from_name() {
        assert_eq!(
            SkillType::from_name("code-review"),
            Some(SkillType::CodeReview)
        );
        assert_eq!(SkillType::from_name("review"), Some(SkillType::CodeReview));
        assert_eq!(
            SkillType::from_name("implement"),
            Some(SkillType::Implement)
        );
        assert_eq!(SkillType::from_name("plan"), Some(SkillType::Plan));
        assert_eq!(SkillType::from_name("debug"), Some(SkillType::Debug));
        assert_eq!(SkillType::from_name("refactor"), Some(SkillType::Refactor));
        assert_eq!(SkillType::from_name("unknown"), None);
    }

    #[test]
    fn test_skill_type_all() {
        let all = SkillType::all();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_default_skill() {
        let skill = Skill::default_for(SkillType::CodeReview);
        assert_eq!(skill.skill_type, SkillType::CodeReview);
        assert!(skill.methodology.contains("Code Review"));
    }
}
