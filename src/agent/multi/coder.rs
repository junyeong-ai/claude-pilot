//! Coder agent for implementation with P2P conflict handling via MessageBus.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

use super::messaging::{
    AgentMessage, AgentMessageBus, FilteredReceiver, MessagePayload, MessageType,
};
#[cfg(test)]
use super::traits::extract_file_path;
use super::traits::{
    AgentCore, AgentPromptBuilder, AgentRole, AgentTask, AgentTaskResult, ArtifactType,
    SpecializedAgent, TaskArtifact, extract_files_from_output,
};
use crate::agent::TaskAgent;
use crate::agent::multi::AgentIdentifier;
use crate::error::Result;

const CODER_SYSTEM_PROMPT: &str = r"# Coder Agent

You are a specialized coding agent focused on implementation.

## Primary Responsibilities
- Implement features according to plan
- Fix bugs and issues
- Refactor code when needed
- Write clean, maintainable code
- Follow existing patterns and conventions

## Implementation Principles
1. Minimal changes: Only modify what's necessary
2. Follow patterns: Match existing code style
3. No regressions: Ensure existing tests pass
4. Testable: Write code that can be tested
5. Documented: Add comments for complex logic

## Output Requirements
- Complete, working code
- Clear commit message describing changes
- List of files modified/created
- Any follow-up tasks needed

## Constraints
- Do not over-engineer
- Do not add unnecessary dependencies
- Do not modify unrelated code
- Ensure builds pass before completion
";

pub struct CoderAgent {
    core: AgentCore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2PConflictAction {
    Proceed,
    Yield { to_agent: String },
}

impl CoderAgent {
    pub fn new(id: String, task_agent: Arc<TaskAgent>) -> Self {
        Self {
            core: AgentCore::new(id, AgentRole::core_coder(), task_agent),
        }
    }

    pub fn with_id(id: &str, task_agent: Arc<TaskAgent>) -> Arc<Self> {
        Arc::new(Self::new(id.to_string(), task_agent))
    }

    pub async fn execute_with_messaging(
        &self,
        task: &AgentTask,
        working_dir: &Path,
        message_bus: &AgentMessageBus,
    ) -> Result<AgentTaskResult> {
        let mut receiver =
            message_bus.subscribe_filtered(self.id(), vec![MessageType::ConflictAlert]);

        if let Some(resolution) = self.check_conflicts(&mut receiver) {
            match resolution {
                P2PConflictAction::Yield { to_agent } => {
                    warn!(
                        agent = %self.id(),
                        yielding_to = %to_agent,
                        "Yielding due to file conflict - task will be retried"
                    );
                    return Ok(AgentTaskResult::deferred(
                        &task.id,
                        format!("Deferred: yielded to {} due to file conflict", to_agent),
                    ));
                }
                P2PConflictAction::Proceed => {}
            }
        }

        self.execute(task, working_dir).await
    }

