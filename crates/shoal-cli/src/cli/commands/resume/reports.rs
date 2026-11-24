//! What `shoal resume` tells the user: the report of each outcome, and the
//! notices a resume in this terminal prints before its agent starts.
use crate::execution::recovery::changes::Change;
use anyhow::{Result, ensure};
use std::fmt::Display;

use crate::cli::commands::restore::reopen::ReopenedExt;
use crate::client::output::{
    batch::Failure,
    restore::Reopened,
    resume::{Queued, StoppedCommands},
};
use crate::{
    cli::{context::Context, ui},
    execution::recovery::{SavedCommand, SavedSession},
    model::Space,
    state::QueueState,
};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::space_names::SpaceName;
use shoal_workers::resume::Resuming;

/// What `shoal resume` tells the user as a resume goes.
pub(in crate::cli::commands) struct Reporting<'a>(pub &'a Context);

impl Resuming for Reporting<'_> {
    fn pick_agent(&self, agents: &[SavedSession], labels: &[String]) -> Result<SavedSession> {
        ensure!(
            self.0.interactive(),
            "multiple stopped agents; select one with --execution <id>"
        );
        let choices: Vec<_> = (agents.iter().cloned())
            .zip(labels.iter().map(String::as_str))
            .collect();
        ui::pick_choice(self.0, "Resume agent> ", &choices)
    }

    fn stopped_commands(&self, space: &Space, commands: &[Vec<String>]) {
        stopped_commands(self.0, (space.number, &space.name), commands);
    }

    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure {
        ui::failure(self.0, "resume", space, error)
    }
}

/// Report what the resumes of the selected spaces did, `resumed`, or that
/// nothing resumed, returning the exit code: 1 when any failed.
pub(super) fn resumed(ctx: &Context, resumed: &Queued) -> Result<i32> {
    if !resumed.reports_anything() {
        ctx.emit("No agents to resume", Queued::default())?;
        return Ok(0);
    }
    ctx.show(resumed, |resumed| {
        reopened_lines(ctx, &resumed.reopened);
        for continued in &resumed.continued {
            let agent = (continued.agent.as_ref()).map_or("the agent", |agent| agent.name());
            outln!("Continued the turn of {agent} in {}", continued.space);
        }
        if let Some(summary) = resumed.summary() {
            outln!("{summary}");
        }
    })?;
    Ok(if resumed.failed.is_empty() { 0 } else { 1 })
}

/// Report the `queued` restores and the spaces reopened.
pub(in crate::cli::commands) fn queued(ctx: &Context, queued: &Queued) -> Result<()> {
    ctx.show(queued, |queued| {
        reopened_lines(ctx, &queued.reopened);
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })
}

/// Tell what each of the `reopened` workspaces restored.
fn reopened_lines(ctx: &Context, reopened: &[Reopened]) {
    for reopened in reopened {
        outln!("{}", reopened.message(ctx, "Restored"));
    }
}

/// Operations on the client type [`Queued`] that only the `shoal` command uses.
pub(crate) trait QueuedExt {
    /// What the user reads once restores are queued, `None` without any.
    fn summary(&self) -> Option<String>;

    /// Whether it reports anything a restore did or failed to do.
    fn reports_anything(&self) -> bool;
}

impl QueuedExt for Queued {
    fn reports_anything(&self) -> bool {
        let Self {
            queued,
            commands,
            reopened,
            continued,
            failed,
        } = self;
        !(queued.is_empty()
            && commands.is_empty()
            && reopened.is_empty()
            && continued.is_empty()
            && failed.is_empty())
    }

    fn summary(&self) -> Option<String> {
        let starting = (self.queued.iter()).all(|entry| entry.state == QueueState::Starting);
        (!self.queued.is_empty()).then(|| summary(self.queued.len(), starting))
    }
}

/// What the user reads once `count` restores are queued, or `starting`.
fn summary(count: usize, starting: bool) -> String {
    let agents = if count == 1 { "agent" } else { "agents" };
    let done = if starting { "Starting" } else { "Queued" };
    format!("{done} {count} {agents}; shoal queue shows them")
}

/// List the stopped `commands` of the space numbered and named `space`,
/// each as its arguments, which no agent receives, in text output.
pub(in crate::cli::commands) fn stopped_commands(
    ctx: &Context,
    space: (SpaceNumber, &SpaceName),
    commands: &[Vec<String>],
) {
    if !commands.is_empty() && !ctx.json {
        outln!("{}", command_list(space, commands));
    }
}

/// Report a workspace with only stopped commands, which
/// [`stopped_commands`] listed already in text output.
pub(super) fn commands_only(ctx: &Context, report: &StoppedCommands) -> Result<()> {
    ctx.show(report, |_| {})
}

/// Tell the user `agent` restores after a stop for `reason`.
pub(super) fn restoring(agent: impl Display, reason: &str) {
    eprintln!("shoal: restoring {agent}, stopped: {reason}");
}

/// Tell the user `agent`'s last session continues.
pub(super) fn continuing(agent: impl Display) {
    eprintln!("shoal: continuing the last {agent} session");
}

/// Tell the user how `agent` starts in a space of `kind` that changed as
/// `change` says: from a fork of its session when `forked`, otherwise as a
/// new session. The user converted a converted space, so its agent's start
/// needs no notice.
pub(super) fn starting_changed(
    agent: impl Display,
    kind: crate::model::SpaceKind,
    change: &Change,
    forked: bool,
) {
    match (change, forked) {
        (Change::Converted, _) => {}
        (Change::HandedOver(_), _) => {
            eprintln!("shoal: starting a new {agent} session to take the {kind}'s work over")
        }
        (Change::Copied(_), true) => {
            eprintln!("shoal: starting {agent} from a fork of its session in the copy")
        }
        (Change::Copied(_), false) => {
            eprintln!("shoal: starting a new {agent} session in the copy")
        }
        (Change::Moved, _) => {
            eprintln!("shoal: starting a new {agent} session in the moved {kind}")
        }
    }
}

/// List `workspace`'s stopped `commands`, which the restored agent was not
/// told about.
pub(super) fn commands_not_told(space: &Space, commands: &[SavedCommand]) {
    let commands: Vec<_> = commands
        .iter()
        .map(|command| command.argv.clone())
        .collect();
    eprintln!("{}", command_list((space.number, &space.name), &commands));
}

/// The stopped `commands` of the space numbered and named `space`, each as
/// the command that reruns it there.
fn command_list((number, name): (SpaceNumber, &SpaceName), commands: &[Vec<String>]) -> String {
    let label = shoal_protocol::space_names::label(number, name);
    let mut text = format!("Stopped commands in {label}, not restarted:");
    for argv in commands {
        text.push_str(&format!("\n  {}", rerun(number, argv)));
    }
    text
}

/// The command that runs `argv` in the space numbered `number` again,
/// selecting it by its number, since names select nothing.
fn rerun(number: SpaceNumber, argv: &[String]) -> String {
    let number = number.to_string();
    let mut words = vec!["shoal", "run", "--workspace", &number, "--"];
    words.extend(argv.iter().map(String::as_str));
    crate::shell::quote(&words)
}

#[cfg(test)]
mod tests;
