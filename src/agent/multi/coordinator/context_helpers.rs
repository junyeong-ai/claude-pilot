use std::path::Path;

use tracing::{debug, warn};

use super::super::traits::{AgentId, AgentTask, AgentTaskResult, TaskContext};
use super::Coordinator;

impl Coordinator {
    /// Enrich a task with manifest-derived module context.
    ///
    /// Finds modules matching the task's related files and builds a manifest
    /// context string containing module responsibilities, conventions, known
    /// issues, and dependency information.
    ///
    /// Returns the list of affected module IDs (for event emission).
    fn enrich_task_with_manifest(&self, task: &mut AgentTask) -> Vec<String> {
        let Some(ws) = &self.workspace else {
            return Vec::new();
        };

        let mut seen = std::collections::HashSet::new();
        let affected_modules: Vec<&modmap::Module> = task
            .context
            .related_files
            .iter()
            .filter_map(|f| ws.find_module_for_file(Path::new(f)))
            .filter(|m| seen.insert(m.id.clone()))
            .collect();

        if affected_modules.is_empty() {
            return Vec::new();
        }

        let module_ids: Vec<String> = affected_modules.iter().map(|m| m.id.clone()).collect();
        let context = Self::build_manifest_context(&affected_modules);
        task.context.manifest_context = Some(context);
        module_ids
    }

    fn build_manifest_context(modules: &[&modmap::Module]) -> String {
        let mut sections = Vec::new();
        sections.push("# Task Module Context\n".to_string());

        for module in modules {
            let mut section = format!("## Module: {}\n", module.name);
            section.push_str(&format!("**Responsibility**: {}\n", module.responsibility));

            if !module.conventions.is_empty() {
                section.push_str("**Conventions**:\n");
                for c in &module.conventions {
                    section.push_str(&format!("- {}: {}\n", c.name, c.pattern));
                }
            }

            if !module.known_issues.is_empty() {
                section.push_str("**Known Issues**:\n");
                for i in &module.known_issues {
                    section.push_str(&format!("- [{}] {}: {}\n", i.severity, i.id, i.description));
                }
            }

            if !module.dependencies.is_empty() {
                section.push_str("**Dependencies**: ");
                let deps: Vec<String> = module
                    .dependencies
                    .iter()
                    .map(|d| {
                        let dt = match d.dependency_type {
                            modmap::DependencyType::Runtime => "Runtime",
                            modmap::DependencyType::Build => "Build",
                            modmap::DependencyType::Test => "Test",
                            modmap::DependencyType::Optional => "Optional",
                        };
                        format!("{} ({})", d.module_id, dt)
                    })
                    .collect();
                section.push_str(&deps.join(", "));
                section.push('\n');
            }

            if !module.dependents.is_empty() {
                section.push_str(&format!("**Dependents**: {}\n", module.dependents.join(", ")));
            }

            sections.push(section);
        }

        sections.join("\n")
    }

    /// Enrich a task with manifest context and emit audit event.
    ///
    /// Unified entry point for manifest enrichment. All task execution paths
    /// should call this before pool.execute to ensure consistent context
    /// injection and audit trail.
    pub(super) async fn enrich_task_and_emit(&self, task: &mut AgentTask) {
        let module_ids = self.enrich_task_with_manifest(task);
        if !module_ids.is_empty() {
            let agent_id = task
                .role
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_else(|| "unassigned".to_string());
            self.emit_manifest_context_event(
                &task.context.mission_id,
                &agent_id,
                &task.id,
                &module_ids,
            )
            .await;
        }
    }

    pub(super) fn maybe_compact_task_context(&self, context: &mut TaskContext) {
        if let Some(compactor) = &self.context_compactor {
            let compactor = compactor.read();
            if compactor.compact_task_context(context) {
                debug!(
                    mission_id = %context.mission_id,
                    "TaskContext compacted due to threshold exceeded"
                );
            }
        }
    }

    pub(super) async fn execute_and_collect(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        context: &mut TaskContext,
        results: &mut Vec<AgentTaskResult>,
    ) {
        let agent_id = task
            .role
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| {
                warn!(task_id = %task.id, "Task missing role assignment, defaulting to coder");
                "coder".to_string()
            });

        let is_qualified = task
            .role
            .as_ref()
            .is_some_and(|r| r.is_qualified_module_coder());
        debug!(
            task_id = %task.id,
            target_agent = %agent_id,
            is_qualified_module = %is_qualified,
            workspace = ?task.role.as_ref().and_then(|r| r.workspace()),
            "Routing task to coder agent"
        );

        let mut task_with_context = task.clone();
        self.enrich_task_and_emit(&mut task_with_context).await;

        self.broadcast_task_assignment(&task_with_context, &agent_id)
            .await;

        let agent_id_typed = AgentId::new(&agent_id);
        let start_time = std::time::Instant::now();

        let execution_result = if let Some(bus) = &self.message_bus {
            self.pool
                .execute_with_messaging(&task_with_context, working_dir, bus)
                .await
        } else {
            self.pool.execute(&task_with_context, working_dir).await
        };

        let elapsed_ms = start_time.elapsed().as_millis() as u64;

        match execution_result {
            Ok(r) => {
                self.broadcast_task_result(&r, &agent_id).await;
                self.record_task_completion(&agent_id_typed, elapsed_ms);
                context.key_findings.extend(r.findings.clone());
                results.push(r);
            }
            Err(e) => {
                warn!(error = %e, task_id = %task.id, "Task execution failed");
                self.record_task_failure(&agent_id_typed);
                let failed_result = AgentTaskResult::failure(&task.id, e.to_string());
                self.broadcast_task_result(&failed_result, &agent_id).await;
                results.push(failed_result);
            }
        }
    }
}
