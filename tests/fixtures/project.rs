//! Test project fixtures for creating temporary project structures.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorType {
    TypeMismatch,
}

impl ErrorType {
    pub fn rust_code(&self) -> &'static str {
        match self {
            ErrorType::TypeMismatch => "let x: i32 = \"not an int\";",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestFile {
    pub path: PathBuf,
    pub content: String,
}

impl TestFile {
    pub fn new(path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }

    pub fn with_error(path: impl Into<PathBuf>, error: ErrorType) -> Self {
        Self {
            path: path.into(),
            content: error.rust_code().to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestDomain {
    pub name: String,
    pub path: PathBuf,
    pub files: Vec<TestFile>,
    pub language: String,
    pub dependencies: Vec<String>,
}

impl TestDomain {
    pub fn new(name: impl Into<String>, language: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            path: PathBuf::from(format!("src/{}", &name)),
            name,
            files: Vec::new(),
            language: language.into(),
            dependencies: Vec::new(),
        }
    }

    pub fn with_file(mut self, file: TestFile) -> Self {
        self.files.push(file);
        self
    }

    pub fn with_dependency(mut self, dep: impl Into<String>) -> Self {
        self.dependencies.push(dep.into());
        self
    }
}

pub struct TestProjectFixture {
    pub root: TempDir,
    pub domains: Vec<TestDomain>,
    pub files: Vec<TestFile>,
    pub language: String,
    pub has_module_map: bool,
}

impl TestProjectFixture {
    pub fn rust_project() -> Self {
        TestProjectBuilder::new()
            .language("rust")
            .file(
                "Cargo.toml",
                r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
            )
            .file(
                "src/lib.rs",
                r#"pub fn hello() -> &'static str {
    "hello"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello() {
        assert_eq!(hello(), "hello");
    }
}
"#,
            )
            .build()
            .expect("Failed to create Rust project fixture")
    }

    pub fn multi_domain() -> Self {
        TestProjectBuilder::new()
            .language("rust")
            .file(
                "Cargo.toml",
                r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#,
            )
            .domain(
                TestDomain::new("auth", "rust")
                    .with_file(TestFile::new(
                        "src/auth/mod.rs",
                        "pub mod login;\npub mod session;",
                    ))
                    .with_file(TestFile::new(
                        "src/auth/login.rs",
                        "pub fn login(_user: &str, _pass: &str) -> bool { true }",
                    ))
                    .with_file(TestFile::new(
                        "src/auth/session.rs",
                        "pub struct Session { pub id: String }",
                    )),
            )
            .domain(
                TestDomain::new("api", "rust")
                    .with_file(TestFile::new(
                        "src/api/mod.rs",
                        "pub mod routes;\npub mod handlers;",
                    ))
                    .with_file(TestFile::new(
                        "src/api/routes.rs",
                        "pub fn setup_routes() {}",
                    ))
                    .with_file(TestFile::new(
                        "src/api/handlers.rs",
                        "pub async fn handle_request() {}",
                    ))
                    .with_dependency("auth"),
            )
            .domain(
                TestDomain::new("db", "rust")
                    .with_file(TestFile::new(
                        "src/db/mod.rs",
                        "pub mod connection;\npub mod queries;",
                    ))
                    .with_file(TestFile::new("src/db/connection.rs", "pub struct DbPool;"))
                    .with_file(TestFile::new(
                        "src/db/queries.rs",
                        "pub fn query(_sql: &str) {}",
                    )),
            )
            .file("src/lib.rs", "pub mod auth;\npub mod api;\npub mod db;")
            .build()
            .expect("Failed to create multi-domain project fixture")
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn with_module_map(mut self) -> Self {
        let module_map = self.generate_module_map();
        let map_path = self.root.path().join("module_map.json");
        let content =
            serde_json::to_string_pretty(&module_map).expect("Failed to serialize module_map");
        fs::write(&map_path, content).expect("Failed to write module_map.json");
        self.has_module_map = true;
        self
    }

    fn generate_module_map(&self) -> serde_json::Value {
        let modules: Vec<serde_json::Value> = self
            .domains
            .iter()
            .map(|d| {
                json!({
                    "id": d.name,
                    "name": d.name,
                    "paths": [d.path.to_string_lossy()],
                    "key_files": d.files.iter().map(|f| f.path.to_string_lossy().to_string()).collect::<Vec<_>>(),
                    "dependencies": d.dependencies.iter().map(|dep| json!({"module_id": dep})).collect::<Vec<_>>(),
                    "responsibility": format!("{} module", d.name),
                    "primary_language": d.language,
                    "coverage_ratio": 0.8,
                    "value_score": 0.7,
                    "risk_score": 0.2
                })
            })
            .collect();

        json!({
            "schema_version": "1.0.0",
            "generator": {"name": "test", "version": "1.0.0"},
            "project": {
                "name": "test-project",
                "workspace": {"workspace_type": "single_package"},
                "tech_stack": {"primary_language": self.language},
                "languages": [{"name": self.language, "marker_files": []}],
                "total_files": self.files.len() + self.domains.iter().map(|d| d.files.len()).sum::<usize>()
            },
            "modules": modules,
            "groups": [],
            "generated_at": "2025-01-01T00:00:00Z"
        })
    }
}

pub struct TestProjectBuilder {
    language: String,
    files: Vec<TestFile>,
    domains: Vec<TestDomain>,
}

impl TestProjectBuilder {
    pub fn new() -> Self {
        Self {
            language: "rust".to_string(),
            files: Vec::new(),
            domains: Vec::new(),
        }
    }

    pub fn language(mut self, lang: impl Into<String>) -> Self {
        self.language = lang.into();
        self
    }

    pub fn file(mut self, path: impl Into<String>, content: impl Into<String>) -> Self {
        self.files.push(TestFile::new(path.into(), content));
        self
    }

    pub fn file_with_error(mut self, path: impl Into<String>, error: ErrorType) -> Self {
        self.files.push(TestFile::with_error(path.into(), error));
        self
    }

    pub fn domain(mut self, domain: TestDomain) -> Self {
        self.domains.push(domain);
        self
    }

    pub fn build(self) -> std::io::Result<TestProjectFixture> {
        let root = TempDir::new()?;

        for domain in &self.domains {
            let domain_path = root.path().join(&domain.path);
            fs::create_dir_all(&domain_path)?;

            for file in &domain.files {
                let file_path = root.path().join(&file.path);
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&file_path, &file.content)?;
            }
        }

        for file in &self.files {
            let file_path = root.path().join(&file.path);
            if let Some(parent) = file_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&file_path, &file.content)?;
        }

        Ok(TestProjectFixture {
            root,
            domains: self.domains,
            files: self.files,
            language: self.language,
            has_module_map: false,
        })
    }
}

impl Default for TestProjectBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_project_fixture() {
        let fixture = TestProjectFixture::rust_project();

        assert!(fixture.path().join("Cargo.toml").exists());
        assert!(fixture.path().join("src/lib.rs").exists());
    }

    #[test]
    fn test_multi_domain_fixture() {
        let fixture = TestProjectFixture::multi_domain();

        assert!(fixture.path().join("src/auth/mod.rs").exists());
        assert!(fixture.path().join("src/api/mod.rs").exists());
        assert!(fixture.path().join("src/db/mod.rs").exists());
        assert_eq!(fixture.domains.len(), 3);
    }

    #[test]
    fn test_with_module_map() {
        let fixture = TestProjectFixture::multi_domain().with_module_map();

        assert!(fixture.has_module_map);
        assert!(fixture.path().join("module_map.json").exists());

        let content = fs::read_to_string(fixture.path().join("module_map.json")).unwrap();
        let map: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(map["modules"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn test_builder_with_error() {
        let fixture = TestProjectBuilder::new()
            .language("rust")
            .file("Cargo.toml", "[package]\nname = \"test\"")
            .file_with_error("src/broken.rs", ErrorType::TypeMismatch)
            .build()
            .unwrap();

        let content = fs::read_to_string(fixture.path().join("src/broken.rs")).unwrap();
        assert!(content.contains("let x: i32"));
    }
}
