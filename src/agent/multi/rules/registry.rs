//! Rule registry for loading rules from the filesystem.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::fs;
use tracing::debug;

use super::types::{InjectionConfig, Rule, RuleCategory, RuleMetadata};
use crate::error::Result;

/// Registry for loading and caching rules from the filesystem.
///
/// Expected directory structure:
/// ```text
/// .claude/rules/
/// ├── project.md               # Always injected
/// ├── tech/
/// │   ├── rust.md              # Injected for *.rs files
/// │   └── typescript.md        # Injected for *.ts, *.tsx files
/// ├── frameworks/
/// │   └── tokio.md             # Injected by path + triggers
/// ├── modules/
/// │   └── auth.md              # Injected for src/auth/** paths
/// ├── groups/
/// │   └── core.md              # Injected for module group members
/// └── domains/
///     ├── security.md          # Injected by keyword triggers
///     └── concurrency.md       # Injected by keyword triggers
/// ```
pub struct RuleRegistry {
    rules: HashMap<String, Rule>,
    rules_dir: PathBuf,
}

impl RuleRegistry {
    pub fn new(rules_dir: PathBuf) -> Self {
        Self {
            rules: HashMap::new(),
            rules_dir,
        }
    }

    /// Load all rules from the rules directory.
    pub async fn load(&mut self) -> Result<()> {
        if !self.rules_dir.exists() {
            debug!(path = %self.rules_dir.display(), "Rules directory not found, skipping");
            return Ok(());
        }

        self.load_project_rules().await?;

        for category in [
            RuleCategory::Tech,
            RuleCategory::Framework,
            RuleCategory::Module,
            RuleCategory::Group,
            RuleCategory::Domain,
        ] {
            self.load_category_rules(category).await?;
        }

        debug!(count = self.rules.len(), "Loaded rules");
        Ok(())
    }

    /// Load project-level rules (always injected).
    async fn load_project_rules(&mut self) -> Result<()> {
        let project_rule = self.rules_dir.join("project.md");
        if project_rule.exists()
            && let Some(rule) = self
                .load_rule_file(&project_rule, RuleCategory::Project)
                .await?
        {
            self.rules.insert(rule.name().to_string(), rule);
        }
        Ok(())
    }

    /// Load rules from a category subdirectory.
    async fn load_category_rules(&mut self, category: RuleCategory) -> Result<()> {
        let dir_name = category.directory_name();
        if dir_name.is_empty() {
            return Ok(());
        }

        let category_dir = self.rules_dir.join(dir_name);
        if !category_dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(&category_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && let Some(rule) = self.load_rule_file(&path, category).await?
            {
                self.rules.insert(rule.name().to_string(), rule);
            }
        }

        Ok(())
    }

    /// Load a single rule file.
    async fn load_rule_file(&self, path: &Path, category: RuleCategory) -> Result<Option<Rule>> {
        let content = fs::read_to_string(path).await?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let (metadata, rule_content) = self.parse_rule_content(&name, category, &content);

        Ok(Some(Rule::new(metadata, rule_content, path.to_path_buf())))
    }

    /// Parse rule content, extracting frontmatter metadata if present.
    fn parse_rule_content(
        &self,
        name: &str,
        category: RuleCategory,
        content: &str,
    ) -> (RuleMetadata, String) {
        if let Some(rest) = content.strip_prefix("---\n")
            && let Some(end) = rest.find("\n---\n")
        {
            let frontmatter = &rest[..end];
            let rule_content = rest[end + 5..].trim().to_string();

            if let Some(metadata) = self.parse_frontmatter(name, category, frontmatter) {
                return (metadata, rule_content);
            }
        }

        let metadata = RuleMetadata::new(name, category)
            .with_injection(self.infer_injection_config(name, category));

        (metadata, content.to_string())
    }

