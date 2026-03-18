use std::path::PathBuf;

use tracing::{debug, warn};

use crate::domain::Severity;
use super::super::consensus::ConsensusTask;
use super::super::ownership::{AccessType, AcquisitionResult, DeferredTaskRef, LeaseGuard};
use super::super::shared::AgentId;
use super::super::traits::TaskContext;
use super::Coordinator;

impl Coordinator {
    pub(super) async fn acquire_ownership_for_tasks<'a>(
        &self,
        tasks: &[&'a ConsensusTask],
        context: &TaskContext,
    ) -> (
        Vec<(&'a ConsensusTask, LeaseGuard)>,
        Vec<(&'a ConsensusTask, String)>,
    ) {
        let mut tasks_with_ownership = Vec::new();
        let mut tasks_deferred = Vec::new();

        for task in tasks {
            let files: Vec<PathBuf> = task.files_affected.iter().map(PathBuf::from).collect();
            let module = task.assigned_module.as_deref();

            match self.ownership_manager.acquire_ordered(
                &files,
                &task.id,
                module,
                AccessType::Exclusive,
                &format!("Task {} implementation", task.id),
            ) {
                Ok(guard) => {
                    tasks_with_ownership.push((*task, guard));
                }
                Err(result) => {
                    // For NeedsNegotiation, send P2P release requests via ConflictResolver
                    if let AcquisitionResult::NeedsNegotiation { ref conflicting_modules } = result
                        && let Some(resolver) = &self.conflict_resolver
                    {
                        let requester = AgentId::new(&task.id);
                        debug!(
                            task_id = %task.id,
                            modules = ?conflicting_modules,
                            "Sending P2P release requests for conflict resolution"
                        );
                        for file in &files {
                            if let Some(owner_str) = self.ownership_manager.current_owner(file) {
                                let owner = AgentId::new(&owner_str);
                                let priority = (task.priority.weight() * 10.0) as u8;
                                let _ = resolver.request_release(
                                    file,
                                    &requester,
                                    &owner,
                                    priority,
                                    &format!("Task {} needs file", task.id),
                                );
                            }
                        }
                    }

                    let denial_reason = match &result {
                        AcquisitionResult::Denied { reason, owner_module } => {
                            owner_module
                                .as_ref()
                                .map(|owner| format!("Module boundary: owned by {}", owner))
                                .unwrap_or_else(|| reason.clone())
                        }
                        AcquisitionResult::Queued { position, estimated_wait } => {
                            format!("Queued at position {} (~{:?} wait)", position, estimated_wait)
                        }
                        AcquisitionResult::NeedsNegotiation { conflicting_modules } => {
                            format!("Negotiation needed: {:?}", conflicting_modules)
                        }
                        AcquisitionResult::Granted { .. } => {
                            // Logically unreachable in Err path — defensive fallback
                            warn!(task_id = %task.id, "Unexpected Granted in acquisition error path");
                            continue;
                        }
                    };

                    if matches!(
                        &result,
                        AcquisitionResult::Denied { .. } | AcquisitionResult::NeedsNegotiation { .. }
                    ) {
                        let severity = if matches!(&result, AcquisitionResult::NeedsNegotiation { .. }) {
                            Severity::Error
                        } else {
                            Severity::Critical
                        };
                        if let Some(file) = files.first() {
                            self.broadcast_conflict_alert(file, &denial_reason, severity).await;
                        }
                    }

                    warn!(
                        task_id = %task.id,
                        mission_id = %context.mission_id,
                        reason = %denial_reason,
                        "Task deferred due to ownership issue"
                    );

                    for file in &task.files_affected {
                        self.ownership_manager.register_deferred_task(
                            std::path::Path::new(file),
                            DeferredTaskRef {
                                task_id: task.id.clone(),
                            },
                        );
                    }
                    tasks_deferred.push((*task, denial_reason));
                }
            }
        }

        (tasks_with_ownership, tasks_deferred)
    }

    /// Process pending conflict requests and auto-grant releases for completed tasks.
    pub(super) fn process_pending_conflicts(&self, completed_task_ids: &[String]) {
        let Some(resolver) = &self.conflict_resolver else {
            return;
        };

        for task_id in completed_task_ids {
            let agent_id = AgentId::new(task_id);
            let requests = match resolver.process_pending_requests(&agent_id) {
                Ok(reqs) => reqs,
                Err(e) => {
                    debug!(task_id = %task_id, error = %e, "Failed to process conflict requests");
                    continue;
                }
            };

            for (msg, request) in requests {
                if let Some(ownership) = self.ownership_manager.ownership.get(&request.file) {
                    if ownership.owner == task_id.as_str() {
                        let version = ownership.version;
                        drop(ownership);
                        match resolver.grant_and_transfer(
                            &request.file,
                            task_id,
                            version,
                            &request.requester,
                            None,
                            request.access_type,
                            &request.reason,
                        ) {
                            Ok(_lease) => {
                                debug!(
                                    file = %request.file.display(),
                                    from = %task_id,
                                    to = %request.requester,
                                    "Ownership transferred via conflict resolution"
                                );
                            }
                            Err(e) => {
                                debug!(error = %e, "Ownership transfer failed, will retry");
                            }
                        }
                    } else {
                        drop(ownership);
                        let _ = resolver.respond_to_request(&msg.id, true, None);
                    }
                } else {
                    let _ = resolver.respond_to_request(&msg.id, true, None);
                }
            }
        }
    }
}
