use std::path::Path;

use super::verification::VerificationCommands;

/// Build system detection for auto-configuration
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSystem {
    Gradle,
    Maven,
    Npm,
    Yarn,
    Pnpm,
    Cargo,
    Make,
    Go,
    Poetry,
    Pip,
    Dotnet,
    /// Custom build system (e.g., Bazel, Buck, CMake, Nix, etc.)
    /// Commands must be configured manually via VerificationConfig.
    Custom(String),
}

impl BuildSystem {
    /// Detect build system from marker files in working directory.
    ///
    /// Returns known build systems for standard markers, or Custom for
    /// recognized but unconfigured systems (Bazel, Buck, CMake, etc.).
    pub fn detect(working_dir: &Path) -> Option<Self> {
        if working_dir.join("Cargo.toml").exists() {
            return Some(Self::Cargo);
        }
        if working_dir.join("pnpm-lock.yaml").exists() {
            return Some(Self::Pnpm);
        }
        if working_dir.join("yarn.lock").exists() {
            return Some(Self::Yarn);
        }
        if working_dir.join("package-lock.json").exists()
            || working_dir.join("package.json").exists()
        {
            return Some(Self::Npm);
        }
        for gradle_file in &[
            "build.gradle.kts",
            "build.gradle",
            "settings.gradle.kts",
            "settings.gradle",
        ] {
            if working_dir.join(gradle_file).exists() {
                return Some(Self::Gradle);
            }
        }
        if working_dir.join("pom.xml").exists() {
            return Some(Self::Maven);
        }
        if working_dir.join("go.mod").exists() {
            return Some(Self::Go);
        }
        if working_dir.join("pyproject.toml").exists() {
            return Some(Self::Poetry);
        }
        if working_dir.join("requirements.txt").exists() {
            return Some(Self::Pip);
        }
        if working_dir.join("Makefile").exists() {
            return Some(Self::Make);
        }
        if Self::has_file_with_extension(working_dir, ".csproj")
            || Self::has_file_with_extension(working_dir, ".sln")
        {
            return Some(Self::Dotnet);
        }

        Self::detect_custom(working_dir)
    }

