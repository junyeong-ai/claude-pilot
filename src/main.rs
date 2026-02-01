use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use claude_pilot::cli::{Cli, Commands, ConfigAction, Display, OutputFormat, StatusFilterArg};
use claude_pilot::config::{PilotConfig, ProjectPaths};
use claude_pilot::error::{PilotError, Result};
use claude_pilot::learning::LearningExtractor;
use claude_pilot::mission::{IsolationMode, MissionFlags, OnComplete, Priority};
use claude_pilot::orchestrator::Orchestrator;
use claude_pilot::output::{MissionOutput, OutputWriter};

/// Context for command output handling.
struct OutputContext<'a> {
    display: &'a Display,
    writer: &'a OutputWriter,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            Display::new().print_error(&e.to_string());
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbose: bool) {
    let filter = if verbose {
        EnvFilter::new("claude_pilot=debug")
    } else {
        EnvFilter::new("claude_pilot=info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).without_time())
        .with(filter)
        .init();
}

async fn run(cli: Cli) -> Result<()> {
    let display = Display::new();
    let writer = OutputWriter::new(cli.output);
    let out = OutputContext {
        display: &display,
        writer: &writer,
    };
    let manifest = cli.manifest;

    match cli.command {
        Commands::Init => cmd_init(&out).await,
        Commands::Mission {
            description,
            isolated,
            branch,
            direct,
            priority,
            on_complete,
        } => {
            let flags = MissionFlags {
                isolated,
                direct: direct || branch,
            };
            cmd_mission(
                &out,
                &description,
                flags,
                priority.map(Into::into),
                on_complete.map(Into::into),
                manifest.clone(),
            )
            .await
        }
        Commands::Status { mission_id } => cmd_status(&out, mission_id).await,
        Commands::List { status } => cmd_list(&out, status).await,
        Commands::Logs { mission_id, lines } => cmd_logs(&out, &mission_id, lines).await,
        Commands::Pause { mission_id } => cmd_pause(&out, &mission_id).await,
        Commands::Resume { mission_id } => cmd_resume(&out, &mission_id).await,
        Commands::Cancel { mission_id } => cmd_cancel(&out, &mission_id).await,
        Commands::Retry {
            mission_id,
            continue_from,
            force,
        } => cmd_retry(&out, &mission_id, continue_from, force).await,
        Commands::Merge {
            mission_id,
            delete_branch,
        } => cmd_merge(&out, &mission_id, delete_branch).await,
        Commands::Cleanup { mission_id, all } => cmd_cleanup(&out, mission_id, all).await,
        Commands::Extract {
            mission_id,
            all,
            apply,
        } => cmd_extract(&out, mission_id, all, apply).await,
        Commands::Config { action } => cmd_config(&out, action).await,
    }
}

fn find_project_root() -> Result<PathBuf> {
    let current = std::env::current_dir()?;

    let mut path = current.as_path();
    loop {
        if path.join(".git").exists() {
            return Ok(path.to_path_buf());
        }
        path = path.parent().ok_or(PilotError::NotInGitRepo)?;
    }
}

fn ensure_initialized(paths: &ProjectPaths) -> Result<()> {
    if !paths.pilot_dir.exists() {
        return Err(PilotError::NotInitialized);
    }
    Ok(())
}

async fn cmd_init(out: &OutputContext<'_>) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::default();
    let paths = ProjectPaths::new(root, &config);

    if paths.pilot_dir.exists() {
        if out.writer.format() == OutputFormat::Text {
            out.display
                .print_warning("claude-pilot is already initialized in this project.");
        }
        return Ok(());
    }

    paths.ensure_dirs().await?;
    config.save(&paths.pilot_dir).await?;

    if out.writer.format() == OutputFormat::Text {
        out.display.print_success("Initialized claude-pilot.");
        out.display.print_info(&format!(
            "Configuration: {}",
            paths.pilot_dir.join("config.toml").display()
        ));
        out.display
            .print_info(&format!("Missions: {}", paths.missions_dir.display()));
    } else {
        out.writer.emit_message("Initialized claude-pilot");
    }

    Ok(())
}

async fn cmd_mission(
    out: &OutputContext<'_>,
    description: &str,
    flags: MissionFlags,
    priority: Option<Priority>,
    on_complete: Option<OnComplete>,
    manifest: Option<PathBuf>,
) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::with_manifest(config, paths, manifest).await?;
    let mission = orchestrator
        .create_mission(description, flags, priority, on_complete)
        .await?;

    if out.writer.format() == OutputFormat::Text {
        out.display
            .print_success(&format!("Created mission: {}", mission.id));
        out.display
            .print_info(&format!("Isolation: {}", mission.isolation));
    }

    let spinner = if out.writer.format() == OutputFormat::Text {
        Some(out.display.create_spinner("Executing mission..."))
    } else {
        None
    };

    let result = orchestrator.execute(&mission.id).await;

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    let updated = orchestrator.get_status(&mission.id).await?;
    let success = result.is_ok();

    match out.writer.format() {
        OutputFormat::Text => {
            if success {
                out.display
                    .print_success(&format!("Mission {} completed!", mission.id));
            } else {
                out.display
                    .print_error(&format!("Mission failed: {}", result.unwrap_err()));
            }
            out.display.print_mission_summary(&updated);
        }
        OutputFormat::Json | OutputFormat::Stream => {
            let output = MissionOutput::from_mission(&updated, success);
            out.writer.emit_result(&output);
        }
    }

    Ok(())
}

async fn cmd_status(out: &OutputContext<'_>, mission_id: Option<String>) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;

    match mission_id {
        Some(id) => {
            let mission = orchestrator.get_status(&id).await?;

            // Check for orphan state and show warning
            if mission.status == claude_pilot::MissionState::Running
                && let Ok(is_orphan) = orchestrator.is_orphan_state(&id).await
                && is_orphan
                && out.writer.format() == OutputFormat::Text
            {
                out.display.print_warning(&format!(
                    "Mission {} appears to be in orphan state (process died). \
                            Use 'claude-pilot retry {} --force' to recover.",
                    id, id
                ));
            }

            match out.writer.format() {
                OutputFormat::Text => out.display.print_mission_detail(&mission),
                OutputFormat::Json | OutputFormat::Stream => out.writer.emit_status(&mission),
            }
        }
        None => {
            let missions = orchestrator.list_missions().await?;

            // Check for orphan states and show warnings
            if out.writer.format() == OutputFormat::Text {
                for m in &missions {
                    if m.status == claude_pilot::MissionState::Running
                        && let Ok(is_orphan) = orchestrator.is_orphan_state(&m.id).await
                        && is_orphan
                    {
                        out.display.print_warning(&format!(
                            "Mission {} appears to be in orphan state. \
                                    Use 'claude-pilot retry {} --force' to recover.",
                            m.id, m.id
                        ));
                    }
                }
            }

            match out.writer.format() {
                OutputFormat::Text => {
                    out.display.print_header("Claude-Pilot Status");
                    out.display.print_missions_table(&missions);
                }
                OutputFormat::Json | OutputFormat::Stream => out.writer.emit_list(&missions),
            }
        }
    }

    Ok(())
}

async fn cmd_list(out: &OutputContext<'_>, status: Option<StatusFilterArg>) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;
    let missions = orchestrator.list_missions().await?;

    let filtered: Vec<_> = match status {
        Some(s) => {
            let status: claude_pilot::MissionState = s.into();
            missions
                .into_iter()
                .filter(|m| m.status == status)
                .collect()
        }
        None => missions,
    };

    match out.writer.format() {
        OutputFormat::Text => {
            out.display.print_header("Missions");
            out.display.print_missions_table(&filtered);
        }
        OutputFormat::Json | OutputFormat::Stream => out.writer.emit_list(&filtered),
    }

    Ok(())
}

async fn cmd_logs(out: &OutputContext<'_>, mission_id: &str, lines: usize) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let log_path = paths.mission_log(mission_id);

    if !log_path.exists() {
        if out.writer.format() == OutputFormat::Text {
            out.display
                .print_warning(&format!("No logs found for mission {}", mission_id));
        }
        return Ok(());
    }

    let content = tokio::fs::read_to_string(&log_path).await?;
    let log_lines: Vec<_> = content.lines().collect();
    let start = log_lines.len().saturating_sub(lines);

    for line in &log_lines[start..] {
        println!("{}", line);
    }

    Ok(())
}

async fn cmd_pause(out: &OutputContext<'_>, mission_id: &str) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;
    orchestrator.pause(mission_id).await?;

    match out.writer.format() {
        OutputFormat::Text => out
            .display
            .print_success(&format!("Paused mission: {}", mission_id)),
        OutputFormat::Json | OutputFormat::Stream => out
            .writer
            .emit_message(&format!("Paused mission: {}", mission_id)),
    }
    Ok(())
}

async fn cmd_resume(out: &OutputContext<'_>, mission_id: &str) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;

    let spinner = if out.writer.format() == OutputFormat::Text {
        Some(out.display.create_spinner("Resuming mission..."))
    } else {
        None
    };

    let result = orchestrator.resume(mission_id).await;

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    match out.writer.format() {
        OutputFormat::Text => match result {
            Ok(()) => out
                .display
                .print_success(&format!("Mission {} resumed and completed!", mission_id)),
            Err(e) => out.display.print_error(&format!("Failed to resume: {}", e)),
        },
        OutputFormat::Json | OutputFormat::Stream => {
            let updated = orchestrator.get_status(mission_id).await?;
            let output = MissionOutput::from_mission(&updated, result.is_ok());
            out.writer.emit_result(&output);
        }
    }

    Ok(())
}

async fn cmd_cancel(out: &OutputContext<'_>, mission_id: &str) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;
    orchestrator.cancel(mission_id).await?;

    match out.writer.format() {
        OutputFormat::Text => out
            .display
            .print_success(&format!("Cancelled mission: {}", mission_id)),
        OutputFormat::Json | OutputFormat::Stream => out
            .writer
            .emit_message(&format!("Cancelled mission: {}", mission_id)),
    }
    Ok(())
}

async fn cmd_retry(
    out: &OutputContext<'_>,
    mission_id: &str,
    continue_from: bool,
    force: bool,
) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths).await?;

    // Check if this is an orphan state recovery
    if force
        && let Ok(was_orphan) = orchestrator.recover_orphan_state(mission_id).await
        && was_orphan
        && out.writer.format() == OutputFormat::Text
    {
        out.display.print_info(&format!(
            "Recovered mission {} from orphan state (process had died)",
            mission_id
        ));
    }

    let mode = if continue_from {
        "continuing"
    } else {
        "restarting"
    };
    let spinner = if out.writer.format() == OutputFormat::Text {
        Some(
            out.display
                .create_spinner(&format!("Retrying mission ({})...", mode)),
        )
    } else {
        None
    };

    let result = orchestrator.retry(mission_id, !continue_from, force).await;

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    let updated = orchestrator.get_status(mission_id).await?;
    let success = result.is_ok();

    match out.writer.format() {
        OutputFormat::Text => {
            if success {
                out.display
                    .print_success(&format!("Mission {} completed!", mission_id));
                out.display.print_mission_summary(&updated);
            } else {
                out.display
                    .print_error(&format!("Retry failed: {}", result.unwrap_err()));
            }
        }
        OutputFormat::Json | OutputFormat::Stream => {
            let output = MissionOutput::from_mission(&updated, success);
            out.writer.emit_result(&output);
        }
    }

    Ok(())
}

