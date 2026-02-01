//! Rule system types for context-aware knowledge injection.
//!
//! Rules represent domain knowledge (WHAT) that gets auto-injected based on context.
//! They follow a 6-category priority system from artifact-architecture-v3.0.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Rule category determines injection priority.
/// Higher priority rules are injected first and override lower ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleCategory {
    /// Priority 100: Always injected for all contexts
    Project,
    /// Priority 90: Injected by language/file extension
    Tech,
    /// Priority 85: Injected by framework detection (paths + triggers)
    Framework,
    /// Priority 80: Injected by module path
    Module,
    /// Priority 70: Injected for module groups
    Group,
    /// Priority 60: Injected by keyword triggers in task content
    Domain,
}

impl RuleCategory {
    pub fn priority(&self) -> u8 {
        match self {
            Self::Project => 100,
            Self::Tech => 90,
            Self::Framework => 85,
            Self::Module => 80,
            Self::Group => 70,
            Self::Domain => 60,
        }
    }

    pub fn directory_name(&self) -> &'static str {
        match self {
            Self::Project => "",
            Self::Tech => "tech",
            Self::Framework => "frameworks",
            Self::Module => "modules",
            Self::Group => "groups",
            Self::Domain => "domains",
        }
    }
}

/// How a rule should be injected into context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InjectionConfig {
    /// Always inject (project rules)
    #[default]
    Always,
    /// Inject when any file matches the path patterns
    Paths { patterns: Vec<String> },
    /// Inject when task content contains any trigger keyword
    Triggers { keywords: Vec<String> },
    /// Inject when both path and trigger conditions match
    Combined {
        patterns: Vec<String>,
        keywords: Vec<String>,
    },
}

impl InjectionConfig {
    pub fn always() -> Self {
        Self::Always
    }

    pub fn paths(patterns: Vec<String>) -> Self {
        Self::Paths { patterns }
    }

    pub fn triggers(keywords: Vec<String>) -> Self {
        Self::Triggers { keywords }
    }

    pub fn combined(patterns: Vec<String>, keywords: Vec<String>) -> Self {
        Self::Combined { patterns, keywords }
    }

    /// Check if path patterns match the given file.
    pub fn matches_path(&self, file: &str) -> bool {
        match self {
            Self::Always => true,
            Self::Paths { patterns } | Self::Combined { patterns, .. } => {
                patterns.iter().any(|p| Self::glob_match(p, file))
            }
            Self::Triggers { .. } => false,
        }
    }

    /// Check if trigger keywords match the given content.
    pub fn matches_content(&self, content: &str) -> bool {
        let content_lower = content.to_lowercase();
        match self {
            Self::Always => true,
            Self::Triggers { keywords } | Self::Combined { keywords, .. } => keywords
                .iter()
                .any(|k| content_lower.contains(&k.to_lowercase())),
            Self::Paths { .. } => false,
        }
    }

    fn glob_match(pattern: &str, path: &str) -> bool {
        if pattern == "**/*" {
            return true;
        }

        if let Some(suffix) = pattern.strip_prefix("**/") {
            if let Some(ext) = suffix.strip_prefix("*.") {
                return path.ends_with(&format!(".{}", ext));
            }
            return path.contains(suffix) || path.ends_with(suffix);
        }

        if let Some(prefix) = pattern.strip_suffix("/**") {
            return path.starts_with(prefix);
        }

        if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                return path.starts_with(parts[0]) && path.ends_with(parts[1]);
            }
        }

        path.starts_with(pattern) || path == pattern
    }
}

/// Metadata for a rule loaded from the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleMetadata {
    pub name: String,
    pub category: RuleCategory,
    #[serde(default)]
    pub injection: InjectionConfig,
    #[serde(default)]
    pub description: Option<String>,
}

impl RuleMetadata {
    pub fn new(name: impl Into<String>, category: RuleCategory) -> Self {
        Self {
            name: name.into(),
            category,
            injection: InjectionConfig::default(),
            description: None,
        }
    }

    pub fn with_injection(mut self, injection: InjectionConfig) -> Self {
        self.injection = injection;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn priority(&self) -> u8 {
        self.category.priority()
    }
}

/// A fully loaded rule with content.
#[derive(Debug, Clone)]
pub struct Rule {
    pub metadata: RuleMetadata,
    pub content: String,
    pub source_path: PathBuf,
}

impl Rule {
    pub fn new(metadata: RuleMetadata, content: String, source_path: PathBuf) -> Self {
        Self {
            metadata,
            content,
            source_path,
        }
    }

    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    pub fn category(&self) -> RuleCategory {
        self.metadata.category
    }

    pub fn priority(&self) -> u8 {
        self.metadata.priority()
    }

    /// Check if this rule applies to the given context.
    pub fn applies_to(&self, files: &[PathBuf], content: &str) -> bool {
        match &self.metadata.injection {
            InjectionConfig::Always => true,
            InjectionConfig::Paths { .. } => files
                .iter()
                .any(|f| self.metadata.injection.matches_path(&f.to_string_lossy())),
            InjectionConfig::Triggers { .. } => self.metadata.injection.matches_content(content),
            InjectionConfig::Combined { .. } => {
                let path_match = files
                    .iter()
                    .any(|f| self.metadata.injection.matches_path(&f.to_string_lossy()));
                let content_match = self.metadata.injection.matches_content(content);
                path_match && content_match
            }
        }
    }
}

/// Collection of resolved rules ready for injection.
#[derive(Debug, Clone, Default)]
pub struct ResolvedRules {
    rules: Vec<Rule>,
}

impl ResolvedRules {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rules(mut self, rules: Vec<Rule>) -> Self {
        self.rules = rules;
        self.sort_by_priority();
        self
    }

