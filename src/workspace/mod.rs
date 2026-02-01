//! Workspace management for manifest-first multi-project orchestration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use modmap::{
    DetectedLanguage, Domain, DomainContext, GroupContext, Module, ModuleContext, ModuleGroup,
    ModuleMap, ProjectManifest, WorkspaceType,
};

use crate::error::{PilotError, Result};

/// A workspace loaded from a claudegen manifest.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub manifest_path: PathBuf,
    pub root: PathBuf,
    pub manifest: ProjectManifest,
    pub name: String,
}

impl Workspace {
    /// Load workspace from an explicit manifest path.
    pub async fn from_manifest(path: &Path) -> Result<Self> {
        let manifest_path = tokio::fs::canonicalize(path).await?;

        let root = manifest_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| PilotError::Config("Invalid manifest path structure".into()))?
            .to_path_buf();

        let content = tokio::fs::read_to_string(&manifest_path).await?;

        let manifest: ProjectManifest = serde_json::from_str(&content)
            .map_err(|e| PilotError::Config(format!("Invalid manifest JSON: {}", e)))?;

        let name = manifest.project.project.name.clone();

        Ok(Self {
            manifest_path,
            root,
            manifest,
            name,
        })
    }

    #[inline]
    pub fn project_map(&self) -> &ModuleMap {
        &self.manifest.project
    }

    #[inline]
    pub fn modules(&self) -> &[Module] {
        &self.manifest.project.modules
    }

    #[inline]
    pub fn groups(&self) -> &[ModuleGroup] {
        &self.manifest.project.groups
    }

    #[inline]
    pub fn domains(&self) -> &[Domain] {
        &self.manifest.project.domains
    }

    #[inline]
    pub fn languages(&self) -> &[DetectedLanguage] {
        &self.manifest.project.project.languages
    }

    #[inline]
    pub fn workspace_type(&self) -> &WorkspaceType {
        &self.manifest.project.project.workspace.workspace_type
    }

    #[inline]
    pub fn module_context(&self, module_id: &str) -> Option<&ModuleContext> {
        self.manifest.modules.get(module_id)
    }

    #[inline]
    pub fn group_context(&self, group_id: &str) -> Option<&GroupContext> {
        self.manifest.groups.get(group_id)
    }

    #[inline]
    pub fn domain_context(&self, domain_id: &str) -> Option<&DomainContext> {
        self.manifest.domains.get(domain_id)
    }

    #[inline]
    pub fn module_contexts(&self) -> &HashMap<String, ModuleContext> {
        &self.manifest.modules
    }

    pub fn find_module_for_file(&self, path: &Path) -> Option<&Module> {
        let rel_path = path.strip_prefix(&self.root).ok()?;
        let rel_str = rel_path.to_string_lossy();
        self.modules().iter().find(|m| m.contains_file(&rel_str))
    }

    pub fn find_group_for_module(&self, module_id: &str) -> Option<&ModuleGroup> {
        self.project_map().find_group_containing(module_id)
    }

    pub fn find_domain_for_group(&self, group_id: &str) -> Option<&Domain> {
        self.project_map().find_domain_containing_group(group_id)
    }
}

/// Collection of workspaces for multi-project orchestration.
#[derive(Debug)]
pub struct WorkspaceSet {
    workspaces: HashMap<String, Arc<Workspace>>,
    order: Vec<String>,
}

impl WorkspaceSet {
    /// Create from explicit manifest paths.
    pub async fn from_manifests(paths: &[PathBuf]) -> Result<Self> {
        if paths.is_empty() {
            return Err(PilotError::Config("No manifests provided".into()));
        }

        let mut workspaces = HashMap::new();
        let mut order = Vec::with_capacity(paths.len());

        for path in paths {
            let ws = Workspace::from_manifest(path).await?;
            let name = ws.name.clone();
            order.push(name.clone());
            workspaces.insert(name, Arc::new(ws));
        }

        Ok(Self { workspaces, order })
    }

    #[inline]
    pub fn get(&self, name: &str) -> Option<&Arc<Workspace>> {
        self.workspaces.get(name)
    }

    #[inline]
    pub fn first(&self) -> Option<&Arc<Workspace>> {
        self.order
            .first()
            .and_then(|name| self.workspaces.get(name))
    }

    pub fn workspace_for_file(&self, file: &Path) -> Option<&Arc<Workspace>> {
        self.workspaces
            .values()
            .find(|ws| file.starts_with(&ws.root))
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Workspace>> {
        self.order
            .iter()
            .filter_map(|name| self.workspaces.get(name))
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.workspaces.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
    }

    pub fn names(&self) -> &[String] {
        &self.order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_manifest(dir: &Path, name: &str) -> PathBuf {
        let claudegen_dir = dir.join(".claudegen");
        std::fs::create_dir_all(&claudegen_dir).unwrap();

        let manifest_path = claudegen_dir.join("manifest.json");
        let manifest_content = format!(
            r#"{{
                "version": "1.0.0",
                "created_at": "2026-01-30T00:00:00Z",
                "generator": "test",
                "project": {{
                    "schema_version": "1.0.0",
                    "generator": {{ "name": "test", "version": "1.0.0" }},
                    "project": {{
                        "name": "{}",
                        "workspace": {{ "workspace_type": "single_package" }},
                        "tech_stack": {{ "primary_language": "rust" }},
                        "languages": [],
                        "total_files": 0
                    }},
                    "modules": [],
                    "generated_at": "2026-01-30T00:00:00Z"
                }}
            }}"#,
            name
        );

        let mut file = std::fs::File::create(&manifest_path).unwrap();
        file.write_all(manifest_content.as_bytes()).unwrap();

        manifest_path
    }

    #[tokio::test]
    async fn test_workspace_from_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let manifest_path = create_test_manifest(temp_dir.path(), "test-project");

        let ws = Workspace::from_manifest(&manifest_path).await.unwrap();

        assert_eq!(ws.name, "test-project");
        // Canonicalize temp_dir path for symlink resolution (macOS /var -> /private/var)
        let expected_root = std::fs::canonicalize(temp_dir.path()).unwrap();
        assert_eq!(ws.root, expected_root);
    }

    #[tokio::test]
    async fn test_workspace_set_from_manifests() {
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();

        let manifest1 = create_test_manifest(temp_dir1.path(), "frontend");
        let manifest2 = create_test_manifest(temp_dir2.path(), "backend");

        let ws_set = WorkspaceSet::from_manifests(&[manifest1, manifest2])
            .await
            .unwrap();

        assert_eq!(ws_set.len(), 2);
        assert!(ws_set.get("frontend").is_some());
        assert!(ws_set.get("backend").is_some());
        assert_eq!(ws_set.first().unwrap().name, "frontend");
    }

    #[tokio::test]
    async fn test_workspace_set_order_preserved() {
        let temp_dir1 = TempDir::new().unwrap();
        let temp_dir2 = TempDir::new().unwrap();

        let manifest1 = create_test_manifest(temp_dir1.path(), "second");
        let manifest2 = create_test_manifest(temp_dir2.path(), "first");

        let ws_set = WorkspaceSet::from_manifests(&[manifest1, manifest2])
            .await
            .unwrap();

        let names: Vec<_> = ws_set.iter().map(|ws| ws.name.as_str()).collect();
        assert_eq!(names, vec!["second", "first"]);
    }

    #[tokio::test]
    async fn test_empty_manifests_error() {
        let result = WorkspaceSet::from_manifests(&[]).await;
        assert!(result.is_err());
    }
}