async fn cmd_merge(out: &OutputContext<'_>, mission_id: &str, delete_branch: bool) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config, paths.clone()).await?;
    let mission = orchestrator.get_status(mission_id).await?;

    if mission.status != claude_pilot::MissionState::Completed {
        return Err(PilotError::InvalidMissionState {
            expected: "completed".into(),
            actual: mission.status.to_string(),
        });
    }

    let isolation = claude_pilot::isolation::IsolationManager::new(&paths);
    isolation.merge_to_base(&mission).await?;

    match out.writer.format() {
        OutputFormat::Text => {
            out.display.print_success(&format!(
                "Merged mission {} to {}",
                mission_id, mission.base_branch
            ));
        }
        OutputFormat::Json | OutputFormat::Stream => {
            out.writer.emit_message(&format!(
                "Merged mission {} to {}",
                mission_id, mission.base_branch
            ));
        }
    }

    if delete_branch {
        isolation.cleanup(&mission, true).await?;
        match out.writer.format() {
            OutputFormat::Text => {
                if let Some(branch) = &mission.branch {
                    out.display
                        .print_info(&format!("Deleted branch: {}", branch));
                }
            }
            OutputFormat::Json | OutputFormat::Stream => {
                if let Some(branch) = &mission.branch {
                    out.writer
                        .emit_message(&format!("Deleted branch: {}", branch));
                }
            }
        }
    }

    Ok(())
}

