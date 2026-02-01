use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{PilotError, Result};

use super::model::{DEFAULT_MODEL, ModelConfig};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PilotConfig {
    pub orchestrator: OrchestratorConfig,
    pub isolation: IsolationConfig,
    pub verification: VerificationConfig,
    pub git: GitConfig,
    pub learning: LearningConfig,
    pub agent: AgentConfig,
    pub notification: NotificationConfig,
    pub context: ContextConfig,
    pub recovery: RecoveryConfig,
    pub chunked_planning: ChunkedPlanningConfig,
    pub quality: QualityConfig,
    pub complexity: ComplexityConfig,
    pub evidence: EvidenceConfig,
    pub display: DisplayConfig,
    pub task_decomposition: TaskDecompositionConfig,
    pub search: SearchConfig,
    pub evidence_sufficiency: crate::quality::EvidenceSufficiencyConfig,
    pub coherence: crate::quality::CoherenceConfig,
    pub pattern_bank: PatternBankConfig,
    pub focus: FocusConfig,
    pub multi_agent: MultiAgentConfig,
    pub tokenizer: TokenizerConfig,
    pub task_scoring: TaskScoringConfig,
    pub state: StateConfig,
    pub rules: RulesConfig,
}

impl PilotConfig {
    pub async fn load(pilot_dir: &Path) -> Result<Self> {
        let config_path = pilot_dir.join("config.toml");
        let config = if config_path.exists() {
            let content = fs::read_to_string(&config_path).await?;
            toml::from_str(&content)?
        } else {
            Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub async fn save(&self, pilot_dir: &Path) -> Result<()> {
        self.validate()?;
        let config_path = pilot_dir.join("config.toml");
        let content =
            toml::to_string_pretty(self).map_err(|e| PilotError::Config(e.to_string()))?;
        fs::write(&config_path, content).await?;
        Ok(())
    }

    /// Validate configuration values for consistency and safety.
    pub fn validate(&self) -> Result<()> {
        let mut errors = Vec::new();

        // Orchestrator validation
        if self.orchestrator.max_iterations == 0 {
            errors.push("max_iterations must be greater than 0");
        }
        if self.orchestrator.max_parallel_tasks == 0 {
            errors.push("max_parallel_tasks must be greater than 0");
        }
        // Isolation validation
        if self.isolation.small_change_threshold >= self.isolation.large_change_threshold {
            errors.push("small_change_threshold must be less than large_change_threshold");
        }

        // Agent validation
        if self.agent.timeout_secs == 0 {
            errors.push("agent timeout_secs must be greater than 0");
        }
        if self.agent.model.is_empty() {
            errors.push("agent model must not be empty");
        }

        // Context validation
        if !(0.0..=1.0).contains(&self.context.compaction.compaction_threshold) {
            errors.push("compaction.compaction_threshold must be between 0.0 and 1.0");
        }

        // Quality config validation (NON-NEGOTIABLE: Fact-Based Development from CLAUDE.md)
        // Evidence-based planning requires meaningful quality thresholds
        if !(0.0..=1.0).contains(&self.quality.min_evidence_quality) {
            errors.push("min_evidence_quality must be between 0.0 and 1.0");
        }
        if self.quality.min_evidence_quality < 0.5 {
            errors.push("min_evidence_quality must be >= 0.5 (NON-NEGOTIABLE: meaningful quality gate required)");
        }
        if !(0.0..=1.0).contains(&self.quality.min_evidence_confidence) {
            errors.push("min_evidence_confidence must be between 0.0 and 1.0");
        }
        if self.quality.min_evidence_confidence < 0.3 {
            errors.push("min_evidence_confidence must be >= 0.3 (NON-NEGOTIABLE: minimum confidence required)");
        }
        if !self.quality.require_verifiable_evidence {
            errors.push(
                "require_verifiable_evidence must be true (NON-NEGOTIABLE: fact-based development)",
            );
        }

        // Complexity config validation
        if self.complexity.assessment_timeout_secs == 0 {
            errors.push("complexity.assessment_timeout_secs must be greater than 0");
        }

        // Evidence budget coherence validation
        let budget_sum = self.evidence.file_budget_percent
            + self.evidence.pattern_budget_percent
            + self.evidence.dependency_budget_percent
            + self.evidence.knowledge_budget_percent;
        if budget_sum != 100 {
            errors.push("evidence budget percentages must sum to 100");
        }
        if !(0.0..=1.0).contains(&self.quality.evidence_coverage_threshold) {
            errors.push("evidence_coverage_threshold must be between 0.0 and 1.0");
        }

        // Context estimator validation
        if !(0.0..=1.0).contains(&self.context.estimator.safety_margin) {
            errors.push("context.estimator.safety_margin must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&self.context.estimator.selective_loading_ratio) {
            errors.push("context.estimator.selective_loading_ratio must be between 0.0 and 1.0");
        }

        // Convergent verification validation (NON-NEGOTIABLE conditions from CLAUDE.md)
        let cv = &self.recovery.convergent_verification;
        if cv.required_clean_rounds < 2 {
            errors.push("required_clean_rounds must be >= 2 (NON-NEGOTIABLE: 2-pass requirement)");
        }
        if !cv.include_ai_review {
            errors.push("include_ai_review must be true (NON-NEGOTIABLE: AI review required)");
        }
        if cv.max_rounds == 0 {
            errors.push("convergent max_rounds must be greater than 0");
        }
        if cv.max_rounds < cv.required_clean_rounds {
            errors.push("convergent max_rounds must be >= required_clean_rounds");
        }
        if cv.fix_timeout_secs < 30 {
            errors.push("fix_timeout_secs must be at least 30 seconds");
        }
        if cv.total_timeout_secs < cv.fix_timeout_secs {
            errors.push("total_timeout_secs must be >= fix_timeout_secs");
        }

        // Checkpoint config validation
        let cp = &self.recovery.checkpoint;
        if cp.interval_tasks == 0 {
            errors.push("checkpoint.interval_tasks must be greater than 0");
        }
        if cp.max_checkpoints == 0 {
            errors.push("checkpoint.max_checkpoints must be greater than 0");
        }
        // Evidence persistence consistency: if verifiable evidence is required,
        // it should also be persisted for durable execution
        if self.quality.require_verifiable_evidence && !cp.persist_evidence {
            errors.push(
                "checkpoint.persist_evidence must be true when require_verifiable_evidence is true \
                 (evidence gathering is expensive; persistence enables durable recovery)",
            );
        }

        // Learning config validation
        if !(0.0..=1.0).contains(&self.learning.min_confidence) {
            errors.push("learning.min_confidence must be between 0.0 and 1.0");
        }
        if self.learning.retry_history.retention_days <= 0 {
            errors.push("learning.retry_history.retention_days must be positive");
        }
        if !(0.0..=1.0).contains(&self.learning.retry_history.min_success_rate) {
            errors.push("learning.retry_history.min_success_rate must be between 0.0 and 1.0");
        }

        // Reasoning config validation (durable execution)
        let rc = &self.recovery.reasoning;
        if rc.enabled && rc.retention_days <= 0 {
            errors.push("reasoning.retention_days must be positive when enabled");
        }
        if rc.max_hypotheses_in_context == 0 {
            errors.push("reasoning.max_hypotheses_in_context must be greater than 0");
        }
        if rc.max_decisions_in_context == 0 {
            errors.push("reasoning.max_decisions_in_context must be greater than 0");
        }

        // Pattern bank validation
        let pb = &self.pattern_bank;
        if !(0.0..=1.0).contains(&pb.min_success_rate) {
            errors.push("pattern_bank.min_success_rate must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&pb.min_confidence) {
            errors.push("pattern_bank.min_confidence must be between 0.0 and 1.0");
        }
        if !(0.0..=1.0).contains(&pb.merge_similarity_threshold) {
            errors.push("pattern_bank.merge_similarity_threshold must be between 0.0 and 1.0");
        }
        if pb.max_patterns == 0 {
            errors.push("pattern_bank.max_patterns must be greater than 0");
        }
        {
            let weight_sum = pb.similarity_weight
                + pb.success_rate_weight
                + pb.recency_weight
                + pb.confidence_weight;
            if (weight_sum - 1.0).abs() > 0.01 {
                errors.push("pattern_bank scoring weights (similarity + success_rate + recency + confidence) must sum to 1.0");
            }
        }

        // Focus tracker validation
        if self.focus.max_context_switches == 0 {
            errors.push("focus.max_context_switches must be greater than 0");
        }
        if !(0.0..=1.0).contains(&self.focus.evidence_confidence_threshold) {
            errors.push("focus.evidence_confidence_threshold must be between 0.0 and 1.0");
        }

        // Multi-agent validation
        let ma = &self.multi_agent;
        if ma.max_concurrent_agents == 0 {
            errors.push("multi_agent.max_concurrent_agents must be greater than 0");
        }
        if ma.communication_timeout_secs == 0 {
            errors.push("multi_agent.communication_timeout_secs must be greater than 0");
        }
        if ma.max_agent_load == 0 {
            errors.push("multi_agent.max_agent_load must be greater than 0");
        }
        if ma.convergent_required_clean_rounds == 0 {
            errors.push("multi_agent.convergent_required_clean_rounds must be greater than 0");
        }
        if ma.convergent_required_clean_rounds < 2 {
            errors.push("multi_agent.convergent_required_clean_rounds must be >= 2 (NON-NEGOTIABLE: minimum quality gate)");
        }
        if ma.convergent_max_rounds == 0 {
            errors.push("multi_agent.convergent_max_rounds must be greater than 0");
        }
        if ma.convergent_max_rounds < ma.convergent_required_clean_rounds {
            errors.push(
                "multi_agent.convergent_max_rounds must be >= convergent_required_clean_rounds",
            );
        }
        if ma.convergent_max_fix_attempts_per_issue == 0 {
            errors.push("multi_agent.convergent_max_fix_attempts_per_issue must be greater than 0");
        }

        // Task scoring validation
        let ts = &self.task_scoring;
        {
            let w = &ts.weights;
            let weight_sum = w.file + w.dependency + w.pattern + w.confidence + w.history;
            if (weight_sum - 1.0).abs() > 0.01 {
                errors.push("task_scoring.weights must sum to 1.0");
            }
        }
        if ts.max_file_threshold == 0 {
            errors.push("task_scoring.max_file_threshold must be greater than 0");
        }

        // State config validation
        if self.state.snapshot_interval == 0 {
            errors.push("state.snapshot_interval must be greater than 0");
        }

        // Verification config validation
        if self.verification.allowed_commands_exclusive
            && self.verification.additional_allowed_commands.is_empty()
        {
            errors.push(
                "verification.additional_allowed_commands must not be empty when \
                 allowed_commands_exclusive is true (no commands would be allowed)",
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(PilotError::Config(format!(
                "Configuration validation failed:\n  - {}",
                errors.join("\n  - ")
            )))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorConfig {
    pub max_iterations: u32,
    pub max_parallel_tasks: usize,
    pub auto_commit: bool,
    /// When true, retry limits are dynamically increased based on failure patterns.
    /// Task max_retries increases after each failure category change.
    pub adaptive_limits: bool,
    /// Threshold in seconds before a lock is considered stale (default: 300 = 5 minutes)
    pub lock_stale_threshold_secs: u64,
    /// Number of retry attempts for lock acquisition
    pub lock_retry_attempts: u32,
    /// Delay in milliseconds between lock retry attempts (multiplied by attempt number)
    pub lock_retry_delay_ms: u64,
    /// Heartbeat update interval in seconds (default: 30)
    pub heartbeat_interval_secs: u64,
    /// Maximum mission duration in seconds (0 = unlimited, default: 604800 = 7 days)
    pub mission_timeout_secs: u64,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            max_parallel_tasks: 10,
            auto_commit: true,
            adaptive_limits: true,
            lock_stale_threshold_secs: 300,
            lock_retry_attempts: 3,
            lock_retry_delay_ms: 100,
            heartbeat_interval_secs: 30,
            mission_timeout_secs: 604800, // 7 days
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IsolationConfig {
    pub worktrees_dir: String,
    pub small_change_threshold: usize,
    pub large_change_threshold: usize,
}

impl Default for IsolationConfig {
    fn default() -> Self {
        Self {
            worktrees_dir: String::from(".worktrees"),
            small_change_threshold: 2,
            large_change_threshold: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VerificationConfig {
    pub build_cmd: Option<String>,
    pub test_cmd: Option<String>,
    pub lint_cmd: Option<String>,
    pub test_filter_format: Option<String>,
    pub verify_on_task_complete: bool,
    pub ai_plan_validation: bool,
    /// Timeout for verification commands in seconds (default: 300 = 5 minutes)
    pub command_timeout_secs: u64,
    /// Whether to require at least one verification command (build, test, or lint).
    /// When true and no commands are configured, verification returns a warning.
    pub require_verification_commands: bool,
    #[serde(default)]
    pub modules: HashMap<String, ModuleConfig>,
    /// Additional Bash command prefixes to allow beyond auto-detected defaults.
    /// These are merged with the build system's default allowed prefixes.
    /// Example: `["docker", "kubectl", "terraform"]`
    ///
    /// To use ONLY these commands (ignoring auto-detected), set `allowed_commands_exclusive = true`.
    #[serde(default)]
    pub additional_allowed_commands: Vec<String>,
    /// If true, use ONLY `additional_allowed_commands` and ignore auto-detected defaults.
    /// Default: false (merge additional with auto-detected).
    ///
    /// When true, `additional_allowed_commands` must not be empty.
    #[serde(default)]
    pub allowed_commands_exclusive: bool,
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            build_cmd: None,
            test_cmd: None,
            lint_cmd: None,
            test_filter_format: None,
            verify_on_task_complete: true,
            ai_plan_validation: true,
            command_timeout_secs: 300, // 5 minutes default
            require_verification_commands: true,
            modules: HashMap::new(),
            additional_allowed_commands: Vec::new(),
            allowed_commands_exclusive: false,
        }
    }
}

impl VerificationConfig {
    pub fn commands_for_module(&self, module: Option<&str>) -> VerificationCommands {
        if let Some(module_config) = module.and_then(|m| self.modules.get(m)) {
            return VerificationCommands {
                build_cmd: module_config
                    .build_cmd
                    .clone()
                    .or_else(|| self.build_cmd.clone()),
                test_cmd: module_config
                    .test_cmd
                    .clone()
                    .or_else(|| self.test_cmd.clone()),
                lint_cmd: module_config
                    .lint_cmd
                    .clone()
                    .or_else(|| self.lint_cmd.clone()),
                test_filter_format: module_config
                    .test_filter_format
                    .clone()
                    .or_else(|| self.test_filter_format.clone()),
            };
        }
        VerificationCommands {
            build_cmd: self.build_cmd.clone(),
            test_cmd: self.test_cmd.clone(),
            lint_cmd: self.lint_cmd.clone(),
            test_filter_format: self.test_filter_format.clone(),
        }
    }

    pub fn effective_full_commands(&self) -> VerificationCommands {
        let build_cmd = self
            .build_cmd
            .clone()
            .or_else(|| self.modules.values().find_map(|m| m.build_cmd.clone()));

        let test_cmd = self
            .test_cmd
            .clone()
            .or_else(|| self.modules.values().find_map(|m| m.test_cmd.clone()));

        let lint_cmd = self
            .lint_cmd
            .clone()
            .or_else(|| self.modules.values().find_map(|m| m.lint_cmd.clone()));

        VerificationCommands {
            build_cmd,
            test_cmd,
            lint_cmd,
            test_filter_format: self.test_filter_format.clone(),
        }
    }

    pub fn all_module_commands(&self) -> Vec<(String, VerificationCommands)> {
        self.modules
            .iter()
            .map(|(name, config)| {
                (
                    name.clone(),
                    VerificationCommands {
                        build_cmd: config.build_cmd.clone().or_else(|| self.build_cmd.clone()),
                        test_cmd: config.test_cmd.clone().or_else(|| self.test_cmd.clone()),
                        lint_cmd: config.lint_cmd.clone().or_else(|| self.lint_cmd.clone()),
                        test_filter_format: config
                            .test_filter_format
                            .clone()
                            .or_else(|| self.test_filter_format.clone()),
                    },
                )
            })
            .collect()
    }

    /// Resolve allowed Bash command prefixes for verification.
    ///
    /// Resolution order (layered):
    /// 1. If `allowed_commands_exclusive` is true, use only `additional_allowed_commands`
    /// 2. Otherwise, merge auto-detected build system defaults with `additional_allowed_commands`
    /// 3. If no build system detected, use common defaults + `additional_allowed_commands`
    ///
    /// Returns patterns suitable for `Bash(prefix:*)` permission rules.
    pub fn resolve_allowed_bash_prefixes(&self, working_dir: &Path) -> Vec<String> {
        if self.allowed_commands_exclusive {
            // User explicitly uses exclusive mode - use only their list
            return self.additional_allowed_commands.clone();
        }

        let mut prefixes: Vec<String> = if let Some(build_system) = BuildSystem::detect(working_dir)
        {
            // Use detected build system's defaults
            build_system
                .allowed_bash_prefixes()
                .into_iter()
                .map(String::from)
                .collect()
        } else {
            // Fallback to common defaults when no build system detected
            BuildSystem::default_common_prefixes()
                .into_iter()
                .map(String::from)
                .collect()
        };

        // Merge with user-configured additional commands
        for cmd in &self.additional_allowed_commands {
            if !prefixes.contains(cmd) {
                prefixes.push(cmd.clone());
            }
        }

        prefixes
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleConfig {
    pub build_cmd: Option<String>,
    pub test_cmd: Option<String>,
    pub lint_cmd: Option<String>,
    pub test_filter_format: Option<String>,
}

impl ModuleConfig {
    pub fn has_any(&self) -> bool {
        self.build_cmd.is_some() || self.test_cmd.is_some() || self.lint_cmd.is_some()
    }
}

/// Resolved verification commands for execution.
pub type VerificationCommands = ModuleConfig;

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
        // Check for known build systems first (most common to least common)
        // Rust
        if working_dir.join("Cargo.toml").exists() {
            return Some(Self::Cargo);
        }
        // Node.js (order matters: pnpm > yarn > npm)
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
        // JVM
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
        // Go
        if working_dir.join("go.mod").exists() {
            return Some(Self::Go);
        }
        // Python
        if working_dir.join("pyproject.toml").exists() {
            return Some(Self::Poetry);
        }
        if working_dir.join("requirements.txt").exists() {
            return Some(Self::Pip);
        }
        // Make
        if working_dir.join("Makefile").exists() {
            return Some(Self::Make);
        }
        // .NET
        if Self::has_file_with_extension(working_dir, ".csproj")
            || Self::has_file_with_extension(working_dir, ".sln")
        {
            return Some(Self::Dotnet);
        }

        // Check for custom build systems (recognized but require manual config)
        Self::detect_custom(working_dir)
    }

    /// Detect custom/emerging build systems.
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

        // Check for cabal files (*.cabal pattern)
        if Self::has_file_with_extension(working_dir, ".cabal") {
            return Some(Self::Custom("cabal".to_string()));
        }

        None
    }

    /// Helper to check if directory contains files with a specific extension.
    fn has_file_with_extension(dir: &Path, ext: &str) -> bool {
        std::fs::read_dir(dir).ok().is_some_and(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.path().to_string_lossy().ends_with(ext))
        })
    }

    pub fn commands(&self) -> VerificationCommands {
        match self {
            // Note on lint commands:
            // - Cargo's clippy is the de-facto standard Rust linter
            // - Npm/Yarn/Pnpm use `--if-present` so they only run if defined in package.json
            // - Make's `make lint` is project-defined, safe to run
            // - Other build systems: user must configure their preferred linter
            //   (Gradle: ktlint/detekt/checkstyle, Go: golangci-lint/staticcheck,
            //    Python: ruff/flake8/pylint/mypy, etc.)
            Self::Gradle => VerificationCommands {
                build_cmd: Some("./gradlew build -x test".into()),
                test_cmd: Some("./gradlew test".into()),
                lint_cmd: None, // User configures: ktlint, detekt, checkstyle, spotbugs, etc.
                test_filter_format: Some("--tests '{}'".into()),
            },
            Self::Maven => VerificationCommands {
                build_cmd: Some("mvn compile -q".into()),
                test_cmd: Some("mvn test -q".into()),
                lint_cmd: None, // User configures: checkstyle, spotbugs, pmd, etc.
                test_filter_format: Some("-Dtest={}".into()),
            },
            Self::Npm => VerificationCommands {
                build_cmd: Some("npm run build --if-present".into()),
                test_cmd: Some("npm test --if-present".into()),
                lint_cmd: Some("npm run lint --if-present".into()), // Safe: only runs if defined
                test_filter_format: Some("-- --testPathPattern='{}'".into()),
            },
            Self::Yarn => VerificationCommands {
                build_cmd: Some("yarn build --if-present".into()),
                test_cmd: Some("yarn test --if-present".into()),
                lint_cmd: Some("yarn lint --if-present".into()), // Safe: only runs if defined
                test_filter_format: Some("--testPathPattern='{}'".into()),
            },
            Self::Pnpm => VerificationCommands {
                build_cmd: Some("pnpm build --if-present".into()),
                test_cmd: Some("pnpm test --if-present".into()),
                lint_cmd: Some("pnpm lint --if-present".into()), // Safe: only runs if defined
                test_filter_format: Some("--testPathPattern='{}'".into()),
            },
            Self::Cargo => VerificationCommands {
                build_cmd: Some("cargo build".into()),
                test_cmd: Some("cargo test".into()),
                lint_cmd: Some("cargo clippy -- -D warnings".into()), // De-facto Rust standard
                test_filter_format: Some("{}".into()),
            },
            Self::Go => VerificationCommands {
                build_cmd: Some("go build ./...".into()),
                test_cmd: Some("go test ./...".into()),
                lint_cmd: None, // User configures: golangci-lint, staticcheck, revive, etc.
                test_filter_format: Some("-run {}".into()),
            },
            Self::Poetry => VerificationCommands {
                build_cmd: None,
                test_cmd: Some("poetry run pytest".into()),
                lint_cmd: None, // User configures: ruff, flake8, pylint, mypy, black, etc.
                test_filter_format: Some("-k {}".into()),
            },
            Self::Pip => VerificationCommands {
                build_cmd: None,
                test_cmd: Some("pytest".into()),
                lint_cmd: None, // User configures: ruff, flake8, pylint, mypy, black, etc.
                test_filter_format: Some("-k {}".into()),
            },
            Self::Make => VerificationCommands {
                build_cmd: Some("make".into()),
                test_cmd: Some("make test".into()),
                lint_cmd: Some("make lint".into()), // Safe: project-defined
                test_filter_format: None,
            },
            Self::Dotnet => VerificationCommands {
                build_cmd: Some("dotnet build".into()),
                test_cmd: Some("dotnet test".into()),
                lint_cmd: None, // User configures: dotnet format, StyleCop, etc.
                test_filter_format: Some("--filter {}".into()),
            },
            // Custom build systems require manual configuration
            Self::Custom(_) => VerificationCommands {
                build_cmd: None,
                test_cmd: None,
                lint_cmd: None,
                test_filter_format: None,
            },
        }
    }

    /// Returns the default allowed Bash command prefixes for this build system.
    ///
    /// These prefixes are used by `VerifyExecute` permission profile to allow
    /// specific build/test/lint commands while blocking other shell operations.
    ///
    /// The returned prefixes are patterns like "cargo" that will match commands
    /// starting with that prefix (e.g., "cargo build", "cargo test").
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
            // Rust
            "cargo",
            // JavaScript/Node.js
            "npm",
            "npx",
            "yarn",
            "pnpm",
            "node",
            "bun",
            "deno",
            // Python
            "python",
            "python3",
            "pytest",
            "poetry",
            "pip",
            "pdm",
            "uv",
            // Go
            "go",
            // Java/JVM
            "./gradlew",
            "gradlew",
            "gradle",
            "mvn",
            "./mvnw",
            "mvnw",
            "sbt",
            "ant",
            // .NET
            "dotnet",
            // Ruby
            "bundle",
            "rake",
            "ruby",
            "rspec",
            "rails",
            // PHP
            "composer",
            "php",
            "phpunit",
            "artisan",
            // Swift/iOS
            "swift",
            "xcodebuild",
            "swiftpm",
            // C/C++
            "make",
            "cmake",
            "ninja",
            "meson",
            "scons",
            // Elixir/Erlang
            "mix",
            "elixir",
            "rebar3",
            "rebar",
            // Haskell
            "stack",
            "cabal",
            "ghc",
            // OCaml
            "dune",
            "opam",
            // Nix
            "nix",
            "nix-build",
            "nix-shell",
            // Bazel/Buck
            "bazel",
            "bazelisk",
            "buck",
            "buck2",
            // Zig
            "zig",
            // Scala
            "scala",
            "scalac",
        ]
    }
}

impl VerificationConfig {
    /// Auto-detect build system and populate commands if not already configured
    pub fn with_auto_detection(mut self, working_dir: &Path) -> Self {
        if self.build_cmd.is_some() || self.test_cmd.is_some() || self.lint_cmd.is_some() {
            return self;
        }

        if let Some(build_system) = BuildSystem::detect(working_dir) {
            let commands = build_system.commands();
            self.build_cmd = commands.build_cmd;
            self.test_cmd = commands.test_cmd;
            self.lint_cmd = commands.lint_cmd.or_else(|| {
                // Try to detect lint command from config files
                LintDetector::detect(working_dir, &build_system)
            });
            if self.test_filter_format.is_none() {
                self.test_filter_format = commands.test_filter_format;
            }
        }
        self
    }
}

/// Lint tool detection from build configuration files.
/// Detects configured linters that the build system doesn't know about.
pub struct LintDetector;

impl LintDetector {
    /// Detect lint command from build configuration files.
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

    /// Detect Gradle lint plugins (Detekt, ktlint, Checkstyle, SpotBugs)
    fn detect_gradle_lint(working_dir: &Path) -> Option<String> {
        // Check build.gradle.kts and build.gradle
        for gradle_file in ["build.gradle.kts", "build.gradle"] {
            let path = working_dir.join(gradle_file);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let content_lower = content.to_lowercase();

                // Detekt (Kotlin static analysis)
                if content_lower.contains("detekt") {
                    return Some("./gradlew detekt".into());
                }

                // ktlint (Kotlin linter)
                if content_lower.contains("ktlint") {
                    return Some("./gradlew ktlintCheck".into());
                }

                // Checkstyle (Java)
                if content_lower.contains("checkstyle") {
                    return Some("./gradlew checkstyleMain checkstyleTest".into());
                }

                // SpotBugs (Java)
                if content_lower.contains("spotbugs") {
                    return Some("./gradlew spotbugsMain".into());
                }

                // PMD (Java)
                if content_lower.contains("pmd") {
                    return Some("./gradlew pmdMain".into());
                }
            }
        }
        None
    }

    /// Detect Maven lint plugins
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

    /// Detect Python lint tools from pyproject.toml, setup.cfg, or pre-commit
    fn detect_python_lint(working_dir: &Path) -> Option<String> {
        // Check pyproject.toml (most common for modern Python)
        let pyproject_path = working_dir.join("pyproject.toml");
        if let Ok(content) = std::fs::read_to_string(&pyproject_path) {
            let content_lower = content.to_lowercase();

            // Ruff (modern, fast linter)
            if content_lower.contains("[tool.ruff]") || content_lower.contains("ruff") {
                return Some("ruff check .".into());
            }

            // Flake8
            if content_lower.contains("[tool.flake8]") {
                return Some("flake8 .".into());
            }

            // Pylint
            if content_lower.contains("[tool.pylint]") {
                return Some("pylint .".into());
            }

            // Black (formatter, but commonly run as lint check)
            if content_lower.contains("[tool.black]") {
                return Some("black --check .".into());
            }

            // MyPy (type checker)
            if content_lower.contains("[tool.mypy]") {
                return Some("mypy .".into());
            }
        }

        // Check setup.cfg
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

        // Check .pre-commit-config.yaml
        if let Some(cmd) = Self::detect_from_precommit(working_dir) {
            return Some(cmd);
        }

        None
    }

    /// Detect Go lint tools
    fn detect_go_lint(working_dir: &Path) -> Option<String> {
        // Check .golangci.yml/.golangci.yaml (golangci-lint config)
        for config in [".golangci.yml", ".golangci.yaml", ".golangci.toml"] {
            if working_dir.join(config).exists() {
                return Some("golangci-lint run".into());
            }
        }

        // Check for staticcheck config
        if working_dir.join("staticcheck.conf").exists() {
            return Some("staticcheck ./...".into());
        }

        // Check Makefile for common lint targets
        if let Some(cmd) = Self::detect_from_makefile(working_dir, "lint") {
            return Some(cmd);
        }

        None
    }

    /// Detect Node.js lint tools from package.json
    fn detect_node_lint(working_dir: &Path, build_system: &BuildSystem) -> Option<String> {
        let pkg_path = working_dir.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            // Check scripts section for lint command
            if content.contains("\"lint\"") || content.contains("\"lint:") {
                // The build system already handles --if-present
                return None;
            }

            // Check devDependencies for common linters
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

        // Check for ESLint config files
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

    /// Detect .NET lint tools
    fn detect_dotnet_lint(working_dir: &Path) -> Option<String> {
        // Check for .editorconfig (dotnet format respects it)
        if working_dir.join(".editorconfig").exists() {
            return Some("dotnet format --verify-no-changes".into());
        }

        // Check for StyleCop or other analyzers in .csproj files
        if Self::has_file_with_extension(working_dir, ".csproj") {
            // dotnet format is safe to run as a check
            return Some("dotnet format --verify-no-changes".into());
        }

        None
    }

    /// Detect lint tools from pre-commit config
    fn detect_from_precommit(working_dir: &Path) -> Option<String> {
        let precommit_path = working_dir.join(".pre-commit-config.yaml");
        if let Ok(content) = std::fs::read_to_string(&precommit_path) {
            let content_lower = content.to_lowercase();

            // Check for common linters in pre-commit hooks
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

    /// Check Makefile for lint target
    fn detect_from_makefile(working_dir: &Path, target: &str) -> Option<String> {
        let makefile_path = working_dir.join("Makefile");
        if let Ok(content) = std::fs::read_to_string(&makefile_path) {
            // Simple check: does it have a lint target?
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GitConfig {
    pub default_branch: String,
    pub commit_prefix: String,
    pub branch_prefix: String,
    pub auto_push: bool,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            default_branch: String::from("main"),
            commit_prefix: String::from("wip"),
            branch_prefix: String::from("pilot"),
            auto_push: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    /// Enable automatic learning extraction after mission completion
    pub auto_extract: bool,
    /// Extract reusable skills (patterns, techniques)
    pub extract_skills: bool,
    /// Extract coding rules (conventions, standards)
    pub extract_rules: bool,
    /// Extract specialized agents (experimental, requires vetting)
    pub extract_agents: bool,
    /// Minimum confidence threshold for extraction (0.0 - 1.0)
    pub min_confidence: f32,
    /// Retry history configuration for learning from past retry attempts
    pub retry_history: RetryHistoryConfig,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            auto_extract: false,
            extract_skills: true,
            extract_rules: true,
            extract_agents: false,
            min_confidence: 0.6,
            retry_history: RetryHistoryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryHistoryConfig {
    /// Enable retry history tracking and learning
    pub enabled: bool,
    /// Retention period in days for retry history entries
    pub retention_days: i64,
    /// Minimum sample size before using learned strategies
    pub min_samples_for_learning: usize,
    /// Minimum success rate threshold for strategy recommendation
    pub min_success_rate: f32,
}

impl Default for RetryHistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
            min_samples_for_learning: 2,
            min_success_rate: 0.5,
        }
    }
}

impl LearningConfig {
    /// Filter extraction result based on config flags and confidence threshold
    pub fn filter(
        &self,
        result: crate::learning::ExtractionResult,
    ) -> crate::learning::ExtractionResult {
        crate::learning::ExtractionResult {
            skills: if self.extract_skills {
                result
                    .skills
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
            rules: if self.extract_rules {
                result
                    .rules
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
            agents: if self.extract_agents {
                result
                    .agents
                    .into_iter()
                    .filter(|c| c.confidence >= self.min_confidence)
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// Authentication mode for Claude API access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// OAuth-based authentication via Claude Code CLI.
    /// NOTE: Extended context (1M tokens) is NOT available in OAuth mode.
    /// Context window is limited to 200k tokens regardless of extended_context setting.
    #[default]
    Oauth,
    /// Direct API key authentication.
    /// Extended context (1M tokens) is available for supported models.
    ApiKey,
}

/// Model selection agent type for per-agent model configuration.
/// Note: This is distinct from mission::AgentType which classifies task types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAgentType {
    /// Task execution agent (default workhorse)
    Task,
    /// Planning and specification agent (complex reasoning)
    Planning,
    /// Validation and review agent (quality checks)
    Validation,
    /// Evidence gathering agent (codebase analysis)
    Evidence,
    /// Retry analysis agent (failure diagnosis)
    Retry,
    /// Learning extraction agent (post-execution analysis)
    Learning,
}

/// Per-agent model configuration.
/// Allows different models for different agent types based on their requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentModelsConfig {
    /// Default model used when no specific model is configured for an agent type.
    pub default: String,
    /// Model for task execution (balanced speed/quality).
    pub task: Option<String>,
    /// Model for planning/specification (complex reasoning, may use more powerful model).
    pub planning: Option<String>,
    /// Model for validation/review (can use faster model for simple checks).
    pub validation: Option<String>,
    /// Model for evidence gathering (codebase analysis).
    pub evidence: Option<String>,
    /// Model for retry analysis (failure diagnosis).
    pub retry: Option<String>,
    /// Model for learning extraction (post-execution analysis).
    pub learning: Option<String>,
}

impl Default for AgentModelsConfig {
    fn default() -> Self {
        Self {
            default: String::from(DEFAULT_MODEL),
            task: None,       // Uses default
            planning: None,   // Uses default (consider opus for complex projects)
            validation: None, // Uses default (consider haiku for simple checks)
            evidence: None,   // Uses default
            retry: None,      // Uses default
            learning: None,   // Uses default
        }
    }
}

impl AgentModelsConfig {
    /// Get the model name for a specific agent type.
    /// Falls back to default if no specific model is configured.
    pub fn model_for(&self, agent_type: ModelAgentType) -> &str {
        let specific = match agent_type {
            ModelAgentType::Task => &self.task,
            ModelAgentType::Planning => &self.planning,
            ModelAgentType::Validation => &self.validation,
            ModelAgentType::Evidence => &self.evidence,
            ModelAgentType::Retry => &self.retry,
            ModelAgentType::Learning => &self.learning,
        };
        specific.as_deref().unwrap_or(&self.default)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Default model. Use `models` for per-agent configuration.
    pub model: String,
    /// Per-agent model configuration. Overrides `model` when specific agent model is set.
    pub models: AgentModelsConfig,
    /// Authentication mode: oauth (default) or api_key.
    /// IMPORTANT: In OAuth mode, extended_context is ignored (200k limit applies).
    pub auth_mode: AuthMode,
    /// Enable extended context window (1M tokens) for supported models.
    /// NOTE: Only effective when auth_mode = api_key. Ignored in OAuth mode.
    pub extended_context: bool,
    pub max_retries: u32,
    pub timeout_secs: u64,
    pub planning_timeout_secs: u64,
    pub chunk_timeout_secs: u64,
    pub subprocess_timeout_secs: u64,
    pub complex_task_timeout_multiplier: f32,
    pub max_sdk_iterations: u32,
    #[serde(default)]
    pub plugin_dirs: Vec<String>,
}

impl AgentConfig {
    /// Returns the effective ModelConfig for the default model based on auth_mode.
    /// In OAuth mode, extended_context is always disabled (200k limit).
    /// For per-agent model config, use `model_config_for(agent_type)`.
    pub fn model_config(&self) -> crate::error::Result<ModelConfig> {
        self.model_config_for_name(self.effective_default_model())
    }

    /// Returns the effective ModelConfig for a specific agent type.
    /// Uses per-agent model if configured, otherwise falls back to default.
    pub fn model_config_for(
        &self,
        agent_type: ModelAgentType,
    ) -> crate::error::Result<ModelConfig> {
        let model_name = self.models.model_for(agent_type);
        self.model_config_for_name(model_name)
    }

    /// Internal helper to create ModelConfig with auth_mode constraints.
    fn model_config_for_name(&self, model_name: &str) -> crate::error::Result<ModelConfig> {
        let effective_extended = match self.auth_mode {
            AuthMode::Oauth => false, // OAuth doesn't support extended context
            AuthMode::ApiKey => self.extended_context,
        };
        Ok(
            ModelConfig::from_name_or_default(model_name)?
                .with_extended_context(effective_extended),
        )
    }

    /// Returns the effective default model name.
    /// Uses `models.default` if set, otherwise uses `model`.
    pub fn effective_default_model(&self) -> &str {
        if !self.models.default.is_empty() {
            &self.models.default
        } else {
            &self.model
        }
    }

    /// Returns whether extended context is effectively enabled.
    pub fn effective_extended_context(&self) -> bool {
        self.auth_mode == AuthMode::ApiKey && self.extended_context
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: String::from(DEFAULT_MODEL),
            models: AgentModelsConfig::default(),
            auth_mode: AuthMode::Oauth, // Default: OAuth via Claude Code CLI
            extended_context: false,    // Disabled by default (and ignored in OAuth anyway)
            max_retries: 3,
            timeout_secs: 600,
            planning_timeout_secs: 0,
            chunk_timeout_secs: 180,
            subprocess_timeout_secs: 30,
            complex_task_timeout_multiplier: 1.5,
            max_sdk_iterations: 100,
            // Plugin directories searched in order:
            // 1. Project-level claudegen plugins (standard output location)
            // 2. Project-local plugins directory
            // 3. SDK default (~/.claude/plugins/) is added automatically by SDK
            plugin_dirs: vec![
                ".".to_string(), // Project root for {project}-plugin/ directories
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    pub desktop: bool,
    pub event_log: bool,
    pub hook_command: Option<String>,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            desktop: true,
            event_log: true,
            hook_command: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    // MissionContext compaction settings
    pub compaction_threshold: f32,
    pub aggressive_ratio: f32,
    pub preserve_recent_tasks: usize,
    pub preserve_recent_learnings: usize,
    /// Maximum critical learnings to preserve during emergency compaction
    pub emergency_max_critical_learnings: usize,
    pub min_phase_age_for_compression: usize,
    pub enable_semantic_dedup: bool,
    pub phase_summary_max_chars: usize,
    pub task_summary_max_chars: usize,
    pub max_archived_snapshots: usize,
    pub dedup_similarity_threshold: f32,

    // TaskContext compaction settings
    /// Estimated token threshold for TaskContext compaction (default: 50_000)
    pub task_context_threshold: usize,
    /// Maximum key findings to retain in TaskContext (default: 100)
    pub max_key_findings: usize,
    /// Maximum blockers to retain in TaskContext (default: 20)
    pub max_blockers: usize,
    /// Maximum related files to retain in TaskContext (default: 200)
    pub max_related_files: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            compaction_threshold: 0.8,
            aggressive_ratio: 0.5,
            preserve_recent_tasks: 5,
            preserve_recent_learnings: 10,
            emergency_max_critical_learnings: 3,
            min_phase_age_for_compression: 2,
            enable_semantic_dedup: true,
            phase_summary_max_chars: 50,
            task_summary_max_chars: 30,
            max_archived_snapshots: 3,
            dedup_similarity_threshold: 0.8,
            // TaskContext defaults
            task_context_threshold: 50_000,
            max_key_findings: 100,
            max_blockers: 20,
            max_related_files: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextLimitsConfig {
    pub max_objective_chars: usize,
    pub max_critical_learnings: usize,
    pub max_key_decisions: usize,
    pub max_phase_files: usize,
    pub max_phase_learnings: usize,
    pub max_phase_changes: usize,
}

impl Default for ContextLimitsConfig {
    fn default() -> Self {
        Self {
            max_objective_chars: 200,
            max_critical_learnings: 5,
            max_key_decisions: 3,
            max_phase_files: 10,
            max_phase_learnings: 3,
            max_phase_changes: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    pub max_tokens_override: Option<usize>,
    pub enable_auto_compaction: bool,
    pub limits: ContextLimitsConfig,
    pub compaction: CompactionConfig,
    pub task_budget: TaskBudgetConfig,
    pub budget_allocation: BudgetAllocationConfig,
    pub phase_complexity: PhaseComplexityConfig,
    pub estimator: ContextEstimatorConfig,
}

/// Configuration for context size estimation.
/// Controls pre-flight checks for MCP resource loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextEstimatorConfig {
    /// Estimated base context overhead from Claude Code SDK.
    /// Includes system prompt, tool definitions, and MCP server instructions.
    pub base_overhead_tokens: usize,
    /// Minimum available budget for actual work (prompt + evidence + output).
    pub min_working_budget: usize,
    /// Safety margin ratio to avoid hitting exact limits (0.0-1.0).
    pub safety_margin: f32,
    /// Ratio threshold for switching to selective loading (0.0-1.0).
    pub selective_loading_ratio: f32,
    /// Maximum directory depth for resource scanning.
    pub max_directory_depth: usize,
    /// Token threshold for warning about large files.
    pub large_file_warning_threshold: usize,
    /// Fallback chars per token when content is unreadable.
    pub fallback_chars_per_token: usize,
    /// Maximum files to scan per directory before extrapolating.
    /// Prevents slow scanning on large directories.
    pub max_files_per_directory: usize,
}

impl Default for ContextEstimatorConfig {
    fn default() -> Self {
        Self {
            base_overhead_tokens: 40_000,
            min_working_budget: 30_000,
            safety_margin: 0.9,
            selective_loading_ratio: 0.7,
            max_directory_depth: 5,
            large_file_warning_threshold: 10_000,
            fallback_chars_per_token: 4,
            max_files_per_directory: 500,
        }
    }
}

impl ContextConfig {
    pub fn effective_max_tokens(&self, model_config: &ModelConfig) -> usize {
        self.max_tokens_override
            .unwrap_or_else(|| model_config.usable_context() as usize)
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens_override: None,
            enable_auto_compaction: true,
            limits: ContextLimitsConfig::default(),
            compaction: CompactionConfig::default(),
            task_budget: TaskBudgetConfig::default(),
            budget_allocation: BudgetAllocationConfig::default(),
            phase_complexity: PhaseComplexityConfig::default(),
            estimator: ContextEstimatorConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryConfig {
    pub enabled: bool,
    pub max_retries_per_failure: u32,
    pub failure_history_ttl_secs: u64,
    pub retry_analyzer: RetryAnalyzerConfig,
    pub execution_error: ExecutionErrorConfig,
    pub compaction_levels: CompactionLevelConfig,
    pub convergent_verification: ConvergentVerificationConfig,
    pub checkpoint: CheckpointConfig,
    pub reasoning: ReasoningConfig,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries_per_failure: 3,
            failure_history_ttl_secs: 3600,
            retry_analyzer: RetryAnalyzerConfig::default(),
            execution_error: ExecutionErrorConfig::default(),
            compaction_levels: CompactionLevelConfig::default(),
            convergent_verification: ConvergentVerificationConfig::default(),
            checkpoint: CheckpointConfig::default(),
            reasoning: ReasoningConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetryAnalyzerConfig {
    pub max_error_history: usize,
    pub consecutive_error_threshold: usize,
    /// Maximum retries for transient errors (network, rate-limit, etc.)
    /// before escalating as likely permanent.
    pub transient_error_retry_limit: usize,
}

impl Default for RetryAnalyzerConfig {
    fn default() -> Self {
        Self {
            max_error_history: 5,
            consecutive_error_threshold: 3,
            transient_error_retry_limit: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExecutionErrorConfig {
    pub timeout_retries: u32,
    pub timeout_delay_secs: u64,
    pub chunk_timeout_retries: u32,
    pub chunk_timeout_delay_secs: u64,
    pub rate_limit_retries: u32,
    pub rate_limit_default_delay_secs: u64,
    pub network_error_retries: u32,
    pub network_error_delay_secs: u64,
    pub stream_error_retries: u32,
    pub stream_error_delay_secs: u64,
}

impl Default for ExecutionErrorConfig {
    fn default() -> Self {
        Self {
            timeout_retries: 3,
            timeout_delay_secs: 5,
            chunk_timeout_retries: 2,
            chunk_timeout_delay_secs: 10,
            rate_limit_retries: 1,
            rate_limit_default_delay_secs: 120,
            network_error_retries: 3,
            network_error_delay_secs: 5,
            stream_error_retries: 2,
            stream_error_delay_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionLevelConfig {
    pub light_threshold: f32,
    pub light_aggressive: f32,
    pub moderate_threshold: f32,
    pub moderate_aggressive: f32,
    pub aggressive_threshold: f32,
    pub aggressive_ratio: f32,
    pub aggressive_preserve_tasks: usize,
    pub aggressive_preserve_learnings: usize,
    pub emergency_threshold: f32,
    pub emergency_aggressive: f32,
    pub emergency_preserve_tasks: usize,
    pub emergency_preserve_learnings: usize,
    pub emergency_min_phase_age: usize,
}

impl Default for CompactionLevelConfig {
    fn default() -> Self {
        Self {
            light_threshold: 0.9,
            light_aggressive: 0.7,
            moderate_threshold: 0.8,
            moderate_aggressive: 0.5,
            aggressive_threshold: 0.6,
            aggressive_ratio: 0.4,
            aggressive_preserve_tasks: 3,
            aggressive_preserve_learnings: 5,
            emergency_threshold: 0.4,
            emergency_aggressive: 0.2,
            emergency_preserve_tasks: 1,
            emergency_preserve_learnings: 3,
            emergency_min_phase_age: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvergentVerificationConfig {
    pub required_clean_rounds: u32,
    pub max_rounds: u32,
    pub max_fix_attempts_per_issue: u32,
    pub include_ai_review: bool,
    pub auto_fix_enabled: bool,
    /// Timeout for individual fix attempts in seconds (default: 300 = 5 minutes)
    pub fix_timeout_secs: u64,
    /// Maximum total time for convergent verification in seconds (default: 1800 = 30 minutes)
    pub total_timeout_secs: u64,
    /// Number of recent rounds to check for regression detection (default: 3)
    pub regression_detection_rounds: usize,
    /// Maximum rounds to keep in verification history.
    pub max_history_rounds: usize,
    /// Maximum issues to process per batch in fix attempts.
    pub max_issues_per_batch: usize,
    /// Maximum length for fix output storage.
    pub max_fix_output_length: usize,
    /// Whether to use isolated sessions for fix attempts.
    pub use_isolated_fix_session: bool,
    /// Maximum entries in drift history window.
    pub max_drift_history_window: usize,
    /// Maximum entries in global issue registry.
    pub max_issue_registry_entries: usize,
    /// Minimum occurrences for an issue to be considered "persistent" (default: 3).
    /// Issues with total_occurrences >= this value and unresolved are persistent.
    pub persistent_issue_threshold: u32,
    /// Oscillation threshold - number of fix-break-fix cycles before declaring oscillation.
    /// Higher values allow more retry attempts before giving up.
    pub oscillation_threshold: u32,
    /// Grace period before oscillation detection kicks in.
    /// Allows fixes time to stabilize before flagging as oscillating.
    /// Default: 2 rounds (issues reappearing after round 2 count as oscillation).
    pub oscillation_grace_rounds: u32,
    /// Additional grace rounds for regression without progress.
    /// Default: 1 extra round beyond base grace period (so round > grace + 1).
    pub oscillation_regression_buffer: u32,
    /// Minimum confidence score (0.0-1.0) for PatternBank recommendations to be used.
    /// Lower values allow more pattern suggestions; higher values require stronger matches.
    pub pattern_confidence_threshold: f32,
    /// Maximum characters for diff display before truncation.
    /// Larger values provide more context but may overwhelm output.
    pub diff_truncation_length: usize,
    /// Allow adaptive extension of max_rounds when meaningful progress is detected.
    /// If drift analysis shows net issues reduced, additional rounds are granted.
    pub allow_adaptive_extension: bool,
    /// Additional rounds granted per adaptive extension (default: 3).
    pub adaptive_extension_rounds: u32,
    /// Maximum number of adaptive extensions allowed (default: 2, so up to 6 extra rounds).
    pub max_adaptive_extensions: u32,
}

impl Default for ConvergentVerificationConfig {
    fn default() -> Self {
        Self {
            required_clean_rounds: 2,
            max_rounds: 10,
            max_fix_attempts_per_issue: 5, // Strategies cycle if this exceeds available count
            include_ai_review: true,
            auto_fix_enabled: true,
            fix_timeout_secs: 300,
            total_timeout_secs: 1800,
            regression_detection_rounds: 3,
            max_history_rounds: 20,
            max_issues_per_batch: 5,
            max_fix_output_length: 2000,
            use_isolated_fix_session: false,
            max_drift_history_window: 5,
            max_issue_registry_entries: 100,
            persistent_issue_threshold: 3,
            oscillation_threshold: 3,
            // Grace period for oscillation detection: allows fixes to stabilize.
            // Issues reappearing after this many rounds count toward oscillation.
            oscillation_grace_rounds: 2,
            // Extra buffer for regression (new issues without progress): grace + 1.
            oscillation_regression_buffer: 1,
            pattern_confidence_threshold: 0.6,
            diff_truncation_length: 10000,
            // Adaptive extension: grant extra rounds when meaningful progress is detected.
            // Disabled by default to preserve backwards compatibility with existing behavior.
            allow_adaptive_extension: true,
            adaptive_extension_rounds: 3,
            max_adaptive_extensions: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChunkedPlanningConfig {
    pub enabled: bool,
    pub max_user_stories_per_chunk: usize,
    pub max_requirements_per_chunk: usize,
    pub auto_chunk: bool,
    pub max_tokens_divisor: usize,
    pub complexity_divisor: usize,
    /// Minimum number of phases for mission outline.
    pub min_phases: usize,
    /// Maximum number of phases for mission outline.
    pub max_phases: usize,
    /// Maximum user stories per phase.
    pub max_stories_per_phase: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskBudgetConfig {
    pub mission_summary_tokens: usize,
    pub current_task_tokens: usize,
    pub direct_dependencies_tokens: usize,
    pub learnings_tokens: usize,
}

impl Default for TaskBudgetConfig {
    fn default() -> Self {
        Self {
            mission_summary_tokens: 200,
            current_task_tokens: 500,
            direct_dependencies_tokens: 1000,
            learnings_tokens: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BudgetAllocationConfig {
    pub mission_summary_ratio: f32,
    pub current_phase_ratio: f32,
    pub current_task_ratio: f32,
    pub recent_tasks_ratio: f32,
    pub learnings_ratio: f32,
    pub evidence_ratio: f32,
    pub reserved_output_ratio: f32,
    pub recent_tasks_available_divisor: usize,
    pub rebalance_reduction_divisor: usize,
}

impl Default for BudgetAllocationConfig {
    fn default() -> Self {
        Self {
            mission_summary_ratio: 0.025,
            current_phase_ratio: 0.10,
            current_task_ratio: 0.15,
            recent_tasks_ratio: 0.10,
            learnings_ratio: 0.05,
            evidence_ratio: 0.15,
            reserved_output_ratio: 0.20,
            recent_tasks_available_divisor: 4,
            rebalance_reduction_divisor: 3,
        }
    }
}

impl BudgetAllocationConfig {
    pub fn for_budget(
        &self,
        max_total: usize,
    ) -> (usize, usize, usize, usize, usize, usize, usize) {
        (
            (max_total as f32 * self.mission_summary_ratio) as usize,
            (max_total as f32 * self.current_phase_ratio) as usize,
            (max_total as f32 * self.current_task_ratio) as usize,
            (max_total as f32 * self.recent_tasks_ratio) as usize,
            (max_total as f32 * self.learnings_ratio) as usize,
            (max_total as f32 * self.evidence_ratio) as usize,
            (max_total as f32 * self.reserved_output_ratio) as usize,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PhaseComplexityConfig {
    /// Weight for task count in complexity scoring
    pub task_count_weight: usize,
    /// Weight for dependency depth in complexity scoring
    pub dependency_depth_weight: usize,
    /// Weight for files touched in complexity scoring
    pub files_touched_weight: usize,
    /// Score threshold for Simple complexity (inclusive)
    pub simple_threshold: usize,
    /// Score threshold for Moderate complexity (inclusive)
    pub moderate_threshold: usize,
    /// Score threshold for Complex complexity (inclusive)
    pub complex_threshold: usize,
    /// Multiplier for Simple complexity budget adjustment
    pub simple_multiplier: f32,
    /// Multiplier for Moderate complexity budget adjustment
    pub moderate_multiplier: f32,
    /// Multiplier for Complex complexity budget adjustment
    pub complex_multiplier: f32,
    /// Multiplier for Critical complexity budget adjustment
    pub critical_multiplier: f32,
}

impl Default for PhaseComplexityConfig {
    fn default() -> Self {
        Self {
            task_count_weight: 2,
            dependency_depth_weight: 3,
            files_touched_weight: 1,
            simple_threshold: 10,
            moderate_threshold: 25,
            complex_threshold: 50,
            simple_multiplier: 0.7,
            moderate_multiplier: 1.0,
            complex_multiplier: 1.4,
            critical_multiplier: 1.8,
        }
    }
}

/// Configuration for display limits and presentation.
/// Controls how many items are shown in various contexts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Maximum recent items to show (learnings, phases, tasks, etc.)
    pub max_recent_items: usize,
    /// Maximum samples to display (errors, validation results, etc.)
    pub max_display_samples: usize,
    /// Maximum path depth for directory analysis
    pub max_path_depth: usize,
    /// Git commit hash display length
    pub commit_display_length: usize,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            max_recent_items: 3,
            max_display_samples: 5,
            max_path_depth: 2,
            commit_display_length: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QualityConfig {
    pub min_evidence_quality: f32,
    pub min_evidence_confidence: f32,
    pub require_verifiable_evidence: bool,
    pub verifiable_weight: f32,
    pub confidence_weight: f32,
    /// Evidence coverage threshold for "Sufficient" tier (0.5 = 50% coverage required)
    pub evidence_coverage_threshold: f32,
    /// Lower bound for "SufficientButRisky" tier (0.3 = 30%).
    /// Coverage between this and evidence_coverage_threshold triggers LLM assessment.
    pub risky_coverage_threshold: f32,
    /// Maximum items to display in validation warnings/errors.
    pub max_display_items: usize,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            min_evidence_quality: 0.6,
            min_evidence_confidence: 0.5,
            require_verifiable_evidence: true,
            verifiable_weight: 0.4,
            confidence_weight: 0.6,
            evidence_coverage_threshold: 0.5,
            risky_coverage_threshold: 0.3,
            max_display_items: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EvidenceConfig {
    pub max_relevant_files: usize,
    pub max_dependencies_display: usize,
    pub file_budget_percent: usize,
    pub pattern_budget_percent: usize,
    pub dependency_budget_percent: usize,
    pub knowledge_budget_percent: usize,
    pub min_budget_threshold: usize,
    pub min_confidence_display: f32,
    pub spec_evidence_budget: usize,
    pub plan_evidence_budget: usize,
    pub token_char_ratio: usize,
    pub skill_confidence: f32,
    pub rule_confidence: f32,
    pub markdown_truncation_buffer: usize,
    pub exclude_dirs: Vec<String>,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            max_relevant_files: 20,
            max_dependencies_display: 10,
            file_budget_percent: 40,
            pattern_budget_percent: 20,
            dependency_budget_percent: 20,
            knowledge_budget_percent: 20,
            min_budget_threshold: 100,
            min_confidence_display: 0.6,
            spec_evidence_budget: 8000,
            plan_evidence_budget: 50_000,
            token_char_ratio: 4,
            skill_confidence: 0.8,
            rule_confidence: 0.85,
            markdown_truncation_buffer: 50,
            exclude_dirs: vec![
                "node_modules".into(),
                "target".into(),
                ".git".into(),
                "dist".into(),
                "build".into(),
                "__pycache__".into(),
                ".venv".into(),
                "vendor".into(),
                "coverage".into(),
                ".next".into(),
                ".nuxt".into(),
                ".gradle".into(),
                "bin".into(),
                "obj".into(),
            ],
        }
    }
}

/// Configuration for task decomposition quality validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskDecompositionConfig {
    /// Minimum tasks per user story.
    pub min_tasks_per_story: usize,
    /// Maximum tasks per user story.
    pub max_tasks_per_story: usize,
    /// Task quality threshold (0.0-1.0).
    pub quality_threshold: f32,
    /// Maximum retry attempts on quality failure.
    pub max_retry_attempts: u32,
    /// Require affected_files to be non-empty.
    pub require_affected_files: bool,
}

impl Default for TaskDecompositionConfig {
    fn default() -> Self {
        Self {
            min_tasks_per_story: 2,
            max_tasks_per_story: 8,
            quality_threshold: 0.7,
            max_retry_attempts: 3,
            require_affected_files: true,
        }
    }
}

/// Configuration for search and retrieval features.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Enable search-based features (fix history, analysis index).
    pub enabled: bool,
    /// Directory name for search indexes (relative to pilot_dir).
    pub index_dir: String,
    /// Timeout in seconds for search operations.
    pub timeout_secs: u64,
    /// Include previous fix attempts in search context.
    pub include_previous_attempts: bool,
    /// Maximum tokens for related code in search results.
    pub max_related_code_tokens: usize,
    /// Maximum number of fix records to consider from history.
    pub max_fix_records: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            index_dir: "search".into(),
            timeout_secs: 10,
            include_previous_attempts: true,
            max_related_code_tokens: 2000,
            max_fix_records: 5,
        }
    }
}

/// Configuration for recovery checkpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckpointConfig {
    /// Interval in minutes between automatic checkpoints.
    pub interval_minutes: u64,
    /// Interval in tasks between automatic checkpoints.
    pub interval_tasks: u32,
    /// Maximum number of checkpoints to keep per mission.
    pub max_checkpoints: usize,
    /// Whether to persist evidence snapshot in checkpoints for durable recovery.
    /// Essential for long-running missions where evidence re-gathering is expensive.
    pub persist_evidence: bool,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            interval_minutes: 10,
            interval_tasks: 3,
            max_checkpoints: 5,
            persist_evidence: true,
        }
    }
}

/// Configuration for reasoning history persistence (durable execution).
/// Enables recovery from long interruptions with full decision context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReasoningConfig {
    /// Enable reasoning history persistence.
    pub enabled: bool,
    /// Retention days for reasoning history.
    pub retention_days: i64,
    /// Maximum hypotheses to keep in checkpoint context.
    pub max_hypotheses_in_context: usize,
    /// Maximum decisions to keep in checkpoint context.
    pub max_decisions_in_context: usize,
    /// Ratio of max_decisions allocated to recent decisions during compaction (0.0-1.0).
    /// The rest is allocated to high-impact decisions.
    /// Default 0.5 means 50% recent, 50% high-impact.
    pub recent_decisions_ratio: f32,
}

impl Default for ReasoningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
            max_hypotheses_in_context: 10,
            max_decisions_in_context: 20,
            recent_decisions_ratio: 0.5,
        }
    }
}

/// Configuration for complexity gate (Direct vs Planning decision).
/// Complexity assessment configuration.
/// All complexity decisions are LLM-based - no heuristic gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ComplexityConfig {
    pub assessment_timeout_secs: u64,
}

impl Default for ComplexityConfig {
    fn default() -> Self {
        Self {
            assessment_timeout_secs: 30,
        }
    }
}

/// Configuration for event-sourced state management.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    /// Enable event store for event sourcing.
    /// When enabled, all state changes are recorded as immutable events.
    pub event_store_enabled: bool,
    /// Interval for creating snapshots (number of events).
    /// Snapshots optimize read performance for state reconstruction.
    pub snapshot_interval: u32,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            event_store_enabled: true,
            snapshot_interval: 100,
        }
    }
}

impl ChunkedPlanningConfig {
    pub fn max_tokens_per_chunk(&self, model_config: &ModelConfig) -> usize {
        model_config.usable_context() as usize / self.max_tokens_divisor
    }

    pub fn complexity_threshold(&self, model_config: &ModelConfig) -> usize {
        model_config.usable_context() as usize / self.complexity_divisor
    }
}

impl Default for ChunkedPlanningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_user_stories_per_chunk: 3,
            max_requirements_per_chunk: 10,
            auto_chunk: true,
            max_tokens_divisor: 5,
            complexity_divisor: 3,
            min_phases: 2,
            max_phases: 5,
            max_stories_per_phase: 4,
        }
    }
}

#[derive(Clone)]
pub struct ProjectPaths {
    pub root: PathBuf,
    pub claude_dir: PathBuf,
    pub pilot_dir: PathBuf,
    pub missions_dir: PathBuf,
    pub wisdom_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub worktrees_dir: PathBuf,
}

impl ProjectPaths {
    pub fn new(root: PathBuf, config: &PilotConfig) -> Self {
        let claude_dir = root.join(".claude");
        let pilot_dir = claude_dir.join("pilot");

        Self {
            worktrees_dir: root.join(&config.isolation.worktrees_dir),
            root,
            missions_dir: pilot_dir.join("missions"),
            wisdom_dir: pilot_dir.join("wisdom"),
            logs_dir: pilot_dir.join("logs"),
            claude_dir,
            pilot_dir,
        }
    }

    pub async fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            &self.pilot_dir,
            &self.missions_dir,
            &self.wisdom_dir,
            &self.logs_dir,
        ];

        for dir in dirs {
            fs::create_dir_all(dir).await?;
        }

        Ok(())
    }

    pub fn agents_dir(&self) -> PathBuf {
        self.claude_dir.join("agents")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.claude_dir.join("skills")
    }

    pub fn rules_dir(&self) -> PathBuf {
        self.claude_dir.join("rules")
    }

    pub fn mission_log(&self, mission_id: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.log", mission_id))
    }
}

/// Configuration for Pattern Learning (Phase 2).
/// 4-stage pipeline: RETRIEVE → JUDGE → DISTILL → CONSOLIDATE
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PatternBankConfig {
    /// Enable pattern learning.
    pub enabled: bool,
    /// Minimum success rate for pattern retention (0.0-1.0).
    pub min_success_rate: f32,
    /// Minimum confidence for pattern extraction (0.0-1.0).
    pub min_confidence: f32,
    /// Interval in seconds for consolidation (cleanup) operation.
    pub consolidation_interval_secs: u64,
    /// Maximum patterns to retain in memory cache.
    pub max_patterns: usize,
    /// Days of inactivity before pattern is eligible for removal.
    pub inactive_days_threshold: u32,
    /// Maximum patterns to return from RETRIEVE.
    pub max_retrieve_results: usize,
    /// Minimum Jaccard similarity for pattern matching.
    pub min_similarity_threshold: f32,
    /// Weight for similarity in pattern retrieve scoring.
    pub similarity_weight: f32,
    /// Weight for success rate in pattern retrieve scoring.
    pub success_rate_weight: f32,
    /// Weight for recency in pattern retrieve scoring.
    pub recency_weight: f32,
    /// Weight for confidence in pattern retrieve scoring.
    pub confidence_weight: f32,
    /// Similarity threshold for merging patterns during consolidation (0.0-1.0).
    /// Patterns with similarity above this threshold may be merged.
    pub merge_similarity_threshold: f32,
    /// Penalty per additional attempt in trajectory scoring (0.0-1.0).
    /// Higher values penalize multi-attempt trajectories more heavily.
    pub trajectory_attempt_penalty: f32,
    /// Penalty per side effect in trajectory scoring (0.0-1.0).
    /// Higher values penalize trajectories with side effects more heavily.
    pub trajectory_side_effect_penalty: f32,
    /// Initial confidence for successful patterns (0.0-1.0).
    /// Higher values trust first success more.
    pub initial_success_confidence: f32,
    /// Initial confidence for failed patterns (0.0-1.0).
    /// Lower values are more conservative about failed approaches.
    pub initial_failure_confidence: f32,
    /// Weight of success rate in confidence calculation (0.0-1.0).
    /// Remaining weight goes to sample size factor.
    pub confidence_success_rate_weight: f32,
    /// Days before pattern confidence starts decaying.
    /// Patterns unused for longer than this lose confidence.
    pub recency_decay_days: u32,
    /// Decay rate per day after recency threshold (0.0-1.0).
    /// Applied as: confidence * (1 - decay_rate * days_over_threshold).
    pub recency_decay_rate: f32,
}

impl Default for PatternBankConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_success_rate: 0.3,
            min_confidence: 0.6,
            consolidation_interval_secs: 3600,
            max_patterns: 500,
            inactive_days_threshold: 30,
            max_retrieve_results: 5,
            min_similarity_threshold: 0.3,
            similarity_weight: 0.3,
            success_rate_weight: 0.3,
            recency_weight: 0.2,
            confidence_weight: 0.2,
            merge_similarity_threshold: 0.9,
            trajectory_attempt_penalty: 0.1,
            trajectory_side_effect_penalty: 0.1,
            // Confidence calculation parameters
            initial_success_confidence: 0.7,
            initial_failure_confidence: 0.3,
            confidence_success_rate_weight: 0.7, // 70% success rate, 30% sample size
            // Recency decay: patterns unused for 7+ days lose confidence
            recency_decay_days: 7,
            recency_decay_rate: 0.02, // 2% per day after threshold
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusConfig {
    pub enabled: bool,
    pub max_context_switches: u32,
    pub derive_from_plan: bool,
    pub derive_from_evidence: bool,
    pub evidence_confidence_threshold: f32,
    pub warn_on_unexpected_files: bool,
    pub strict_module_boundaries: bool,
}

impl Default for FocusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_context_switches: 3,
            derive_from_plan: true,
            derive_from_evidence: true,
            evidence_confidence_threshold: 0.7,
            warn_on_unexpected_files: true,
            strict_module_boundaries: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskScoringConfig {
    pub enabled: bool,
    pub weights: TaskScoringWeights,
    pub max_file_threshold: usize,
    pub max_dependency_threshold: usize,
    pub min_similarity_threshold: f32,
    pub max_history_tasks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskScoringWeights {
    pub file: f32,
    pub dependency: f32,
    pub pattern: f32,
    pub confidence: f32,
    pub history: f32,
}

/// Configuration for Multi-Agent Architecture (Phase 5).
/// Specialized agents with parallel execution and coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiAgentConfig {
    pub enabled: bool,
    pub dynamic_mode: bool,
    pub selection_policy: SelectionPolicy,
    pub parallel_execution: bool,
    pub instances: HashMap<String, usize>,
    pub coordinator_enabled: bool,
    pub max_concurrent_agents: usize,
    pub communication_timeout_secs: u64,
    pub consensus: ConsensusConfig,
    pub discovery: DiscoveryConfig,
    /// Maximum load per agent for scoring calculations.
    /// Used to normalize agent load scores in the selection policy.
    pub max_agent_load: u32,
    /// Number of consecutive clean verification rounds required for convergence.
    /// Default: 2 (NON-NEGOTIABLE baseline for quality).
    pub convergent_required_clean_rounds: usize,
    /// Maximum number of verification rounds before giving up.
    /// Default: 10.
    pub convergent_max_rounds: usize,
    /// Maximum number of fix attempts per issue before escalating.
    /// Default: 3.
    pub convergent_max_fix_attempts_per_issue: usize,
    /// Context compaction settings for long-running missions.
    pub context_compaction: CompactionConfig,
    /// Maximum number of deferred task retry rounds.
    /// Default: 3.
    pub max_deferred_retries: usize,
    /// Delay in milliseconds between deferred task retry attempts.
    /// Default: 500.
    pub deferred_retry_delay_ms: u64,
}

impl Default for MultiAgentConfig {
    fn default() -> Self {
        let mut instances = HashMap::new();
        instances.insert("research".to_string(), 1);
        instances.insert("planning".to_string(), 1);
        instances.insert("coder".to_string(), 2);
        instances.insert("verifier".to_string(), 1);

        Self {
            enabled: false,
            dynamic_mode: false,
            selection_policy: SelectionPolicy::RoundRobin,
            parallel_execution: true,
            instances,
            coordinator_enabled: true,
            max_concurrent_agents: 4,
            communication_timeout_secs: 30,
            consensus: ConsensusConfig::default(),
            discovery: DiscoveryConfig::default(),
            max_agent_load: 10,
            convergent_required_clean_rounds: 2,
            convergent_max_rounds: 10,
            convergent_max_fix_attempts_per_issue: 3,
            context_compaction: CompactionConfig::default(),
            max_deferred_retries: 3,
            deferred_retry_delay_ms: 500,
        }
    }
}

impl MultiAgentConfig {
    pub fn instance_count(&self, role_id: &str) -> usize {
        self.instances.get(role_id).copied().unwrap_or(1)
    }
}

/// Quorum type for consensus decisions.
///
/// This enum mirrors `agent::multi::shared::Quorum` but is defined separately
/// to avoid circular dependencies (config cannot depend on agent::multi).
/// Use `Quorum::from(quorum_type)` to convert at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuorumType {
    /// Simple majority (>50%)
    #[default]
    Majority,
    /// Supermajority (≥2/3)
    Supermajority,
    /// All participants must agree
    Unanimous,
    /// Unanimous with fallback to supermajority
    UnanimousWithFallback,
}

/// Per-tier quorum configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierQuorumConfig {
    /// Quorum for Module tier (default: Supermajority)
    pub module: QuorumType,
    /// Quorum for Group tier (default: Majority)
    pub group: QuorumType,
    /// Quorum for Domain tier (default: Majority)
    pub domain: QuorumType,
    /// Quorum for Workspace tier (default: UnanimousWithFallback)
    pub workspace: QuorumType,
    /// Quorum for CrossWorkspace tier (default: UnanimousWithFallback)
    pub cross_workspace: QuorumType,
}

impl Default for TierQuorumConfig {
    fn default() -> Self {
        Self {
            module: QuorumType::Supermajority,
            group: QuorumType::Majority,
            domain: QuorumType::Majority,
            workspace: QuorumType::UnanimousWithFallback,
            cross_workspace: QuorumType::UnanimousWithFallback,
        }
    }
}

/// Per-tier cross-visibility configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TierCrossVisibilityConfig {
    /// Enable cross-visibility for Module tier (default: true)
    pub module: bool,
    /// Enable cross-visibility for Group tier (default: false)
    pub group: bool,
    /// Enable cross-visibility for Domain tier (default: false)
    pub domain: bool,
    /// Enable cross-visibility for Workspace tier (default: true)
    pub workspace: bool,
    /// Enable cross-visibility for CrossWorkspace tier (default: true)
    pub cross_workspace: bool,
}

impl Default for TierCrossVisibilityConfig {
    fn default() -> Self {
        Self {
            module: true,
            group: false,
            domain: false,
            workspace: true,
            cross_workspace: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsensusConfig {
    pub max_rounds: usize,
    pub min_participants: usize,
    pub require_architecture: bool,

    // Adaptive strategy thresholds
    /// Maximum participants for flat consensus (≤ flat_threshold → flat)
    pub flat_threshold: usize,
    /// Minimum participants for hierarchical consensus (> hierarchical_threshold → hierarchical)
    pub hierarchical_threshold: usize,

    // Hierarchical execution limits
    /// Maximum participants per tier level
    pub max_participants_per_tier: usize,
    /// Maximum concurrent consensus groups
    pub max_concurrent_groups: usize,

    // Parallel execution
    /// Maximum concurrent LLM calls per consensus round
    pub max_concurrent_llm_calls: usize,
    /// Timeout per agent LLM call in seconds
    pub per_agent_timeout_secs: u64,
    /// Timeout for synthesis step in seconds
    pub synthesis_timeout_secs: u64,
    /// Extra buffer time added to calculated timeout
    pub timeout_buffer_secs: u64,

    // Timeouts (in seconds)
    /// Timeout for each round
    pub round_timeout_secs: u64,
    /// Timeout for each tier consensus
    pub tier_timeout_secs: u64,
    /// Total timeout for entire consensus process
    pub total_timeout_secs: u64,

    // Cross-visibility
    /// Enable cross-visibility mode where agents see peer proposals
    pub enable_cross_visibility: bool,
    /// Number of cross-visibility rounds before finalizing
    pub cross_visibility_rounds: usize,

    // P2P Conflict Resolution
    /// Enable peer-to-peer conflict resolution via MessageBus
    pub enable_p2p_conflict_resolution: bool,
    /// Timeout for P2P conflict resolution in seconds
    pub conflict_resolution_timeout_secs: u64,

    // Convergence
    /// Threshold for considering consensus converged
    pub convergence_threshold: f64,
    /// Minimum ratio of participants that must participate
    pub min_participation_ratio: f64,

    // Checkpointing
    /// Create checkpoint every N rounds
    pub checkpoint_interval: usize,
    /// Also checkpoint on tier completion
    pub checkpoint_on_tier_complete: bool,
    /// Maximum checkpoints to retain
    pub max_checkpoints: usize,

    // Recovery
    /// Enable automatic recovery from checkpoints
    pub auto_recovery: bool,
    /// Maximum recovery attempts
    pub max_recovery_attempts: usize,

    // Tier-specific settings
    /// Per-tier quorum requirements
    pub tier_quorum: TierQuorumConfig,
    /// Per-tier cross-visibility settings
    pub tier_cross_visibility: TierCrossVisibilityConfig,

    // Escalation
    pub escalation: EscalationConfig,
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            max_rounds: 3,
            min_participants: 2,
            require_architecture: true,

            // Adaptive strategy
            flat_threshold: 5,
            hierarchical_threshold: 15,

            // Hierarchical limits
            max_participants_per_tier: 10,
            max_concurrent_groups: 5,

            // Parallel execution
            max_concurrent_llm_calls: 5,
            per_agent_timeout_secs: 90,
            synthesis_timeout_secs: 60,
            timeout_buffer_secs: 120,

            // Timeouts
            round_timeout_secs: 120,
            tier_timeout_secs: 300,
            total_timeout_secs: 1800,

            // Cross-visibility
            enable_cross_visibility: true,
            cross_visibility_rounds: 2,

            // P2P Conflict Resolution
            enable_p2p_conflict_resolution: true,
            conflict_resolution_timeout_secs: 30,

            // Convergence
            convergence_threshold: 0.8,
            min_participation_ratio: 0.7,

            // Checkpointing
            checkpoint_interval: 2,
            checkpoint_on_tier_complete: true,
            max_checkpoints: 10,

            // Recovery
            auto_recovery: true,
            max_recovery_attempts: 3,

            // Tier-specific settings
            tier_quorum: TierQuorumConfig::default(),
            tier_cross_visibility: TierCrossVisibilityConfig::default(),

            // Escalation
            escalation: EscalationConfig::default(),
        }
    }
}

impl ConsensusConfig {
    /// Calculate dynamic timeout based on participant count and config settings.
    ///
    /// Formula: (batches × per_agent_timeout + synthesis_timeout) × rounds + buffer
    /// where batches = ceil(participants / max_concurrent_llm_calls)
    pub fn calculate_dynamic_timeout(&self, participant_count: usize) -> std::time::Duration {
        let concurrent = self.max_concurrent_llm_calls.max(1);
        let batches = participant_count.div_ceil(concurrent);

        let per_round =
            (batches as u64) * self.per_agent_timeout_secs + self.synthesis_timeout_secs;
        let total = per_round * (self.max_rounds as u64) + self.timeout_buffer_secs;
        let capped = total.min(self.total_timeout_secs);

        std::time::Duration::from_secs(capped)
    }

    /// Calculate per-tier timeout based on unit participant count.
    pub fn calculate_tier_timeout(&self, max_unit_participants: usize) -> std::time::Duration {
        let concurrent = self.max_concurrent_llm_calls.max(1);
        let batches = max_unit_participants.div_ceil(concurrent);

        let per_round =
            (batches as u64) * self.per_agent_timeout_secs + self.synthesis_timeout_secs;
        let with_buffer = per_round * (self.max_rounds as u64) + (self.timeout_buffer_secs / 2);

        std::time::Duration::from_secs(with_buffer.min(self.tier_timeout_secs))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EscalationConfig {
    /// Number of stalled rounds before escalating
    pub max_stalled_rounds: usize,
    /// Ratio of conflicting proposals that triggers escalation
    pub conflict_threshold: f64,
    /// Escalation level configurations
    pub retry_max_attempts: usize,
    pub scope_reduction_max_attempts: usize,
    pub mediation_max_attempts: usize,
    pub split_max_attempts: usize,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            max_stalled_rounds: 3,
            conflict_threshold: 0.4,
            retry_max_attempts: 3,
            scope_reduction_max_attempts: 2,
            mediation_max_attempts: 1,
            split_max_attempts: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    pub cache_enabled: bool,
    pub cache_path: String,
    pub cache_ttl_secs: u64,
    pub exclude_dirs: Vec<String>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            cache_enabled: true,
            cache_path: ".claude-pilot/cache/discovery.json".to_string(),
            cache_ttl_secs: 3600,
            exclude_dirs: vec![
                "node_modules".to_string(),
                "__pycache__".to_string(),
                "target".to_string(),
                "build".to_string(),
                ".git".to_string(),
                "dist".to_string(),
                ".venv".to_string(),
                "vendor".to_string(),
                "coverage".to_string(),
                ".next".to_string(),
                ".nuxt".to_string(),
                ".gradle".to_string(),
                "bin".to_string(),
                "obj".to_string(),
            ],
        }
    }
}

/// Agent selection policy for load balancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    #[default]
    RoundRobin,
    LeastLoaded,
    Random,
    /// Weighted scoring: capability * 0.30 + load * 0.20 + performance * 0.25 + health * 0.15 + availability * 0.10
    Scored,
}

// AgentInstancesConfig removed: replaced by HashMap<String, usize> in MultiAgentConfig

impl Default for TaskScoringConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            weights: TaskScoringWeights::default(),
            max_file_threshold: 20,
            max_dependency_threshold: 30,
            min_similarity_threshold: 0.3,
            max_history_tasks: 500,
        }
    }
}

impl Default for TaskScoringWeights {
    fn default() -> Self {
        Self {
            file: 0.20,
            dependency: 0.20,
            pattern: 0.25,
            confidence: 0.15,
            history: 0.20,
        }
    }
}

/// Token encoding strategy for context size estimation.
///
/// Note: Claude uses a proprietary tokenizer. These OpenAI-based
/// encodings are approximations. For most use cases, the difference
/// is negligible (< 5% error).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenEncoding {
    /// cl100k_base: Used by GPT-4, ChatGPT. Generally closer to modern LLMs.
    /// Recommended for Claude estimation.
    #[default]
    Cl100kBase,
    /// o200k_base: Used by GPT-4o, o1 series. Newest encoding.
    O200kBase,
    /// p50k_base: Used by GPT-3.
    P50kBase,
    /// Heuristic: Fast approximation using chars_per_token ratio.
    /// Use when exact counting isn't critical or tiktoken fails.
    Heuristic,
}

/// Configuration for token counting and estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenizerConfig {
    /// Encoding strategy for token estimation.
    pub encoding: TokenEncoding,
    /// Characters per token for heuristic mode.
    /// Also used as fallback when tiktoken fails.
    pub heuristic_chars_per_token: usize,
    /// Whether to use heuristic as fallback on tiktoken errors.
    pub fallback_to_heuristic: bool,
}

impl Default for TokenizerConfig {
    fn default() -> Self {
        Self {
            encoding: TokenEncoding::Cl100kBase,
            heuristic_chars_per_token: 4,
            fallback_to_heuristic: true,
        }
    }
}

/// Configuration for the rule system (artifact-architecture v3.0).
/// Rules represent domain knowledge (WHAT) that gets auto-injected based on context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Enable the rule system for context-aware knowledge injection.
    pub enabled: bool,
    /// Directory containing rules (relative to .claude/).
    pub rules_dir: String,
    /// Directory containing skills (relative to .claude/).
    pub skills_dir: String,
    /// Maximum rules to inject per request.
    pub max_rules_per_request: usize,
    /// Maximum combined rule content size (characters).
    pub max_rule_content_chars: usize,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules_dir: "rules".into(),
            skills_dir: "skills".into(),
            max_rules_per_request: 10,
            max_rule_content_chars: 50_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_build_system_allowed_bash_prefixes_cargo() {
        let prefixes = BuildSystem::Cargo.allowed_bash_prefixes();
        assert!(prefixes.contains(&"cargo"));
    }

    #[test]
    fn test_build_system_allowed_bash_prefixes_npm() {
        let prefixes = BuildSystem::Npm.allowed_bash_prefixes();
        assert!(prefixes.contains(&"npm"));
        assert!(prefixes.contains(&"npx"));
        assert!(prefixes.contains(&"node"));
    }

    #[test]
    fn test_build_system_allowed_bash_prefixes_python() {
        let prefixes = BuildSystem::Poetry.allowed_bash_prefixes();
        assert!(prefixes.contains(&"poetry"));
        assert!(prefixes.contains(&"python"));
        assert!(prefixes.contains(&"pytest"));
    }

    #[test]
    fn test_build_system_allowed_bash_prefixes_jvm() {
        let gradle = BuildSystem::Gradle.allowed_bash_prefixes();
        assert!(gradle.contains(&"./gradlew"));
        assert!(gradle.contains(&"gradle"));

        let maven = BuildSystem::Maven.allowed_bash_prefixes();
        assert!(maven.contains(&"mvn"));
        assert!(maven.contains(&"./mvnw"));
    }

    #[test]
    fn test_build_system_allowed_bash_prefixes_custom() {
        let cmake = BuildSystem::Custom("cmake".into()).allowed_bash_prefixes();
        assert!(cmake.contains(&"cmake"));
        assert!(cmake.contains(&"make"));
        assert!(cmake.contains(&"ninja"));

        let mix = BuildSystem::Custom("mix".into()).allowed_bash_prefixes();
        assert!(mix.contains(&"mix"));
        assert!(mix.contains(&"elixir"));
    }

    #[test]
    fn test_default_common_prefixes_includes_major_languages() {
        let prefixes = BuildSystem::default_common_prefixes();

        // Rust
        assert!(prefixes.contains(&"cargo"));
        // JavaScript
        assert!(prefixes.contains(&"npm"));
        assert!(prefixes.contains(&"yarn"));
        // Python
        assert!(prefixes.contains(&"python"));
        assert!(prefixes.contains(&"pytest"));
        // Go
        assert!(prefixes.contains(&"go"));
        // Java
        assert!(prefixes.contains(&"mvn"));
        assert!(prefixes.contains(&"gradle"));
        // Ruby
        assert!(prefixes.contains(&"bundle"));
        assert!(prefixes.contains(&"rake"));
        // PHP
        assert!(prefixes.contains(&"composer"));
        // .NET
        assert!(prefixes.contains(&"dotnet"));
        // Swift
        assert!(prefixes.contains(&"swift"));
    }

    #[test]
    fn test_verification_config_resolve_with_cargo_project() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let config = VerificationConfig::default();
        let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

        assert!(prefixes.contains(&"cargo".to_string()));
    }

    #[test]
    fn test_verification_config_resolve_with_additional() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let config = VerificationConfig {
            additional_allowed_commands: vec!["docker".into(), "kubectl".into()],
            allowed_commands_exclusive: false,
            ..Default::default()
        };
        let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

        // Should include both auto-detected and additional
        assert!(prefixes.contains(&"cargo".to_string()));
        assert!(prefixes.contains(&"docker".to_string()));
        assert!(prefixes.contains(&"kubectl".to_string()));
    }

    #[test]
    fn test_verification_config_resolve_exclusive() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let config = VerificationConfig {
            additional_allowed_commands: vec!["custom-build".into()],
            allowed_commands_exclusive: true, // Use only these, ignore auto-detected
            ..Default::default()
        };
        let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

        // Should only have custom, not auto-detected
        assert_eq!(prefixes, vec!["custom-build".to_string()]);
        assert!(!prefixes.contains(&"cargo".to_string()));
    }

    #[test]
    fn test_verification_config_resolve_no_build_system() {
        let temp = TempDir::new().unwrap();
        // Empty directory - no build system

        let config = VerificationConfig::default();
        let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

        // Should fall back to common defaults
        assert!(prefixes.contains(&"cargo".to_string()));
        assert!(prefixes.contains(&"npm".to_string()));
        assert!(prefixes.contains(&"python".to_string()));
    }

    #[test]
    fn test_validate_exclusive_without_commands_fails() {
        let mut config = PilotConfig::default();
        config.verification.allowed_commands_exclusive = true;
        config.verification.additional_allowed_commands = vec![]; // Empty

        let result = config.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("additional_allowed_commands must not be empty"));
    }

    #[test]
    fn test_validate_exclusive_with_commands_passes() {
        let mut config = PilotConfig::default();
        config.verification.allowed_commands_exclusive = true;
        config.verification.additional_allowed_commands = vec!["custom".into()];

        // Should pass - exclusive mode with commands is valid
        let result = config.validate();
        assert!(result.is_ok());
    }
}
