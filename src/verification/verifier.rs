//! Task verification with build, test, and lint checks.

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{Check, CheckResult, Verification, VerificationScope};
use crate::config::{VerificationCommands, VerificationConfig};
use crate::mission::{Task, TaskResult};

#[derive(Clone)]
pub struct Verifier {
    config: VerificationConfig,
}

impl Verifier {
    pub fn new(config: VerificationConfig) -> Self {
        Self { config }
    }

    async fn run_command(
        &self,
        check_type: Check,
        name: &str,
        cmd: &str,
        working_dir: &Path,
    ) -> CheckResult {
        let start = Instant::now();
        let timeout = Duration::from_secs(self.config.command_timeout_secs);
        debug!(check = %name, cmd = %cmd, dir = %working_dir.display(), "Running verification command");

        let mut command = self.build_shell_command(cmd, working_dir);
        let result = tokio::time::timeout(timeout, command.output()).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(output)) if output.status.success() => {
                debug!(check = %name, duration_ms, "Check passed");
                CheckResult::success(check_type, name).with_duration(duration_ms)
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                warn!(check = %name, "Check failed");
                CheckResult::failure(check_type, name, format!("{}\n{}", stdout, stderr).trim())
                    .with_duration(duration_ms)
            }
            Ok(Err(e)) => {
                warn!(check = %name, error = %e, "Execution error");
                CheckResult::failure(check_type, name, e.to_string()).with_duration(duration_ms)
            }
            Err(_) => {
                warn!(check = %name, timeout_secs = timeout.as_secs(), "Timed out");
                CheckResult::failure(
                    check_type,
                    name,
                    format!("{} timed out after {}s", name, timeout.as_secs()),
                )
                .with_duration(duration_ms)
            }
        }
    }

    #[cfg(windows)]
    fn build_shell_command(&self, cmd: &str, working_dir: &Path) -> Command {
        let mut command = Command::new("cmd");
        command.args(["/C", cmd]).current_dir(working_dir);
        command
    }

    #[cfg(not(windows))]
    fn build_shell_command(&self, cmd: &str, working_dir: &Path) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", cmd]).current_dir(working_dir);
        command
    }

    fn build_test_command(
        &self,
        base_cmd: &str,
        patterns: &[String],
        filter_format: Option<&String>,
    ) -> String {
        if patterns.is_empty() {
            return base_cmd.to_string();
        }
        let filter = patterns.join("|");
        match filter_format {
            Some(fmt) => format!("{} {}", base_cmd, fmt.replace("{}", &filter)),
            None => {
                warn!(
                    patterns = ?patterns,
                    "Test patterns ignored: test_filter_format not configured"
                );
                base_cmd.to_string()
            }
        }
    }

    fn verification_commands(&self, task: &Task) -> VerificationCommands {
        self.config.commands_for_module(task.module.as_deref())
    }

    pub async fn verify_task(
        &self,
        task: &Task,
        result: &TaskResult,
        working_dir: &Path,
    ) -> Verification {
        debug!(task_id = %task.id, module = ?task.module, "Verifying task");
        let mut checks = Vec::new();

        // Get commands from config or auto-detect
        let config_commands = self.verification_commands(task);
        let commands = if config_commands.has_any() {
            config_commands
        } else if let Some(build_system) = crate::config::BuildSystem::detect(working_dir) {
            build_system.commands()
        } else {
            config_commands
        };

        for file in &result.files_created {
            let path = working_dir.join(file);
            checks.push(if path.exists() {
                CheckResult::success(Check::FileCreated, format!("Created: {}", file))
            } else {
                CheckResult::failure(
                    Check::FileCreated,
                    format!("Expected: {}", file),
                    "Not created",
                )
            });
        }

        for file in &result.files_modified {
            let path = working_dir.join(file);
            checks.push(if path.exists() {
                CheckResult::success(Check::FileModified, format!("Modified: {}", file))
            } else {
                CheckResult::failure(
                    Check::FileModified,
                    format!("Expected: {}", file),
                    "Missing after modification",
                )
            });
        }

        let has_changes = !result.files_created.is_empty() || !result.files_modified.is_empty();
        let should_verify = self.config.verify_on_task_complete && has_changes;

        if should_verify {
            if let Some(cmd) = &commands.build_cmd {
                checks.push(
                    self.run_command(Check::Build, "Build", cmd, working_dir)
                        .await,
                );
            }
            if let Some(cmd) = &commands.test_cmd {
                let test_cmd = self.build_test_command(
                    cmd,
                    &task.test_patterns,
                    commands.test_filter_format.as_ref(),
                );
                checks.push(
                    self.run_command(Check::Test, "Tests", &test_cmd, working_dir)
                        .await,
                );
            }
            if let Some(cmd) = &commands.lint_cmd {
                checks.push(
                    self.run_command(Check::Lint, "Lint", cmd, working_dir)
                        .await,
                );
            }
        } else if self.config.verify_on_task_complete && !has_changes && result.success {
            checks.push(CheckResult::success(
                Check::Custom,
                "No file changes expected",
            ));
        }

        let verification = Verification::new(checks);
        info!(task_id = %task.id, passed = verification.passed, checks = verification.checks.len(), "Verification complete");
        verification
    }

    pub async fn verify_full(&self, working_dir: &Path) -> Verification {
        info!("Running full verification");
        let mut checks = Vec::new();

        // Try config commands first, then auto-detect from working directory
        let effective = self.config.effective_full_commands();
        let commands = if effective.has_any() {
            effective
        } else if let Some(build_system) = crate::config::BuildSystem::detect(working_dir) {
            info!(build_system = ?build_system, "Auto-detected build system for verification");
            build_system.commands()
        } else {
            effective
        };

        if commands.has_any() {
            if let Some(cmd) = &commands.build_cmd {
                checks.push(
                    self.run_command(Check::Build, "Build", cmd, working_dir)
                        .await,
                );
            }
            if let Some(cmd) = &commands.test_cmd {
                checks.push(
                    self.run_command(Check::Test, "All Tests", cmd, working_dir)
                        .await,
                );
            }
            if let Some(cmd) = &commands.lint_cmd {
                checks.push(
                    self.run_command(Check::Lint, "Lint", cmd, working_dir)
                        .await,
                );
            }
        } else {
            for (module_name, module_commands) in self.config.all_module_commands() {
                if let Some(cmd) = &module_commands.build_cmd {
                    let name = format!("Build ({})", module_name);
                    checks.push(
                        self.run_command(Check::Build, &name, cmd, working_dir)
                            .await,
                    );
                }
                if let Some(cmd) = &module_commands.test_cmd {
                    let name = format!("Tests ({})", module_name);
                    checks.push(self.run_command(Check::Test, &name, cmd, working_dir).await);
                }
                if let Some(cmd) = &module_commands.lint_cmd {
                    let name = format!("Lint ({})", module_name);
                    checks.push(self.run_command(Check::Lint, &name, cmd, working_dir).await);
                }
            }
        }

        if checks.is_empty() {
            if self.config.require_verification_commands {
                warn!("No verification commands configured or detected");
                checks.push(CheckResult::failure(
                    Check::Custom,
                    "Verification commands required",
                    "No build_cmd, test_cmd, or lint_cmd configured and auto-detection failed. Configure commands or set require_verification_commands=false.",
                ));
            } else {
                checks.push(CheckResult::success(
                    Check::Custom,
                    "No verification commands configured (skipped)",
                ));
            }
        }

        let verification = Verification::new(checks);
        info!(passed = verification.passed, summary = %verification.summary, "Full verification complete");
        verification
    }

    pub async fn verify_scoped(
        &self,
        scope: &VerificationScope,
        working_dir: &Path,
    ) -> Verification {
        match scope {
            VerificationScope::Full => self.verify_full(working_dir).await,
            VerificationScope::TaskScoped {
                test_patterns,
                module,
            } => {
                self.verify_task_scoped(test_patterns, module.as_deref(), working_dir)
                    .await
            }
        }
    }

    async fn verify_task_scoped(
        &self,
        test_patterns: &[String],
        module: Option<&str>,
        working_dir: &Path,
    ) -> Verification {
        debug!(module = ?module, patterns = ?test_patterns, "Running task-scoped verification");
        let mut checks = Vec::new();

        let commands = self.config.commands_for_module(module);
        let effective = if commands.has_any() {
            commands
        } else if let Some(build_system) = crate::config::BuildSystem::detect(working_dir) {
            build_system.commands()
        } else {
            commands
        };

        if let Some(cmd) = &effective.build_cmd {
            checks.push(
                self.run_command(Check::Build, "Build", cmd, working_dir)
                    .await,
            );
        }

        if let Some(cmd) = &effective.test_cmd {
            let test_cmd =
                self.build_test_command(cmd, test_patterns, effective.test_filter_format.as_ref());
            let name = if test_patterns.is_empty() {
                "Tests"
            } else {
                "Tests (scoped)"
            };
            checks.push(
                self.run_command(Check::Test, name, &test_cmd, working_dir)
                    .await,
            );
        }

        if let Some(cmd) = &effective.lint_cmd {
            checks.push(
                self.run_command(Check::Lint, "Lint", cmd, working_dir)
                    .await,
            );
        }

        // Handle empty checks case - must match verify_full behavior
        if checks.is_empty() {
            if self.config.require_verification_commands {
                let module_hint = module
                    .map(|m| format!(" for module '{}'", m))
                    .unwrap_or_default();
                warn!(
                    "No verification commands configured or detected{}",
                    module_hint
                );
                checks.push(CheckResult::failure(
                    Check::Custom,
                    "Verification commands required",
                    format!(
                        "No build_cmd, test_cmd, or lint_cmd configured{} and auto-detection failed. \
                        Configure commands in config.toml or set require_verification_commands=false.",
                        module_hint
                    ),
                ));
            } else {
                checks.push(CheckResult::success(
                    Check::Custom,
                    "No verification commands configured (skipped)",
                ));
            }
        }

        let verification = Verification::new(checks);
        info!(passed = verification.passed, summary = %verification.summary, "Task-scoped verification complete");
        verification
    }
}
