use std::io::{self, Write};

use serde::Serialize;

use crate::cli::OutputFormat;
use crate::mission::Mission;

/// Thread-safe output writer that handles different output formats.
///
/// Supports three output modes:
/// - Text: Human-readable formatted output (default)
/// - Json: Single JSON object at completion
/// - Stream: NDJSON streaming (one JSON object per line)
pub struct OutputWriter {
    format: OutputFormat,
}

impl OutputWriter {
    pub fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    /// Returns the configured output format.
    pub fn format(&self) -> OutputFormat {
        self.format
    }

    /// Emit the final result.
    pub fn emit_result(&self, result: &MissionOutput) {
        match self.format {
            OutputFormat::Text => {
                self.print_text_result(result);
            }
            OutputFormat::Json | OutputFormat::Stream => {
                self.write_json(result);
            }
        }
    }

    /// Emit mission status.
    pub fn emit_status(&self, mission: &Mission) {
        match self.format {
            OutputFormat::Text => {
                self.print_mission_status(mission);
            }
            OutputFormat::Json | OutputFormat::Stream => {
                let status = MissionStateOutput::from(mission);
                self.write_json(&status);
            }
        }
    }

    /// Emit mission list.
    pub fn emit_list(&self, missions: &[Mission]) {
        match self.format {
            OutputFormat::Text => {
                self.print_mission_list(missions);
            }
            OutputFormat::Json | OutputFormat::Stream => {
                let list: Vec<MissionStateOutput> =
                    missions.iter().map(MissionStateOutput::from).collect();
                self.write_json(&list);
            }
        }
    }

    /// Emit a simple message.
    pub fn emit_message(&self, message: &str) {
        match self.format {
            OutputFormat::Text => {
                println!("{}", message);
            }
            OutputFormat::Json | OutputFormat::Stream => {
                let msg = MessageOutput {
                    message: message.to_string(),
                };
                self.write_json(&msg);
            }
        }
    }

    fn write_json<T: Serialize>(&self, value: &T) {
        if let Ok(json) = serde_json::to_string(value) {
            let mut stdout = io::stdout().lock();
            let _ = writeln!(stdout, "{}", json);
            let _ = stdout.flush();
        }
    }

    fn print_text_result(&self, result: &MissionOutput) {
        println!();
        if result.success {
            println!("Mission {} completed successfully!", result.mission_id);
        } else {
            println!("Mission {} failed.", result.mission_id);
        }
        println!();
        println!("Summary: {}", result.summary);

        if !result.files_changed.is_empty() {
            println!();
            println!("Files changed ({}):", result.files_changed.len());
            for file in &result.files_changed {
                println!("  {}", file);
            }
        }

        if !result.learnings.is_empty() {
            println!();
            println!("Learnings ({}):", result.learnings.len());
            for learning in &result.learnings {
                println!("  - {}", learning.content);
            }
        }
    }

    fn print_mission_status(&self, mission: &Mission) {
        println!();
        println!("Mission: {}", mission.id);
        println!("{}", "=".repeat(60));
        println!();
        println!("Description: {}", mission.description);
        println!("Status: {}", mission.status);
        println!("Isolation: {}", mission.isolation);

        if let Some(branch) = &mission.branch {
            println!("Branch: {}", branch);
        }

        let progress = mission.progress();
        println!();
        println!(
            "Progress: {}% ({}/{})",
            progress.percentage, progress.completed, progress.total
        );

        if !mission.tasks.is_empty() {
            println!();
            println!("Tasks:");
            for task in &mission.tasks {
                let status_icon = match task.status {
                    crate::mission::TaskStatus::Completed => "[x]",
                    crate::mission::TaskStatus::InProgress => "[>]",
                    crate::mission::TaskStatus::Failed => "[!]",
                    _ => "[ ]",
                };
                println!("  {} {} - {}", status_icon, task.id, task.description);
            }
        }
    }

    fn print_mission_list(&self, missions: &[Mission]) {
        if missions.is_empty() {
            println!("No missions found.");
            return;
        }

        println!();
        println!(
            "{:<10} {:<40} {:<12} {:>10}",
            "ID", "Description", "Status", "Progress"
        );
        println!("{}", "-".repeat(75));

        for mission in missions {
            let desc = crate::utils::truncate_at_boundary(&mission.description, 38);

            let progress = mission.progress();
            let progress_str = format!("{}%", progress.percentage);

            println!(
                "{:<10} {:<40} {:<12} {:>10}",
                mission.id, desc, mission.status, progress_str
            );
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MissionOutput {
    pub success: bool,
    pub mission_id: String,
    pub summary: String,
    pub files_changed: Vec<String>,
    pub learnings: Vec<LearningOutput>,
}

impl MissionOutput {
    pub fn from_mission(mission: &Mission, success: bool) -> Self {
        let files_changed: Vec<String> = mission
            .tasks
            .iter()
            .flat_map(|t| {
                t.result
                    .as_ref()
                    .map(|r| {
                        r.files_created
                            .iter()
                            .chain(r.files_modified.iter())
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect();

        let learnings: Vec<LearningOutput> = mission
            .learnings
            .iter()
            .map(|l| LearningOutput {
                category: l.category.to_string(),
                content: l.content.clone(),
            })
            .collect();

        Self {
            success,
            mission_id: mission.id.clone(),
            summary: mission.description.clone(),
            files_changed,
            learnings,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningOutput {
    pub category: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
struct MissionStateOutput {
    id: String,
    description: String,
    status: String,
    isolation: String,
    branch: Option<String>,
    progress: ProgressInfo,
    tasks: Vec<TaskInfo>,
}

impl From<&Mission> for MissionStateOutput {
    fn from(mission: &Mission) -> Self {
        let progress = mission.progress();
        Self {
            id: mission.id.clone(),
            description: mission.description.clone(),
            status: mission.status.to_string(),
            isolation: mission.isolation.to_string(),
            branch: mission.branch.clone(),
            progress: ProgressInfo {
                completed: progress.completed,
                total: progress.total,
                percentage: progress.percentage,
            },
            tasks: mission
                .tasks
                .iter()
                .map(|t| TaskInfo {
                    id: t.id.clone(),
                    description: t.description.clone(),
                    status: t.status.to_string(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProgressInfo {
    completed: usize,
    total: usize,
    percentage: u8,
}

#[derive(Debug, Clone, Serialize)]
struct TaskInfo {
    id: String,
    description: String,
    status: String,
}

#[derive(Debug, Clone, Serialize)]
struct MessageOutput {
    message: String,
}
