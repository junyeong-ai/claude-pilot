use console::{Style, style};
use indicatif::{ProgressBar, ProgressStyle};

use crate::mission::{Mission, TaskStatus};
use crate::state::MissionState;
use crate::utils::truncate_chars;

pub struct Display;

impl Display {
    pub fn new() -> Self {
        Self
    }

    pub fn print_header(&self, text: &str) {
        println!();
        println!("{}", style(text).bold().cyan());
        println!("{}", style("═".repeat(60)).dim());
        println!();
    }

    pub fn print_mission_summary(&self, mission: &Mission) {
        let status_style = self.status_style(mission.status);
        let progress = mission.progress();

        println!(
            "{}  {}",
            style(&mission.id).bold(),
            style(&mission.description).white()
        );
        println!(
            "    Status: {}  Progress: {}",
            status_style.apply_to(mission.status.to_string()),
            self.progress_bar_inline(progress.percentage)
        );

        if let Some(branch) = &mission.branch {
            println!("    Branch: {}", style(branch).dim());
        }

        if let Some(worktree) = &mission.worktree_path {
            println!("    Worktree: {}", style(worktree.display()).dim());
        }

        println!();
    }

    pub fn print_mission_detail(&self, mission: &Mission) {
        self.print_header(&format!("Mission: {}", mission.id));

        let status_style = self.status_style(mission.status);
        let progress = mission.progress();

        println!(
            "Description: {}",
            style(&mission.description).white().bold()
        );
        println!(
            "Status:      {}",
            status_style.apply_to(mission.status.to_string())
        );
        println!("Isolation:   {}", mission.isolation);
        println!("Priority:    {}", mission.priority);
        println!("On Complete: {}", mission.on_complete);

        if let Some(branch) = &mission.branch {
            println!("Branch:      {}", branch);
        }

        if let Some(worktree) = &mission.worktree_path {
            println!("Worktree:    {}", worktree.display());
        }

        println!();
        println!(
            "Progress: {} {}% ({}/{})",
            self.progress_bar(progress.percentage, 30),
            progress.percentage,
            progress.completed,
            progress.total
        );
        println!();

        if !mission.phases.is_empty() {
            println!("{}", style("Phases:").bold());
            for phase in &mission.phases {
                let phase_tasks: Vec<_> = mission
                    .tasks
                    .iter()
                    .filter(|t| phase.task_ids.contains(&t.id))
                    .collect();

                let completed = phase_tasks
                    .iter()
                    .filter(|t| t.status == TaskStatus::Completed)
                    .count();
                let total = phase_tasks.len();
                let phase_status = if completed == total {
                    style("COMPLETE").green()
                } else if phase_tasks.iter().any(|t| t.status.is_active()) {
                    style("IN PROGRESS").yellow()
                } else {
                    style("PENDING").dim()
                };

                println!(
                    "  {} [{}/{}] {}",
                    phase.name, completed, total, phase_status
                );
            }
            println!();
        }

        let active: Vec<_> = mission
            .tasks
            .iter()
            .filter(|t| t.status.is_active())
            .collect();
        if !active.is_empty() {
            println!("{}", style("Active Tasks:").bold());
            for task in active {
                println!(
                    "  {} {} ({})",
                    style("→").cyan(),
                    task.description,
                    task.agent_type
                );
            }
            println!();
        }

        if !mission.learnings.is_empty() {
            println!(
                "{}",
                style(format!("Learnings: {} recorded", mission.learnings.len())).dim()
            );
        }

        if let Some(started) = mission.started_at {
            println!(
                "{}",
                style(format!("Started: {}", started.format("%Y-%m-%d %H:%M:%S"))).dim()
            );
        }

        if let Some(completed) = mission.completed_at {
            println!(
                "{}",
                style(format!(
                    "Completed: {}",
                    completed.format("%Y-%m-%d %H:%M:%S")
                ))
                .dim()
            );
        }
    }

    pub fn print_missions_table(&self, missions: &[Mission]) {
        if missions.is_empty() {
            println!("{}", style("No missions found.").dim());
            return;
        }

        let active = missions.iter().filter(|m| m.status.is_active()).count();
        let pending = missions
            .iter()
            .filter(|m| m.status == MissionState::Pending)
            .count();
        let completed = missions
            .iter()
            .filter(|m| m.status == MissionState::Completed)
            .count();

        println!(
            "Active: {}  Pending: {}  Completed: {}",
            style(active).yellow(),
            style(pending).dim(),
            style(completed).green()
        );
        println!();

        println!(
            "{:<8} {:<30} {:<12} {:<10}",
            style("ID").bold(),
            style("Description").bold(),
            style("Status").bold(),
            style("Progress").bold()
        );
        println!("{}", style("─".repeat(65)).dim());

        for mission in missions {
            let status_style = self.status_style(mission.status);
            let progress = mission.progress();

            let desc = truncate_chars(&mission.description, 25);

            let progress_str = if progress.total > 0 {
                format!(
                    "{}% {}/{}",
                    progress.percentage, progress.completed, progress.total
                )
            } else {
                "-".to_string()
            };

            println!(
                "{:<8} {:<30} {:<12} {:<10}",
                mission.id,
                desc,
                status_style.apply_to(mission.status.to_string()),
                progress_str
            );
        }
    }

    pub fn print_success(&self, message: &str) {
        println!("{} {}", style("✓").green().bold(), message);
    }

    pub fn print_error(&self, message: &str) {
        eprintln!("{} {}", style("✗").red().bold(), message);
    }

    pub fn print_warning(&self, message: &str) {
        println!("{} {}", style("!").yellow().bold(), message);
    }

    pub fn print_info(&self, message: &str) {
        println!("{} {}", style("→").cyan(), message);
    }

    pub fn create_spinner(&self, message: &str) -> ProgressBar {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .expect("static template")
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(message.to_string());
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        pb
    }

    fn status_style(&self, status: MissionState) -> Style {
        match status {
            MissionState::Pending => Style::new().dim(),
            MissionState::Planning => Style::new().blue(),
            MissionState::Running => Style::new().yellow().bold(),
            MissionState::Paused => Style::new().magenta(),
            MissionState::Verifying => Style::new().cyan(),
            MissionState::Escalated => Style::new().yellow().bold().underlined(),
            MissionState::Completed => Style::new().green(),
            MissionState::Failed => Style::new().red().bold(),
            MissionState::Cancelled => Style::new().dim().strikethrough(),
        }
    }

    fn progress_bar(&self, percentage: u8, width: usize) -> String {
        let filled = (width as f64 * percentage as f64 / 100.0) as usize;
        let empty = width - filled;

        format!(
            "{}{}",
            style("█".repeat(filled)).green(),
            style("░".repeat(empty)).dim()
        )
    }

    fn progress_bar_inline(&self, percentage: u8) -> String {
        self.progress_bar(percentage, 20)
    }
}

impl Default for Display {
    fn default() -> Self {
        Self::new()
    }
}