    fn check_conflicts(&self, receiver: &mut FilteredReceiver) -> Option<P2PConflictAction> {
        match receiver.try_recv() {
            Ok(Some(msg)) => {
                if let MessagePayload::ConflictAlert {
                    conflict_id,
                    description,
                    ..
                } = &msg.payload
                {
                    debug!(
                        conflict_id = %conflict_id,
                        description = %description,
                        from = %msg.from,
                        "Received conflict alert"
                    );
                    if self.should_yield(&msg) {
                        return Some(P2PConflictAction::Yield {
                            to_agent: msg.from.clone(),
                        });
                    }
                    Some(P2PConflictAction::Proceed)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn should_yield(&self, conflict_msg: &AgentMessage) -> bool {
        self.id() > conflict_msg.from.as_str()
    }

    fn build_coding_prompt(&self, task: &AgentTask) -> String {
        let system_prompt = task.context.composed_prompt.as_deref().unwrap_or_else(|| {
            debug!(
                task_id = %task.id,
                "No composed_prompt in task context, falling back to default CODER_SYSTEM_PROMPT"
            );
            CODER_SYSTEM_PROMPT
        });

        AgentPromptBuilder::new(system_prompt, "Implementation", task)
            .with_context(task)
            .with_related_files(task)
            .with_section(
                "Requirements",
                &[
                    "1. Implement the changes as specified",
                    "2. Follow existing patterns and conventions",
                    "3. Ensure the code compiles",
                    "4. Run tests to verify no regressions",
                ],
            )
            .build()
    }

    fn extract_modified_files(&self, output: &str) -> Vec<String> {
        extract_files_from_output(output, 20)
    }
}

#[async_trait]
impl SpecializedAgent for CoderAgent {
    fn role(&self) -> &AgentRole {
        self.core.role()
    }

    fn id(&self) -> &str {
        self.core.id()
    }

    fn identifier(&self) -> AgentIdentifier {
        self.core.identifier().clone()
    }

    fn system_prompt(&self) -> &str {
        CODER_SYSTEM_PROMPT
    }

    async fn execute(&self, task: &AgentTask, working_dir: &Path) -> Result<AgentTaskResult> {
        let _guard = self.core.begin_execution();

        debug!(task_id = %task.id, "Coder agent executing");

        let prompt = self.build_coding_prompt(task);
        let output = self
            .core
            .task_agent
            .run_with_profile(&prompt, working_dir, self.role().permission_profile())
            .await;

        match output {
            Ok(output) => {
                let files = self.extract_modified_files(&output);
                Ok(AgentTaskResult::success(&task.id, output.clone())
                    .with_findings(files)
                    .with_artifacts(vec![TaskArtifact {
                        name: format!("impl-{}.md", task.id),
                        content: output,
                        artifact_type: ArtifactType::Code,
                    }]))
            }
            Err(e) => Ok(AgentTaskResult::failure(&task.id, e.to_string())),
        }
    }

    fn current_load(&self) -> u32 {
        self.core.load.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::multi::consensus::ConflictSeverity;

    #[test]
    fn test_extract_file_path() {
        assert_eq!(
            extract_file_path("Modified: src/auth/login.rs"),
            Some("src/auth/login.rs".to_string())
        );
        assert_eq!(
            extract_file_path("Created file `src/utils.rs`"),
            Some("src/utils.rs".to_string())
        );
        assert!(extract_file_path("No file here").is_none());
    }

    #[test]
    fn test_extract_modified_files() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = CoderAgent::new("test".to_string(), task_agent);

        let output = r#"
I've made the following changes:
- Modified: src/auth.rs
- Created: src/utils/helper.rs
Updated src/lib.rs with new exports
"#;

        let files = agent.extract_modified_files(output);
        assert!(!files.is_empty());
    }

    #[test]
    fn test_should_yield_deterministic() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent_a = CoderAgent::new("agent-a".to_string(), task_agent.clone());
        let agent_b = CoderAgent::new("agent-b".to_string(), task_agent);

        let msg_from_a = AgentMessage::new(
            "agent-a",
            "agent-b",
            MessagePayload::ConflictAlert {
                conflict_id: "c1".into(),
                severity: ConflictSeverity::Blocking,
                description: "test".into(),
            },
        );

        let msg_from_b = AgentMessage::new(
            "agent-b",
            "agent-a",
            MessagePayload::ConflictAlert {
                conflict_id: "c1".into(),
                severity: ConflictSeverity::Blocking,
                description: "test".into(),
            },
        );

        assert!(!agent_a.should_yield(&msg_from_b));
        assert!(agent_b.should_yield(&msg_from_a));
    }

    #[test]
    fn test_check_conflicts_no_pending() {
        let task_agent = Arc::new(TaskAgent::new(Default::default()));
        let agent = CoderAgent::new("test-coder".to_string(), task_agent);
        let bus = AgentMessageBus::default();

        let mut receiver = bus.subscribe_filtered("test-coder", vec![MessageType::ConflictAlert]);
        let conflict: Option<P2PConflictAction> = agent.check_conflicts(&mut receiver);

        assert!(conflict.is_none());
    }
}