    pub fn add(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    /// Add a rule only if no rule with the same name exists.
    /// Returns true if the rule was added, false if a duplicate was skipped.
    pub fn add_unique(&mut self, rule: Rule) -> bool {
        if self.rules.iter().any(|r| r.name() == rule.name()) {
            false
        } else {
            self.rules.push(rule);
            true
        }
    }

    pub fn extend(&mut self, rules: impl IntoIterator<Item = Rule>) {
        self.rules.extend(rules);
    }

    /// Sort rules by priority (highest first).
    pub fn sort_by_priority(&mut self) {
        self.rules.sort_by_key(|r| std::cmp::Reverse(r.priority()));
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn into_rules(self) -> Vec<Rule> {
        self.rules
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Get rules by category.
    pub fn by_category(&self, category: RuleCategory) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(move |r| r.category() == category)
    }

    /// Build a combined prompt from all rules.
    pub fn to_prompt(&self) -> String {
        if self.rules.is_empty() {
            return String::new();
        }

        let mut prompt = String::with_capacity(self.rules.iter().map(|r| r.content.len()).sum());
        prompt.push_str("# Applicable Rules\n\n");

        for rule in &self.rules {
            prompt.push_str(&format!(
                "## {} (priority {})\n",
                rule.name(),
                rule.priority()
            ));
            prompt.push_str(&rule.content);
            prompt.push_str("\n\n");
        }

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_category_priority() {
        assert_eq!(RuleCategory::Project.priority(), 100);
        assert_eq!(RuleCategory::Tech.priority(), 90);
        assert_eq!(RuleCategory::Framework.priority(), 85);
        assert_eq!(RuleCategory::Module.priority(), 80);
        assert_eq!(RuleCategory::Group.priority(), 70);
        assert_eq!(RuleCategory::Domain.priority(), 60);
    }

    #[test]
    fn test_injection_config_always() {
        let config = InjectionConfig::always();
        assert!(config.matches_path("any/path.rs"));
        assert!(config.matches_content("any content"));
    }

    #[test]
    fn test_injection_config_paths() {
        let config = InjectionConfig::paths(vec!["**/*.rs".into(), "src/auth/**".into()]);
        assert!(config.matches_path("src/main.rs"));
        assert!(config.matches_path("src/auth/login.rs"));
        assert!(!config.matches_path("README.md"));
    }

    #[test]
    fn test_injection_config_triggers() {
        let config = InjectionConfig::triggers(vec!["async".into(), "spawn".into()]);
        assert!(config.matches_content("use async functions"));
        assert!(config.matches_content("tokio::spawn"));
        assert!(!config.matches_content("regular sync code"));
    }

    #[test]
    fn test_injection_config_combined() {
        let config = InjectionConfig::combined(vec!["**/*.rs".into()], vec!["async".into()]);
        assert!(config.matches_path("src/main.rs"));
        assert!(config.matches_content("async fn"));
    }

    #[test]
    fn test_rule_applies_to() {
        let rule = Rule::new(
            RuleMetadata::new("rust", RuleCategory::Tech)
                .with_injection(InjectionConfig::paths(vec!["**/*.rs".into()])),
            "Rust conventions".into(),
            PathBuf::from("rules/tech/rust.md"),
        );

        assert!(rule.applies_to(&[PathBuf::from("src/main.rs")], ""));
        assert!(!rule.applies_to(&[PathBuf::from("package.json")], ""));
    }

    #[test]
    fn test_resolved_rules_sorting() {
        let mut resolved = ResolvedRules::new();
        resolved.add(Rule::new(
            RuleMetadata::new("domain", RuleCategory::Domain),
            "domain".into(),
            PathBuf::new(),
        ));
        resolved.add(Rule::new(
            RuleMetadata::new("project", RuleCategory::Project),
            "project".into(),
            PathBuf::new(),
        ));
        resolved.add(Rule::new(
            RuleMetadata::new("tech", RuleCategory::Tech),
            "tech".into(),
            PathBuf::new(),
        ));

        resolved.sort_by_priority();

        let names: Vec<_> = resolved.rules().iter().map(|r| r.name()).collect();
        assert_eq!(names, vec!["project", "tech", "domain"]);
    }

    #[test]
    fn test_resolved_rules_to_prompt() {
        let resolved = ResolvedRules::new().with_rules(vec![Rule::new(
            RuleMetadata::new("project", RuleCategory::Project),
            "Project rules".into(),
            PathBuf::new(),
        )]);

        let prompt = resolved.to_prompt();
        assert!(prompt.contains("# Applicable Rules"));
        assert!(prompt.contains("## project (priority 100)"));
        assert!(prompt.contains("Project rules"));
    }

    #[test]
    fn test_add_unique() {
        let mut resolved = ResolvedRules::new();
        let rule1 = Rule::new(
            RuleMetadata::new("rust", RuleCategory::Tech),
            "Rust content".into(),
            PathBuf::new(),
        );
        let rule2 = Rule::new(
            RuleMetadata::new("rust", RuleCategory::Tech),
            "Duplicate rust".into(),
            PathBuf::new(),
        );
        let rule3 = Rule::new(
            RuleMetadata::new("python", RuleCategory::Tech),
            "Python content".into(),
            PathBuf::new(),
        );

        assert!(resolved.add_unique(rule1));
        assert!(!resolved.add_unique(rule2)); // Same name, should skip
        assert!(resolved.add_unique(rule3)); // Different name, should add

        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved.rules()[0].name(), "rust");
        assert_eq!(resolved.rules()[1].name(), "python");
    }
}
