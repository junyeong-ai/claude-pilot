//! Skill composer for combining skill methodology with resolved rules.

use super::types::Skill;
use crate::agent::multi::rules::ResolvedRules;

/// Composes skill methodology with applicable rules into a complete prompt.
pub struct SkillComposer;

impl SkillComposer {
    /// Compose a skill with resolved rules into a complete system prompt.
    ///
    /// The output structure:
    /// 1. Skill methodology (HOW)
    /// 2. Separator
    /// 3. Injected rules (WHAT) - by priority order
    pub fn compose(skill: &Skill, rules: &ResolvedRules) -> String {
        let mut prompt = String::with_capacity(
            skill.methodology.len()
                + rules.rules().iter().map(|r| r.content.len()).sum::<usize>()
                + 256,
        );

        // 1. Skill methodology (HOW)
        prompt.push_str(&skill.methodology);

        if !rules.is_empty() {
            prompt.push_str("\n\n---\n\n");
            // 2. Injected rules (WHAT) - already sorted by priority
            prompt.push_str(&rules.to_prompt());
        }

        prompt
    }

    /// Compose skill methodology only (no rules).
    pub fn methodology_only(skill: &Skill) -> String {
        skill.methodology.clone()
    }

    /// Compose rules only (no skill).
    pub fn rules_only(rules: &ResolvedRules) -> String {
        rules.to_prompt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::rules::{InjectionConfig, Rule, RuleCategory, RuleMetadata};
    use crate::agent::multi::skills::types::SkillType;
    use std::path::PathBuf;

    fn create_test_skill() -> Skill {
        Skill::new(
            SkillType::CodeReview,
            "# Code Review\n\nReview code carefully.".into(),
            PathBuf::new(),
        )
    }

    fn create_test_rules() -> ResolvedRules {
        let mut rules = ResolvedRules::new();
        rules.add(Rule::new(
            RuleMetadata::new("project", RuleCategory::Project)
                .with_injection(InjectionConfig::always()),
            "Always follow project conventions.".into(),
            PathBuf::new(),
        ));
        rules.add(Rule::new(
            RuleMetadata::new("rust", RuleCategory::Tech)
                .with_injection(InjectionConfig::paths(vec!["**/*.rs".into()])),
            "Use idiomatic Rust patterns.".into(),
            PathBuf::new(),
        ));
        rules.sort_by_priority();
        rules
    }

    #[test]
    fn test_compose_skill_with_rules() {
        let skill = create_test_skill();
        let rules = create_test_rules();

        let prompt = SkillComposer::compose(&skill, &rules);

        assert!(prompt.contains("# Code Review"));
        assert!(prompt.contains("Review code carefully"));
        assert!(prompt.contains("---"));
        assert!(prompt.contains("# Applicable Rules"));
        assert!(prompt.contains("## project (priority 100)"));
        assert!(prompt.contains("## rust (priority 90)"));
    }

    #[test]
    fn test_compose_skill_without_rules() {
        let skill = create_test_skill();
        let rules = ResolvedRules::new();

        let prompt = SkillComposer::compose(&skill, &rules);

        assert!(prompt.contains("# Code Review"));
        assert!(!prompt.contains("---"));
        assert!(!prompt.contains("# Applicable Rules"));
    }

    #[test]
    fn test_methodology_only() {
        let skill = create_test_skill();
        let prompt = SkillComposer::methodology_only(&skill);

        assert!(prompt.contains("# Code Review"));
        assert!(!prompt.contains("# Applicable Rules"));
    }

    #[test]
    fn test_rules_only() {
        let rules = create_test_rules();
        let prompt = SkillComposer::rules_only(&rules);

        assert!(prompt.contains("# Applicable Rules"));
        assert!(!prompt.contains("# Code Review"));
    }

    #[test]
    fn test_rules_priority_order_in_output() {
        let rules = create_test_rules();
        let prompt = SkillComposer::rules_only(&rules);

        let project_pos = prompt.find("## project").unwrap();
        let rust_pos = prompt.find("## rust").unwrap();
        assert!(project_pos < rust_pos);
    }
}
