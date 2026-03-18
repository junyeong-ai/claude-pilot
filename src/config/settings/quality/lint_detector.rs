use std::path::Path;

use super::build_system::BuildSystem;

/// Lint tool detection from build configuration files.
pub struct LintDetector;

impl LintDetector {
    pub fn detect(working_dir: &Path, build_system: &BuildSystem) -> Option<String> {
        match build_system {
            BuildSystem::Gradle => Self::detect_gradle_lint(working_dir),
            BuildSystem::Maven => Self::detect_maven_lint(working_dir),
            BuildSystem::Poetry | BuildSystem::Pip => Self::detect_python_lint(working_dir),
            BuildSystem::Go => Self::detect_go_lint(working_dir),
            BuildSystem::Npm | BuildSystem::Yarn | BuildSystem::Pnpm => {
                Self::detect_node_lint(working_dir, build_system)
            }
            BuildSystem::Dotnet => Self::detect_dotnet_lint(working_dir),
            _ => None,
        }
    }

    fn detect_gradle_lint(working_dir: &Path) -> Option<String> {
        for gradle_file in ["build.gradle.kts", "build.gradle"] {
            let path = working_dir.join(gradle_file);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let content_lower = content.to_lowercase();

                if content_lower.contains("detekt") {
                    return Some("./gradlew detekt".into());
                }
                if content_lower.contains("ktlint") {
                    return Some("./gradlew ktlintCheck".into());
                }
                if content_lower.contains("checkstyle") {
                    return Some("./gradlew checkstyleMain checkstyleTest".into());
                }
                if content_lower.contains("spotbugs") {
                    return Some("./gradlew spotbugsMain".into());
                }
                if content_lower.contains("pmd") {
                    return Some("./gradlew pmdMain".into());
                }
            }
        }
        None
    }

    fn detect_maven_lint(working_dir: &Path) -> Option<String> {
        let pom_path = working_dir.join("pom.xml");
        if let Ok(content) = std::fs::read_to_string(pom_path) {
            let content_lower = content.to_lowercase();

            if content_lower.contains("checkstyle") {
                return Some("mvn checkstyle:check -q".into());
            }
            if content_lower.contains("spotbugs") {
                return Some("mvn spotbugs:check -q".into());
            }
            if content_lower.contains("pmd") {
                return Some("mvn pmd:check -q".into());
            }
        }
        None
    }

    fn detect_python_lint(working_dir: &Path) -> Option<String> {
        let pyproject_path = working_dir.join("pyproject.toml");
        if let Ok(content) = std::fs::read_to_string(&pyproject_path) {
            let content_lower = content.to_lowercase();

            if content_lower.contains("[tool.ruff]") || content_lower.contains("ruff") {
                return Some("ruff check .".into());
            }
            if content_lower.contains("[tool.flake8]") {
                return Some("flake8 .".into());
            }
            if content_lower.contains("[tool.pylint]") {
                return Some("pylint .".into());
            }
            if content_lower.contains("[tool.black]") {
                return Some("black --check .".into());
            }
            if content_lower.contains("[tool.mypy]") {
                return Some("mypy .".into());
            }
        }

        let setup_cfg = working_dir.join("setup.cfg");
        if let Ok(content) = std::fs::read_to_string(&setup_cfg) {
            let content_lower = content.to_lowercase();
            if content_lower.contains("[flake8]") {
                return Some("flake8 .".into());
            }
            if content_lower.contains("[mypy]") {
                return Some("mypy .".into());
            }
        }

        if let Some(cmd) = Self::detect_from_precommit(working_dir) {
            return Some(cmd);
        }

        None
    }

    fn detect_go_lint(working_dir: &Path) -> Option<String> {
        for config in [".golangci.yml", ".golangci.yaml", ".golangci.toml"] {
            if working_dir.join(config).exists() {
                return Some("golangci-lint run".into());
            }
        }

        if working_dir.join("staticcheck.conf").exists() {
            return Some("staticcheck ./...".into());
        }

        if let Some(cmd) = Self::detect_from_makefile(working_dir, "lint") {
            return Some(cmd);
        }

        None
    }

    fn detect_node_lint(working_dir: &Path, build_system: &BuildSystem) -> Option<String> {
        let pkg_path = working_dir.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if content.contains("\"lint\"") || content.contains("\"lint:") {
                return None;
            }

            let content_lower = content.to_lowercase();

            if content_lower.contains("\"eslint\"") {
                let runner = match build_system {
                    BuildSystem::Pnpm => "pnpm",
                    BuildSystem::Yarn => "yarn",
                    _ => "npx",
                };
                return Some(format!("{} eslint .", runner));
            }

            if content_lower.contains("\"biome\"") {
                let runner = match build_system {
                    BuildSystem::Pnpm => "pnpm",
                    BuildSystem::Yarn => "yarn",
                    _ => "npx",
                };
                return Some(format!("{} biome check .", runner));
            }
        }

        for config in [
            ".eslintrc",
            ".eslintrc.js",
            ".eslintrc.json",
            ".eslintrc.yml",
            "eslint.config.js",
            "eslint.config.mjs",
        ] {
            if working_dir.join(config).exists() {
                let runner = match build_system {
                    BuildSystem::Pnpm => "pnpm",
                    BuildSystem::Yarn => "yarn",
                    _ => "npx",
                };
                return Some(format!("{} eslint .", runner));
            }
        }

        None
    }

    fn detect_dotnet_lint(working_dir: &Path) -> Option<String> {
        if working_dir.join(".editorconfig").exists() {
            return Some("dotnet format --verify-no-changes".into());
        }

        if Self::has_file_with_extension(working_dir, ".csproj") {
            return Some("dotnet format --verify-no-changes".into());
        }

        None
    }

    fn detect_from_precommit(working_dir: &Path) -> Option<String> {
        let precommit_path = working_dir.join(".pre-commit-config.yaml");
        if let Ok(content) = std::fs::read_to_string(&precommit_path) {
            let content_lower = content.to_lowercase();

            if content_lower.contains("ruff") {
                return Some("ruff check .".into());
            }
            if content_lower.contains("flake8") {
                return Some("flake8 .".into());
            }
            if content_lower.contains("eslint") {
                return Some("npx eslint .".into());
            }
            if content_lower.contains("golangci-lint") {
                return Some("golangci-lint run".into());
            }
        }
        None
    }

    fn detect_from_makefile(working_dir: &Path, target: &str) -> Option<String> {
        let makefile_path = working_dir.join("Makefile");
        if let Ok(content) = std::fs::read_to_string(&makefile_path) {
            let target_pattern = format!("{}:", target);
            if content.contains(&target_pattern) {
                return Some(format!("make {}", target));
            }
        }
        None
    }

    fn has_file_with_extension(dir: &Path, ext: &str) -> bool {
        std::fs::read_dir(dir).ok().is_some_and(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().to_string_lossy().ends_with(ext))
        })
    }
}
