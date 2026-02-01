//! Rule resolver for determining applicable rules based on context.

use std::path::{Path, PathBuf};

use super::registry::RuleRegistry;
use super::types::{ResolvedRules, RuleCategory};

/// Resolves which rules apply to a given context.
pub struct RuleResolver<'a> {
    registry: &'a RuleRegistry,
}

impl<'a> RuleResolver<'a> {
    pub fn new(registry: &'a RuleRegistry) -> Self {
        Self { registry }
    }

    /// Resolve all applicable rules for the given context.
    ///
    /// # Arguments
    /// * `affected_files` - Files being worked on
    /// * `task_content` - Task description for keyword triggers
    pub fn resolve(&self, affected_files: &[PathBuf], task_content: &str) -> ResolvedRules {
        let mut resolved = ResolvedRules::new();

        // 1. Always inject project rules
        resolved.extend(self.registry.project_rules().cloned());

        // 2. Match tech rules by extension
        let mut seen_extensions = std::collections::HashSet::new();
        for file in affected_files {
            if let Some(ext) = file.extension().and_then(|e| e.to_str())
                && seen_extensions.insert(ext.to_string())
            {
                for rule in self.registry.tech_rules_for_extension(ext) {
                    resolved.add_unique(rule.clone());
                }
            }
        }

        // 3. Match module rules by path
        for file in affected_files {
            for rule in self.registry.module_rules_for_path(file) {
                resolved.add_unique(rule.clone());
            }
        }

        // 4. Match framework rules (path + trigger combined)
        for rule in self.registry.by_category(RuleCategory::Framework) {
            if rule.applies_to(affected_files, task_content) {
                resolved.add_unique(rule.clone());
            }
        }

        // 5. Match group rules if files span multiple modules
        for rule in self.registry.by_category(RuleCategory::Group) {
            if rule.applies_to(affected_files, task_content) {
                resolved.add_unique(rule.clone());
            }
        }

        // 6. Match domain rules by keyword triggers
        for rule in self.registry.domain_rules_for_content(task_content) {
            resolved.add_unique(rule.clone());
        }

        resolved.sort_by_priority();
        resolved
    }

    /// Resolve rules for a specific file only.
    pub fn resolve_for_file(&self, file: &Path) -> ResolvedRules {
        self.resolve(&[file.to_path_buf()], "")
    }

    /// Resolve rules for task content only (no files).
    pub fn resolve_for_content(&self, content: &str) -> ResolvedRules {
        self.resolve(&[], content)
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{InjectionConfig, Rule, RuleMetadata};
    use super::*;

    fn create_test_rule(name: &str, category: RuleCategory, injection: InjectionConfig) -> Rule {
        Rule::new(
            RuleMetadata::new(name, category).with_injection(injection),
            format!("{} content", name),
            PathBuf::new(),
        )
    }

    #[test]
    fn test_resolved_rules_priority_order() {
        let mut resolved = ResolvedRules::new();

        resolved.add(create_test_rule(
            "domain",
            RuleCategory::Domain,
            InjectionConfig::always(),
        ));
        resolved.add(create_test_rule(
            "project",
            RuleCategory::Project,
            InjectionConfig::always(),
        ));
        resolved.add(create_test_rule(
            "tech",
            RuleCategory::Tech,
            InjectionConfig::always(),
        ));

        resolved.sort_by_priority();

        let priorities: Vec<_> = resolved.rules().iter().map(|r| r.priority()).collect();
        assert_eq!(priorities, vec![100, 90, 60]);
    }

    #[test]
    fn test_add_unique_prevents_duplicates() {
        let mut resolved = ResolvedRules::new();

        let rule = create_test_rule(
            "rust",
            RuleCategory::Tech,
            InjectionConfig::paths(vec!["**/*.rs".into()]),
        );

        assert!(resolved.add_unique(rule.clone()));
        assert!(!resolved.add_unique(rule));

        assert_eq!(resolved.len(), 1);
    }

    #[test]
    fn test_resolve_for_file() {
        let registry = super::super::registry::RuleRegistry::new(PathBuf::new());
        let resolver = RuleResolver::new(&registry);

        let path = Path::new("src/main.rs");
        let resolved = resolver.resolve_for_file(path);

        // Empty registry returns empty rules
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolve_for_content() {
        let registry = super::super::registry::RuleRegistry::new(PathBuf::new());
        let resolver = RuleResolver::new(&registry);

        let resolved = resolver.resolve_for_content("async function");

        // Empty registry returns empty rules
        assert!(resolved.is_empty());
    }
}