    /// Parse YAML frontmatter for rule metadata.
    fn parse_frontmatter(
        &self,
        name: &str,
        category: RuleCategory,
        frontmatter: &str,
    ) -> Option<RuleMetadata> {
        #[derive(serde::Deserialize)]
        struct Frontmatter {
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            paths: Option<Vec<String>>,
            #[serde(default)]
            triggers: Option<Vec<String>>,
        }

        let fm: Frontmatter = serde_yaml_bw::from_str(frontmatter).ok()?;

        let injection = match (&fm.paths, &fm.triggers) {
            (Some(paths), Some(triggers)) => {
                InjectionConfig::combined(paths.clone(), triggers.clone())
            }
            (Some(paths), None) => InjectionConfig::paths(paths.clone()),
            (None, Some(triggers)) => InjectionConfig::triggers(triggers.clone()),
            (None, None) => self.infer_injection_config(name, category),
        };

        let mut metadata = RuleMetadata::new(name, category).with_injection(injection);
        if let Some(desc) = fm.description {
            metadata = metadata.with_description(desc);
        }

        Some(metadata)
    }

    /// Infer injection config from rule name and category.
    fn infer_injection_config(&self, name: &str, category: RuleCategory) -> InjectionConfig {
        match category {
            RuleCategory::Project => InjectionConfig::always(),
            RuleCategory::Tech => {
                let patterns = self.language_to_patterns(name);
                InjectionConfig::paths(patterns)
            }
            RuleCategory::Module => {
                InjectionConfig::paths(vec![format!("src/{}/**", name), format!("{}/**", name)])
            }
            RuleCategory::Domain => {
                let keywords = self.domain_to_keywords(name);
                InjectionConfig::triggers(keywords)
            }
            RuleCategory::Framework | RuleCategory::Group => InjectionConfig::always(),
        }
    }

    /// Map language name to file patterns.
    fn language_to_patterns(&self, language: &str) -> Vec<String> {
        match language.to_lowercase().as_str() {
            "rust" => vec!["**/*.rs".into()],
            "typescript" => vec!["**/*.ts".into(), "**/*.tsx".into()],
            "javascript" => vec!["**/*.js".into(), "**/*.jsx".into(), "**/*.mjs".into()],
            "python" => vec!["**/*.py".into()],
            "go" => vec!["**/*.go".into()],
            "java" => vec!["**/*.java".into()],
            "kotlin" => vec!["**/*.kt".into(), "**/*.kts".into()],
            "swift" => vec!["**/*.swift".into()],
            "ruby" => vec!["**/*.rb".into()],
            "php" => vec!["**/*.php".into()],
            "c" => vec!["**/*.c".into(), "**/*.h".into()],
            "cpp" | "c++" => vec![
                "**/*.cpp".into(),
                "**/*.hpp".into(),
                "**/*.cc".into(),
                "**/*.hh".into(),
            ],
            "csharp" | "c#" => vec!["**/*.cs".into()],
            "scala" => vec!["**/*.scala".into()],
            _ => vec![format!("**/*.{}", language)],
        }
    }

    /// Map domain name to trigger keywords.
    fn domain_to_keywords(&self, domain: &str) -> Vec<String> {
        match domain.to_lowercase().as_str() {
            "security" => vec![
                "password".into(),
                "token".into(),
                "auth".into(),
                "secret".into(),
                "credential".into(),
                "encrypt".into(),
                "hash".into(),
            ],
            "concurrency" => vec![
                "async".into(),
                "mutex".into(),
                "channel".into(),
                "spawn".into(),
                "thread".into(),
                "atomic".into(),
                "lock".into(),
            ],
            "error-handling" | "errors" => vec![
                "error".into(),
                "Result".into(),
                "unwrap".into(),
                "panic".into(),
                "expect".into(),
                "?".into(),
            ],
            "testing" => vec![
                "test".into(),
                "#[test]".into(),
                "assert".into(),
                "mock".into(),
                "fixture".into(),
            ],
            "performance" => vec![
                "optimize".into(),
                "cache".into(),
                "benchmark".into(),
                "profil".into(),
                "latency".into(),
            ],
            _ => vec![domain.to_string()],
        }
    }