    fn detect_custom(working_dir: &Path) -> Option<Self> {
        let custom_checks: &[(&str, &str)] = &[
            ("BUILD.bazel", "bazel"),
            ("WORKSPACE", "bazel"),
            ("WORKSPACE.bazel", "bazel"),
            ("BUCK", "buck"),
            (".buckconfig", "buck"),
            ("CMakeLists.txt", "cmake"),
            ("meson.build", "meson"),
            ("SConstruct", "scons"),
            ("build.sbt", "sbt"),
            ("flake.nix", "nix"),
            ("shell.nix", "nix"),
            ("default.nix", "nix"),
            ("dune", "dune"),
            ("dune-project", "dune"),
            ("mix.exs", "mix"),
            ("rebar.config", "rebar"),
            ("stack.yaml", "stack"),
            ("cabal.project", "cabal"),
        ];

        for (file, name) in custom_checks {
            if working_dir.join(file).exists() {
                return Some(Self::Custom(name.to_string()));
            }
        }

        if Self::has_file_with_extension(working_dir, ".cabal") {
            return Some(Self::Custom("cabal".to_string()));
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

    pub fn commands(&self) -> VerificationCommands {
        match self {
            Self::Gradle => VerificationCommands {
                build_cmd: Some("./gradlew build -x test".into()),
                test_cmd: Some("./gradlew test".into()),
                lint_cmd: None,
                test_filter_format: Some("--tests '{}'".into()),
            },
            Self::Maven => VerificationCommands {
                build_cmd: Some("mvn compile -q".into()),
                test_cmd: Some("mvn test -q".into()),
                lint_cmd: None,
                test_filter_format: Some("-Dtest={}".into()),
            },
            Self::Npm => VerificationCommands {
                build_cmd: Some("npm run build --if-present".into()),
                test_cmd: Some("npm test --if-present".into()),
                lint_cmd: Some("npm run lint --if-present".into()),
                test_filter_format: Some("-- --testPathPattern='{}'".into()),
            },
            Self::Yarn => VerificationCommands {
                build_cmd: Some("yarn build --if-present".into()),
                test_cmd: Some("yarn test --if-present".into()),
                lint_cmd: Some("yarn lint --if-present".into()),
                test_filter_format: Some("--testPathPattern='{}'".into()),
            },
            Self::Pnpm => VerificationCommands {
                build_cmd: Some("pnpm build --if-present".into()),
                test_cmd: Some("pnpm test --if-present".into()),
                lint_cmd: Some("pnpm lint --if-present".into()),
                test_filter_format: Some("--testPathPattern='{}'".into()),
            },
            Self::Cargo => VerificationCommands {
                build_cmd: Some("cargo build".into()),
                test_cmd: Some("cargo test".into()),
                lint_cmd: Some("cargo clippy -- -D warnings".into()),
                test_filter_format: Some("{}".into()),
            },
            Self::Go => VerificationCommands {
                build_cmd: Some("go build ./...".into()),
                test_cmd: Some("go test ./...".into()),
                lint_cmd: None,
                test_filter_format: Some("-run {}".into()),
            },
            Self::Poetry => VerificationCommands {
                build_cmd: None,
                test_cmd: Some("poetry run pytest".into()),
                lint_cmd: None,
                test_filter_format: Some("-k {}".into()),
            },
            Self::Pip => VerificationCommands {
                build_cmd: None,
                test_cmd: Some("pytest".into()),
                lint_cmd: None,
                test_filter_format: Some("-k {}".into()),
            },
            Self::Make => VerificationCommands {
                build_cmd: Some("make".into()),
                test_cmd: Some("make test".into()),
                lint_cmd: Some("make lint".into()),
                test_filter_format: None,
            },
            Self::Dotnet => VerificationCommands {
                build_cmd: Some("dotnet build".into()),
                test_cmd: Some("dotnet test".into()),
                lint_cmd: None,
                test_filter_format: Some("--filter {}".into()),
            },
            Self::Custom(_) => VerificationCommands {
                build_cmd: None,
                test_cmd: None,
                lint_cmd: None,
                test_filter_format: None,
            },
        }
    }

    /// Returns the default allowed Bash command prefixes for this build system.
    pub fn allowed_bash_prefixes(&self) -> Vec<&'static str> {
        match self {
            Self::Cargo => vec!["cargo"],
            Self::Npm => vec!["npm", "npx", "node"],
            Self::Yarn => vec!["yarn", "npx", "node"],
            Self::Pnpm => vec!["pnpm", "npx", "node"],
            Self::Go => vec!["go"],
            Self::Gradle => vec!["./gradlew", "gradlew", "gradle"],
            Self::Maven => vec!["mvn", "./mvnw", "mvnw"],
            Self::Poetry => vec!["poetry", "python", "pytest", "pip"],
            Self::Pip => vec!["python", "pytest", "pip"],
            Self::Make => vec!["make"],
            Self::Dotnet => vec!["dotnet"],
            Self::Custom(name) => match name.as_str() {
                "cmake" => vec!["cmake", "ctest", "make", "ninja"],
                "bazel" => vec!["bazel", "bazelisk"],
                "buck" => vec!["buck", "buck2"],
                "mix" => vec!["mix", "elixir", "iex"],
                "sbt" => vec!["sbt"],
                "nix" => vec!["nix", "nix-build", "nix-shell"],
                "stack" => vec!["stack", "ghc", "cabal"],
                "cabal" => vec!["cabal", "ghc"],
                "dune" => vec!["dune", "opam"],
                "meson" => vec!["meson", "ninja"],
                "scons" => vec!["scons"],
                "rebar" => vec!["rebar3", "rebar", "erl"],
                _ => vec![],
            },
        }
    }

    /// Returns all commonly used build/test tool prefixes.
    /// Used as fallback when build system cannot be detected.
    pub fn default_common_prefixes() -> Vec<&'static str> {
        vec![
            "cargo",
            "npm", "npx", "yarn", "pnpm", "node", "bun", "deno",
            "python", "python3", "pytest", "poetry", "pip", "pdm", "uv",
            "go",
            "./gradlew", "gradlew", "gradle", "mvn", "./mvnw", "mvnw", "sbt", "ant",
            "dotnet",
            "bundle", "rake", "ruby", "rspec", "rails",
            "composer", "php", "phpunit", "artisan",
            "swift", "xcodebuild", "swiftpm",
            "make", "cmake", "ninja", "meson", "scons",
            "mix", "elixir", "rebar3", "rebar",
            "stack", "cabal", "ghc",
            "dune", "opam",
            "nix", "nix-build", "nix-shell",
            "bazel", "bazelisk", "buck", "buck2",
            "zig",
            "scala", "scalac",
        ]
    }
}
