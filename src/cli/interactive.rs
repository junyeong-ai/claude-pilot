//! Interactive CLI handler for human-in-the-loop escalation.

use std::io::{self, Write};

use console::{Term, style};

use crate::domain::{EscalationContext, HumanAction, HumanResponse};
use crate::error::{PilotError, Result};

/// Handler for interactive CLI escalation.
pub struct InteractiveHandler {
    term: Term,
    /// Maximum issues to display before truncating.
    max_display_issues: usize,
}

impl InteractiveHandler {
    /// Default max issues to display in escalation view.
    const DEFAULT_MAX_DISPLAY_ISSUES: usize = 5;

    pub fn new() -> Self {
        Self {
            term: Term::stdout(),
            max_display_issues: Self::DEFAULT_MAX_DISPLAY_ISSUES,
        }
    }

    /// Create handler with custom display limit.
    pub fn with_max_issues(mut self, max: usize) -> Self {
        self.max_display_issues = max;
        self
    }

    /// Handle an escalation request interactively.
    pub fn handle_escalation(&self, ctx: &EscalationContext) -> Result<HumanResponse> {
        self.clear_and_show_header()?;
        self.show_summary(ctx)?;
        self.show_attempted_strategies(ctx)?;
        self.show_issues(ctx)?;

        let response = self.prompt_action(ctx)?;
        Ok(response)
    }

    fn clear_and_show_header(&self) -> Result<()> {
        self.term.clear_screen()?;

        println!();
        println!(
            "{}",
            style("╔════════════════════════════════════════════════════════════╗")
                .yellow()
                .bold()
        );
        println!(
            "{}",
            style("║              🔔 Human Input Required                       ║")
                .yellow()
                .bold()
        );
        println!(
            "{}",
            style("╚════════════════════════════════════════════════════════════╝")
                .yellow()
                .bold()
        );
        println!();

        Ok(())
    }

    fn show_summary(&self, ctx: &EscalationContext) -> Result<()> {
        println!("{}", style("Summary:").bold().white());
        println!("  {}", ctx.summary);
        println!();
        Ok(())
    }

    fn show_attempted_strategies(&self, ctx: &EscalationContext) -> Result<()> {
        if ctx.attempted_strategies.is_empty() {
            return Ok(());
        }

        println!("{}", style("Attempted Strategies:").bold().white());
        for (i, strategy) in ctx.attempted_strategies.iter().enumerate() {
            let outcome_style = match strategy.outcome {
                crate::domain::StrategyOutcome::Success => style("✓").green(),
                crate::domain::StrategyOutcome::PartialSuccess => style("~").yellow(),
                crate::domain::StrategyOutcome::Failed => style("✗").red(),
                crate::domain::StrategyOutcome::Stagnated => style("○").dim(),
            };
            println!(
                "  {}. {} {} (level {}, {} rounds)",
                i + 1,
                outcome_style,
                strategy.name,
                strategy.level,
                strategy.rounds
            );
        }
        println!();
        Ok(())
    }

    fn show_issues(&self, ctx: &EscalationContext) -> Result<()> {
        if ctx.persistent_issues.is_empty() {
            return Ok(());
        }

        let issue_count = ctx.persistent_issues.len();
        let shown = ctx.persistent_issues.iter().take(self.max_display_issues);

        println!(
            "{}",
            style(format!("Persistent Issues ({}):", issue_count))
                .bold()
                .white()
        );
        for issue in shown {
            let severity_style = match issue.severity {
                crate::verification::IssueSeverity::Critical => style("CRIT").red().bold(),
                crate::verification::IssueSeverity::Error => style("ERR ").red(),
                crate::verification::IssueSeverity::Warning => style("WARN").yellow(),
                crate::verification::IssueSeverity::Info => style("INFO").dim(),
            };

            let location = match (&issue.file, issue.line) {
                (Some(f), Some(l)) => format!("{}:{}", f, l),
                (Some(f), None) => f.clone(),
                _ => String::new(),
            };

            println!(
                "  {} {} {}",
                severity_style,
                style(&location).dim(),
                issue.message
            );
        }

        if issue_count > self.max_display_issues {
            println!(
                "  {} more...",
                style(format!("... and {}", issue_count - self.max_display_issues)).dim()
            );
        }
        println!();
        Ok(())
    }

    fn prompt_action(&self, ctx: &EscalationContext) -> Result<HumanResponse> {
        if ctx.suggested_actions.is_empty() {
            return Ok(HumanResponse::Cancelled);
        }

        println!("{}", style("Available Actions:").bold().white());
        for (i, action) in ctx.suggested_actions.iter().enumerate() {
            println!(
                "  [{}] {}",
                style(i + 1).cyan().bold(),
                action.display_text()
            );
        }
        println!();

        let selection = self.prompt_number(1, ctx.suggested_actions.len())?;
        let action = &ctx.suggested_actions[selection - 1];

        self.execute_action(action)
    }