    /// Get all loaded rules.
    pub fn all_rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.values()
    }

    /// Get rule by name.
    pub fn get(&self, name: &str) -> Option<&Rule> {
        self.rules.get(name)
    }

    /// Get rules by category.
    pub fn by_category(&self, category: RuleCategory) -> impl Iterator<Item = &Rule> {
        self.rules
            .values()
            .filter(move |r| r.category() == category)
    }

    /// Get project rules (always injected).
    pub fn project_rules(&self) -> impl Iterator<Item = &Rule> {
        self.by_category(RuleCategory::Project)
    }

    /// Get tech rules for a file extension.
    pub fn tech_rules_for_extension(&self, ext: &str) -> Vec<&Rule> {
        let path = format!("file.{}", ext);
        self.by_category(RuleCategory::Tech)
            .filter(|r| r.metadata.injection.matches_path(&path))
            .collect()
    }

    /// Get domain rules matching content.
    pub fn domain_rules_for_content(&self, content: &str) -> Vec<&Rule> {
        self.by_category(RuleCategory::Domain)
            .filter(|r| r.metadata.injection.matches_content(content))
            .collect()
    }

    /// Get module rules matching a path.
    pub fn module_rules_for_path(&self, path: &Path) -> Vec<&Rule> {
        let path_str = path.to_string_lossy();
        self.by_category(RuleCategory::Module)
            .filter(|r| r.metadata.injection.matches_path(&path_str))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_test_rules() -> (TempDir, RuleRegistry) {
        let temp = TempDir::new().unwrap();
        let rules_dir = temp.path().join("rules");
        fs::create_dir_all(&rules_dir).await.unwrap();

        let registry = RuleRegistry::new(rules_dir);
        (temp, registry)
    }

    #[tokio::test]
    async fn test_empty_registry() {
        let (_temp, mut registry) = setup_test_rules().await;
        registry.load().await.unwrap();
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn test_load_project_rule() {
        let (temp, mut registry) = setup_test_rules().await;
        let rules_dir = temp.path().join("rules");

        fs::write(
            rules_dir.join("project.md"),
            "# Project Rules\n\nAlways apply.",
        )
        .await
        .unwrap();

        registry.load().await.unwrap();
        assert_eq!(registry.len(), 1);

        let project_rules: Vec<_> = registry.project_rules().collect();
        assert_eq!(project_rules.len(), 1);
        assert_eq!(project_rules[0].category(), RuleCategory::Project);
    }

    #[tokio::test]
    async fn test_load_tech_rules() {
        let (temp, mut registry) = setup_test_rules().await;
        let rules_dir = temp.path().join("rules");

        let tech_dir = rules_dir.join("tech");
        fs::create_dir_all(&tech_dir).await.unwrap();
        fs::write(tech_dir.join("rust.md"), "# Rust Rules")
            .await
            .unwrap();

        registry.load().await.unwrap();

        let rust_rules = registry.tech_rules_for_extension("rs");
        assert_eq!(rust_rules.len(), 1);
    }

    #[tokio::test]
    async fn test_load_with_frontmatter() {
        let (temp, mut registry) = setup_test_rules().await;
        let rules_dir = temp.path().join("rules");

        let domains_dir = rules_dir.join("domains");
        fs::create_dir_all(&domains_dir).await.unwrap();

        let content = r#"---
description: Security guidelines
triggers:
  - password
  - secret
---

# Security Rules

Always validate input.
"#;
        fs::write(domains_dir.join("security.md"), content)
            .await
            .unwrap();

        registry.load().await.unwrap();

        let rule = registry.get("security").unwrap();
        assert_eq!(
            rule.metadata.description.as_deref(),
            Some("Security guidelines")
        );
        assert!(rule.metadata.injection.matches_content("check password"));
    }

    #[test]
    fn test_language_to_patterns() {
        let registry = RuleRegistry::new(PathBuf::new());

        let rust_patterns = registry.language_to_patterns("rust");
        assert_eq!(rust_patterns, vec!["**/*.rs"]);

        let ts_patterns = registry.language_to_patterns("typescript");
        assert!(ts_patterns.contains(&"**/*.ts".to_string()));
        assert!(ts_patterns.contains(&"**/*.tsx".to_string()));
    }

    #[test]
    fn test_domain_to_keywords() {
        let registry = RuleRegistry::new(PathBuf::new());

        let security_keywords = registry.domain_to_keywords("security");
        assert!(security_keywords.contains(&"password".to_string()));
        assert!(security_keywords.contains(&"token".to_string()));

        let concurrency_keywords = registry.domain_to_keywords("concurrency");
        assert!(concurrency_keywords.contains(&"async".to_string()));
        assert!(concurrency_keywords.contains(&"mutex".to_string()));
    }
}
