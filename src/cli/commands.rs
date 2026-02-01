use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "claude-pilot")]
#[command(author, version, about = "Mission orchestrator for Claude Code", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[arg(short, long, global = true, value_enum, default_value = "text")]
    pub output: OutputFormat,

    /// Path to manifest.json (default: .claudegen/manifest.json)
    #[arg(long, global = true, env = "CLAUDE_PILOT_MANIFEST")]
    pub manifest: Option<PathBuf>,
}

/// Output format for CLI results.
/// - Text: Human-readable text output (default)
/// - Json: Single JSON object at completion
/// - Stream: NDJSON streaming (one JSON object per line, real-time events)
#[derive(Debug, Clone, Copy, Default, ValueEnum, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Stream,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize claude-pilot in the current project
    Init,

    /// Start a new mission
    Mission {
        /// Mission description
        description: String,

        /// Force worktree isolation
        #[arg(long)]
        isolated: bool,

        /// Force branch-only isolation
        #[arg(long)]
        branch: bool,

        /// Work directly on current branch (no isolation)
        #[arg(long)]
        direct: bool,

        /// Mission priority (P1, P2, P3)
        #[arg(long, value_enum)]
        priority: Option<PriorityArg>,

        /// Action on completion (pr, manual, direct)
        #[arg(long, value_enum)]
        on_complete: Option<OnCompleteArg>,
    },

    /// Show mission status
    Status {
        /// Mission ID (optional, shows all if not specified)
        mission_id: Option<String>,
    },

    /// List all missions
    List {
        /// Filter by status
        #[arg(long, value_enum)]
        status: Option<StatusFilterArg>,
    },

    /// Show mission logs
    Logs {
        /// Mission ID
        mission_id: String,

        /// Number of lines to show
        #[arg(short, long, default_value = "50")]
        lines: usize,
    },

    /// Pause a running mission
    Pause {
        /// Mission ID
        mission_id: String,
    },

    /// Resume a paused mission (continue from where it stopped)
    Resume {
        /// Mission ID
        mission_id: String,
    },

    /// Cancel a mission
    Cancel {
        /// Mission ID
        mission_id: String,
    },

    /// Retry a failed or cancelled mission
    Retry {
        /// Mission ID
        mission_id: String,

        /// Only retry failed tasks (keep completed tasks)
        #[arg(long, short = 'c')]
        continue_from: bool,

        /// Force retry even if mission is in running state (useful for recovering from orphan states)
        #[arg(long, short = 'f')]
        force: bool,
    },

    /// Merge a completed mission
    Merge {
        /// Mission ID
        mission_id: String,

        /// Delete branch after merge
        #[arg(long)]
        delete_branch: bool,
    },

    /// Clean up worktrees
    Cleanup {
        /// Mission ID (optional, cleans all if not specified)
        mission_id: Option<String>,

        /// Clean all completed mission worktrees
        #[arg(long)]
        all: bool,
    },

    /// Extract learnings from completed missions
    Extract {
        /// Mission ID (optional, extracts from all recent if not specified)
        mission_id: Option<String>,

        /// Extract from all completed missions
        #[arg(long)]
        all: bool,

        /// Apply extractions immediately
        #[arg(long)]
        apply: bool,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Edit configuration
    Edit,
    /// Reset to defaults
    Reset,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum PriorityArg {
    P1,
    P2,
    P3,
}

impl From<PriorityArg> for crate::mission::Priority {
    fn from(arg: PriorityArg) -> Self {
        match arg {
            PriorityArg::P1 => Self::P1,
            PriorityArg::P2 => Self::P2,
            PriorityArg::P3 => Self::P3,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum OnCompleteArg {
    Pr,
    Manual,
    Direct,
}

impl From<OnCompleteArg> for crate::mission::OnComplete {
    fn from(arg: OnCompleteArg) -> Self {
        match arg {
            OnCompleteArg::Pr => Self::PullRequest {
                auto_merge: false,
                reviewers: vec![],
            },
            OnCompleteArg::Manual => Self::Manual,
            OnCompleteArg::Direct => Self::Direct,
        }
    }
}

#[derive(Clone, ValueEnum)]
pub enum StatusFilterArg {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl From<StatusFilterArg> for crate::state::MissionState {
    fn from(arg: StatusFilterArg) -> Self {
        match arg {
            StatusFilterArg::Pending => Self::Pending,
            StatusFilterArg::Running => Self::Running,
            StatusFilterArg::Paused => Self::Paused,
            StatusFilterArg::Completed => Self::Completed,
            StatusFilterArg::Failed => Self::Failed,
            StatusFilterArg::Cancelled => Self::Cancelled,
        }
    }
}