async fn cmd_cleanup(out: &OutputContext<'_>, mission_id: Option<String>, all: bool) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root, &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config.clone(), paths.clone()).await?;
    let isolation = claude_pilot::isolation::IsolationManager::new(&paths);

    if all {
        let missions = orchestrator.list_missions().await?;
        let active_ids: Vec<String> = missions
            .iter()
            .filter(|m| !m.status.is_terminal())
            .map(|m| m.id.clone())
            .collect();

        // Cleanup worktrees and branches for terminal missions
        for mission in missions
            .iter()
            .filter(|m| m.status.is_terminal() && m.isolation == IsolationMode::Worktree)
        {
            if let Err(e) = isolation.cleanup(mission, true).await {
                if out.writer.format() == OutputFormat::Text {
                    out.display
                        .print_warning(&format!("Failed to cleanup {}: {}", mission.id, e));
                }
            } else {
                match out.writer.format() {
                    OutputFormat::Text => out.display.print_info(&format!(
                        "Cleaned up worktree and branch for {}",
                        mission.id
                    )),
                    OutputFormat::Json | OutputFormat::Stream => out.writer.emit_message(&format!(
                        "Cleaned up worktree and branch for {}",
                        mission.id
                    )),
                }
            }
        }

        // Cleanup orphaned worktrees
        if let Err(e) = isolation.cleanup_orphaned(&active_ids).await
            && out.writer.format() == OutputFormat::Text
        {
            out.display
                .print_warning(&format!("Failed to cleanup orphaned worktrees: {}", e));
        }

        // Cleanup orphaned branches
        let deleted = isolation
            .cleanup_orphaned_branches(&active_ids, &config.git.branch_prefix)
            .await
            .unwrap_or_default();
        for branch in deleted {
            match out.writer.format() {
                OutputFormat::Text => out
                    .display
                    .print_info(&format!("Deleted orphaned branch: {}", branch)),
                OutputFormat::Json | OutputFormat::Stream => out
                    .writer
                    .emit_message(&format!("Deleted orphaned branch: {}", branch)),
            }
        }
    } else if let Some(id) = mission_id {
        let mission = orchestrator.get_status(&id).await?;
        isolation.cleanup(&mission, true).await?;
        match out.writer.format() {
            OutputFormat::Text => out
                .display
                .print_success(&format!("Cleaned up worktree and branch for {}", id)),
            OutputFormat::Json | OutputFormat::Stream => out
                .writer
                .emit_message(&format!("Cleaned up worktree and branch for {}", id)),
        }
    } else if out.writer.format() == OutputFormat::Text {
        out.display
            .print_warning("Specify a mission ID or use --all");
    }

    Ok(())
}