    fn execute_action(&self, action: &HumanAction) -> Result<HumanResponse> {
        match action {
            HumanAction::ProvideInfo { question } => {
                println!();
                println!("{}", style(question).bold());
                let info = self.prompt_text("Your answer: ")?;
                Ok(HumanResponse::ProvidedInfo { info })
            }

            HumanAction::ManualFix { file, hint } => {
                println!();
                println!(
                    "{}",
                    style(format!("Please fix: {}", file.display())).bold()
                );
                println!("{}", style(format!("Hint: {}", hint)).dim());
                println!();
                self.wait_for_enter("Press Enter when you have fixed the issue...")?;
                let description = self.prompt_text("Describe what you fixed (optional): ")?;
                Ok(HumanResponse::ManuallyFixed {
                    description: if description.is_empty() {
                        "Manual fix applied".to_string()
                    } else {
                        description
                    },
                })
            }

            HumanAction::ReduceScope { task_id, reason } => {
                println!();
                println!("{}", style(format!("Skipping task: {}", task_id)).yellow());
                println!("{}", style(format!("Reason: {}", reason)).dim());
                self.wait_for_enter("Press Enter to confirm...")?;
                Ok(HumanResponse::AcceptedReducedScope {
                    skipped_task: task_id.clone(),
                })
            }

            HumanAction::AcceptPartial { remaining_issues } => {
                println!();
                println!(
                    "{}",
                    style(format!(
                        "Accepting partial completion with {} remaining issues",
                        remaining_issues
                    ))
                    .yellow()
                );
                self.wait_for_enter("Press Enter to confirm...")?;
                Ok(HumanResponse::AcceptedPartial)
            }

            HumanAction::Retry => {
                println!();
                println!("{}", style("Retrying from last checkpoint...").cyan());
                self.wait_for_enter("Press Enter to confirm retry...")?;
                Ok(HumanResponse::RetryRequested)
            }

            HumanAction::Cancel => {
                println!();
                println!("{}", style("Mission cancelled by user.").red());
                Ok(HumanResponse::Cancelled)
            }
        }
    }

    fn prompt_number(&self, min: usize, max: usize) -> Result<usize> {
        loop {
            print!("{}", style(format!("Select ({}-{}): ", min, max)).cyan());
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            match input.trim().parse::<usize>() {
                Ok(n) if n >= min && n <= max => return Ok(n),
                _ => {
                    println!(
                        "{}",
                        style(format!("Please enter a number between {} and {}", min, max)).red()
                    );
                }
            }
        }
    }

    fn prompt_text(&self, prompt: &str) -> Result<String> {
        print!("{}", style(prompt).cyan());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        Ok(input.trim().to_string())
    }

    fn wait_for_enter(&self, message: &str) -> Result<()> {
        print!("{}", style(message).dim());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        Ok(())
    }
}

impl Default for InteractiveHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Non-interactive handler for CI/CD environments.
pub struct NonInteractiveHandler {
    policy: NonInteractivePolicy,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum NonInteractivePolicy {
    /// Fail immediately on escalation.
    #[default]
    Fail,
    /// Accept reduced scope if available.
    ReduceScope,
    /// Accept partial convergence if available.
    AcceptPartial,
}

impl NonInteractiveHandler {
    pub fn new(policy: NonInteractivePolicy) -> Self {
        Self { policy }
    }

    pub fn handle_escalation(&self, ctx: &EscalationContext) -> Result<HumanResponse> {
        match self.policy {
            NonInteractivePolicy::Fail => Err(PilotError::EscalationRequired {
                summary: ctx.summary.clone(),
            }),

            NonInteractivePolicy::ReduceScope => ctx
                .suggested_actions
                .iter()
                .find_map(|a| match a {
                    HumanAction::ReduceScope { task_id, .. } => {
                        Some(HumanResponse::AcceptedReducedScope {
                            skipped_task: task_id.clone(),
                        })
                    }
                    _ => None,
                })
                .ok_or_else(|| PilotError::EscalationRequired {
                    summary: "No scope reduction available".into(),
                }),

            NonInteractivePolicy::AcceptPartial => ctx
                .suggested_actions
                .iter()
                .find_map(|a| match a {
                    HumanAction::AcceptPartial { .. } => Some(HumanResponse::AcceptedPartial),
                    _ => None,
                })
                .ok_or_else(|| PilotError::EscalationRequired {
                    summary: "Partial acceptance not available".into(),
                }),
        }
    }
}
