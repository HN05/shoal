//! What `shoal restore` tells the user: the report of each outcome, and the
//! notices a restore in this terminal prints before its agent starts.
use crate::execution::recovery::changes::Change;
use anyhow::Result;
use std::fmt::Display;

use crate::cli::commands::restore::reopen::ReopenedExt;
use crate::client::output::restore::{
    ForgottenWork, Queued, Reopened, SetUp, StartedRestores, StoppedCommands,
};
use crate::{
    cli::context::Context, execution::recovery::SavedCommand, model::Space, state::QueueState,
};
use shoal_ids::execution_ids::ExecutionId;

/// Report that no space has anything to restore but what `first` reports,
/// the workspaces reopened and the turns continued, as a restore that
/// starts agents at once when `started`, otherwise as one that queues them,
/// returning the exit code.
pub(super) fn nothing(ctx: &Context, first: Queued, started: bool) -> Result<i32> {
    if first.reports_anything() {
        queued(ctx, &first)?;
        return Ok(if first.failed.is_empty() { 0 } else { 1 });
    }
    let message = "No stopped agents, commands or chats";
    match started {
        true => ctx.emit(message, StartedRestores::default())?,
        false => ctx.emit(message, Queued::default())?,
    }
    Ok(0)
}

/// Report what `first` reports, the workspaces reopened for a restore and
/// the turns it continued, before the agent it restores in this terminal,
/// returning the exit code; nothing without any.
pub(super) fn first(ctx: &Context, first: Queued) -> Result<i32> {
    if first.reports_anything() {
        queued(ctx, &first)?;
    }
    Ok(if first.failed.is_empty() { 0 } else { 1 })
}

/// Report the `queued` restores and the workspaces reopened and set up.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<()> {
    ctx.show(queued, |queued| {
        reopened_lines(ctx, &queued.reopened);
        set_up(&queued.set_up);
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

/// List each workspace a restore set up again, with where its waiting agent
/// is.
fn set_up(set_up: &[SetUp]) {
    for done in set_up {
        let name = &done.workspace.name;
        match done.launch.as_ref().map(|launch| launch.state) {
            None => outln!("Set up {name}"),
            Some(QueueState::Queued) => {
                outln!("Set up {name}; its agent starts from the agent queue")
            }
            Some(QueueState::Starting) => outln!("Set up {name}; starting its agent"),
            Some(QueueState::Failed) => outln!("Set up {name}; its agent did not start"),
        }
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
            set_up,
            reopened,
            continued,
            failed,
        } = self;
        !(queued.is_empty()
            && commands.is_empty()
            && set_up.is_empty()
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

/// Report the agents a restore started at once, as `output` lists them:
/// `in_wrappers` of them where they wait to restore, those it started in
/// `place`, and the `listed` ones the user starts in terminals of their own.
pub(super) fn started(
    ctx: &Context,
    output: &StartedRestores,
    in_wrappers: usize,
    place: Option<&str>,
    listed: &[(&Space, &ExecutionId)],
) -> Result<()> {
    ctx.show(output, |output| {
        reopened_lines(ctx, &output.reopened);
        set_up(&output.set_up);
        if !output.queued.is_empty() {
            let starting = (output.queued.iter()).all(|entry| entry.state == QueueState::Starting);
            outln!("{}", summary(output.queued.len(), starting));
        }
        if in_wrappers > 0 {
            let noun = if in_wrappers == 1 { "agent" } else { "agents" };
            outln!("Starting {in_wrappers} {noun} where they wait to restore");
        }
        if let Some(place) = place {
            let started = output.agents.len() - listed.len() - in_wrappers;
            let noun = if started == 1 { "agent" } else { "agents" };
            outln!("Restoring {started} {noun} in {place}");
        }
        if !listed.is_empty() {
            outln!("Restore each agent in its own terminal:");
        }
        for (space, id) in listed {
            let number = space.number.to_string();
            let words = [
                "shoal",
                "restore",
                &number,
                "--execution",
                id.as_str(),
                "--focus",
            ];
            outln!("  {}", crate::shell::quote(&words));
        }
    })
}

/// Report the `forgotten` work of the workspaces `names` lists.
pub(super) fn forgotten(ctx: &Context, forgotten: &ForgottenWork, names: &[&str]) -> Result<()> {
    ctx.show(forgotten, |forgotten| {
        reopened_lines(ctx, &forgotten.reopened);
        if !names.is_empty() {
            outln!(
                "Forgot stopped agents and commands for {}",
                names.join(", ")
            );
        }
    })
}

/// List the stopped `commands` of `space`, which no agent receives, in text
/// output.
pub(super) fn stopped_commands(ctx: &Context, space: &Space, commands: &[SavedCommand]) {
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
    change: Change,
    forked: bool,
) {
    match (change, forked) {
        (Change::Converted, _) => {}
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
    eprintln!("{}", command_list(space, commands));
}

/// The stopped `commands` of `workspace`, each as the command that reruns
/// it there.
fn command_list(space: &Space, commands: &[SavedCommand]) -> String {
    let mut text = format!("Stopped commands in {}, not restarted:", space.label());
    for command in commands {
        text.push_str(&format!("\n  {}", rerun(space, &command.argv)));
    }
    text
}

/// The command that runs `argv` in `space` again, selecting it by its
/// number, since names select nothing.
fn rerun(space: &Space, argv: &[String]) -> String {
    let number = space.number.to_string();
    let mut words = vec!["shoal", "run", "--workspace", &number, "--"];
    words.extend(argv.iter().map(String::as_str));
    crate::shell::quote(&words)
}

#[cfg(test)]
mod tests;
