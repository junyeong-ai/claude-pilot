use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::build_system::BuildSystem;
use super::lint_detector::LintDetector;

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
            command_timeout_secs: 300,
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
            return self.additional_allowed_commands.clone();
        }

        let mut prefixes: Vec<String> = if let Some(build_system) = BuildSystem::detect(working_dir)
        {
            build_system
                .allowed_bash_prefixes()
                .into_iter()
                .map(String::from)
                .collect()
        } else {
            BuildSystem::default_common_prefixes()
                .into_iter()
                .map(String::from)
                .collect()
        };

        for cmd in &self.additional_allowed_commands {
            if !prefixes.contains(cmd) {
                prefixes.push(cmd.clone());
            }
        }

        prefixes
    }

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
                LintDetector::detect(working_dir, &build_system)
            });
            if self.test_filter_format.is_none() {
                self.test_filter_format = commands.test_filter_format;
            }
        }
        self
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