async fn cmd_extract(
    out: &OutputContext<'_>,
    mission_id: Option<String>,
    all: bool,
    apply: bool,
) -> Result<()> {
    let root = find_project_root()?;
    let config = PilotConfig::load(&root.join(".claude/pilot")).await?;
    let paths = ProjectPaths::new(root.clone(), &config);
    ensure_initialized(&paths)?;

    let orchestrator = Orchestrator::new(config.clone(), paths.clone()).await?;
    let extractor = LearningExtractor::new(paths, &config.agent);

    let missions: Vec<_> = if all {
        orchestrator
            .list_missions()
            .await?
            .into_iter()
            .filter(|m| m.status == claude_pilot::MissionState::Completed)
            .collect()
    } else if let Some(id) = mission_id {
        vec![orchestrator.get_status(&id).await?]
    } else {
        if out.writer.format() == OutputFormat::Text {
            out.display
                .print_warning("Specify a mission ID or use --all");
        }
        return Ok(());
    };

    for mission in &missions {
        out.display
            .print_info(&format!("Extracting from mission {}...", mission.id));

        let result = extractor.extract(mission, &root).await?;

        if result.is_empty() {
            out.display.print_info("No extractions found.");
            continue;
        }

        println!();
        println!("Found {} potential extractions:", result.total_count());

        for skill in &result.skills {
            println!(
                "  [Skill] {} - {} (confidence: {:.0}%)",
                skill.name,
                skill.description,
                skill.confidence * 100.0
            );
        }

        for rule in &result.rules {
            println!(
                "  [Rule] {} - {} (confidence: {:.0}%)",
                rule.name,
                rule.description,
                rule.confidence * 100.0
            );
        }

        for agent in &result.agents {
            println!(
                "  [Agent] {} - {} (confidence: {:.0}%)",
                agent.name,
                agent.description,
                agent.confidence * 100.0
            );
        }

        if apply {
            let all_names: Vec<_> = result
                .skills
                .iter()
                .chain(result.rules.iter())
                .chain(result.agents.iter())
                .map(|c| c.name.clone())
                .collect();

            let written = extractor.apply(&result, &all_names).await?;
            for path in written {
                out.display.print_success(&format!("Wrote: {}", path));
            }
        }
    }

    Ok(())
}

async fn cmd_config(out: &OutputContext<'_>, action: ConfigAction) -> Result<()> {
    let root = find_project_root()?;
    let pilot_dir = root.join(".claude/pilot");

    match action {
        ConfigAction::Show => {
            let config = PilotConfig::load(&pilot_dir).await?;
            match out.writer.format() {
                OutputFormat::Text => {
                    let yaml = serde_yaml_bw::to_string(&config)?;
                    println!("{}", yaml);
                }
                OutputFormat::Json | OutputFormat::Stream => {
                    let json = serde_json::to_string_pretty(&config)?;
                    println!("{}", json);
                }
            }
        }
        ConfigAction::Edit => {
            let config_path = pilot_dir.join("config.toml");
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());

            tokio::process::Command::new(&editor)
                .arg(&config_path)
                .status()
                .await?;

            if out.writer.format() == OutputFormat::Text {
                out.display.print_success("Configuration updated.");
            }
        }
        ConfigAction::Reset => {
            let config = PilotConfig::default();
            config.save(&pilot_dir).await?;
            if out.writer.format() == OutputFormat::Text {
                out.display
                    .print_success("Configuration reset to defaults.");
            }
        }
    }

    Ok(())
}
