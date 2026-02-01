//! Project manifest loading for module-based agent coordination.

use std::collections::HashMap;
use std::path::Path;

use modmap::{
    DetectedLanguage, Domain, Module, ModuleContext, ModuleGroup, SchemaError, SchemaRegistry,
    WorkspaceType,
};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tracing::{debug, info};

use crate::error::{PilotError, Result};

const DEFAULT_MANIFEST_PATH: &str = ".claudegen/manifest.json";
const MIN_FILES_FOR_AGENT: usize = 3;
const MIN_VALUE_SCORE: f64 = 0.3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMap {
    pub modules: Vec<Module>,
    pub languages: Vec<DetectedLanguage>,
    pub workspace_type: Option<WorkspaceType>,
    #[serde(default)]
    pub module_context: HashMap<String, ModuleContext>,
    #[serde(default)]
    pub groups: Vec<ModuleGroup>,
    #[serde(default)]
    pub domains: Vec<Domain>,
}

pub struct ManifestLoader {
    schema_registry: SchemaRegistry,
}

impl Default for ManifestLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ManifestLoader {
    pub fn new() -> Self {
        Self {
            schema_registry: SchemaRegistry::new(),
        }
    }

    fn is_significant_module(module: &Module) -> bool {
        module.paths.len() >= MIN_FILES_FOR_AGENT && module.metrics.value_score >= MIN_VALUE_SCORE
    }

    pub async fn load(
        &self,
        working_dir: &Path,
        manifest_path: Option<&Path>,
    ) -> Result<ProjectMap> {
        if let Some(path) = manifest_path {
            return self.load_manifest(path).await;
        }

        let default_path = working_dir.join(DEFAULT_MANIFEST_PATH);
        if default_path.exists() {
            return self.load_manifest(&default_path).await;
        }

        Err(PilotError::Config(format!(
            "No manifest found. Create one at {} or provide an explicit path.",
            DEFAULT_MANIFEST_PATH
        )))
    }

    async fn load_manifest(&self, path: &Path) -> Result<ProjectMap> {
        if !path.exists() {
            return Err(PilotError::Config(format!(
                "Manifest not found: {}",
                path.display()
            )));
        }

        let content = fs::read_to_string(path).await?;

        match self.schema_registry.load(&content) {
            Ok(manifest) => {
                let module_map = &manifest.project;

                let modules: Vec<Module> = module_map
                    .modules
                    .iter()
                    .filter(|m| Self::is_significant_module(m))
                    .cloned()
                    .collect();

                let languages = if module_map.project.languages.is_empty() {
                    Self::detect_languages(path.parent().unwrap_or(Path::new(".")))
                } else {
                    module_map.project.languages.clone()
                };

                info!(
                    version = %module_map.schema_version,
                    modules = modules.len(),
                    module_context = manifest.modules.len(),
                    "Loaded manifest"
                );

                Ok(ProjectMap {
                    modules,
                    languages,
                    workspace_type: Some(module_map.project.workspace.workspace_type.clone()),
                    module_context: manifest.modules.clone(),
                    groups: module_map.groups.clone(),
                    domains: module_map.domains.clone(),
                })
            }
            Err(SchemaError::IncompatibleVersion {
                found,
                required_major,
            }) => Err(PilotError::Config(format!(
                "Manifest version {} is incompatible (requires major version {})",
                found, required_major
            ))),
            Err(e) => {
                debug!(path = %path.display(), error = %e, "Failed to parse manifest");
                Err(PilotError::Config(format!(
                    "Failed to parse manifest: {}",
                    e
                )))
            }
        }
    }

    fn detect_languages(working_dir: &Path) -> Vec<DetectedLanguage> {
        let markers = [
            ("Rust", &["Cargo.toml"][..]),
            ("TypeScript", &["tsconfig.json", "package.json"]),
            (
                "Python",
                &["pyproject.toml", "setup.py", "requirements.txt"],
            ),
            ("Go", &["go.mod"]),
            ("Java", &["pom.xml", "build.gradle", "build.gradle.kts"]),
            ("Ruby", &["Gemfile"]),
            ("PHP", &["composer.json"]),
            ("C/C++", &["CMakeLists.txt", "Makefile"]),
        ];

        markers
            .iter()
            .filter_map(|(lang, files)| {
                let found: Vec<String> = files
                    .iter()
                    .filter(|f| working_dir.join(f).exists())
                    .map(|f| f.to_string())
                    .collect();

                if found.is_empty() {
                    None
                } else {
                    Some(DetectedLanguage::new(*lang).with_marker_files(found))
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_load_missing_manifest() {
        let loader = ManifestLoader::new();
        let temp_dir = std::env::temp_dir();

        let result = loader.load(&temp_dir, None).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_significance_filter() {
        use modmap::ModuleMetrics;

        let significant = Module {
            id: "api".to_string(),
            name: "api".to_string(),
            paths: vec!["a".into(), "b".into(), "c".into()],
            key_files: vec![],
            dependencies: vec![],
            dependents: vec![],
            responsibility: "API".to_string(),
            primary_language: "Rust".to_string(),
            metrics: ModuleMetrics::new(0.5, 0.5, 0.5),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        };

        let insignificant = Module {
            id: "util".to_string(),
            name: "util".to_string(),
            paths: vec!["x".into()],
            key_files: vec![],
            dependencies: vec![],
            dependents: vec![],
            responsibility: "Utils".to_string(),
            primary_language: "Rust".to_string(),
            metrics: ModuleMetrics::new(0.1, 0.1, 0.1),
            conventions: vec![],
            known_issues: vec![],
            evidence: vec![],
        };

        assert!(ManifestLoader::is_significant_module(&significant));
        assert!(!ManifestLoader::is_significant_module(&insignificant));
    }
}
