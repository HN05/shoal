//! What `shoal resume` tells the user: the report of each outcome, and the
//! notices a restore in this terminal prints before its agent starts.
use anyhow::Result;
use std::fmt::Display;

use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::{DiscardedWork, Queued, Resumed, StoppedCommands};
use crate::{
    cli::context::Context, execution::recovery::SavedCommand, model::Workspace, state::QueueState,
};

/// Report that no space has anything to resume, as a resume that starts
/// agents at once when `started`, otherwise as one that queues them.
pub(super) fn nothing(ctx: &Context, started: bool) -> Result<()> {
    let message = "No stopped agents, commands or chats";
    match started {
        true => ctx.emit(message, Resumed::default()),
        false => ctx.emit(message, Queued::default()),
    }
}

/// Report the `queued` restores.
pub(super) fn queued(ctx: &Context, queued: &Queued) -> Result<()> {
    ctx.show(queued, |queued| {
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })
}

/// Operations on the client type [`Queued`] that only the `shoal` command uses.
pub(crate) trait QueuedExt {
    /// What the user reads once restores are queued, `None` without any.
    fn summary(&self) -> Option<String>;
}

impl QueuedExt for Queued {
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

/// Report the agents a resume started at once, as `output` lists them:
/// `in_wrappers` of them where they wait to restore, those it started in
/// `place`, and the `listed` ones the user starts in terminals of their own.
pub(super) fn started(
    ctx: &Context,
    output: &Resumed,
    in_wrappers: usize,
    place: Option<&str>,
    listed: &[(&Workspace, &ExecutionId)],
) -> Result<()> {
    ctx.show(output, |output| {
        if in_wrappers > 0 {
            let noun = if in_wrappers == 1 { "agent" } else { "agents" };
            outln!("Starting {in_wrappers} {noun} where they wait to restore");
        }
        if let Some(place) = place {
            let started = output.agents.len() - listed.len() - in_wrappers;
            let noun = if started == 1 { "agent" } else { "agents" };
            outln!("Resuming {started} {noun} in {place}");
        }
        if !listed.is_empty() {
            outln!("Resume each agent in its own terminal:");
        }
        for (workspace, id) in listed {
            let words = [
                "shoal",
                "resume",
                &workspace.name,
                "--execution",
                id.as_str(),
                "--focus",
            ];
            outln!("  {}", crate::shell::quote(&words));
        }
    })
}

/// Report the `discarded` work of the workspaces `names` lists.
pub(super) fn discarded(ctx: &Context, discarded: &DiscardedWork, names: &[&str]) -> Result<()> {
    ctx.show(discarded, |_| {
        if !names.is_empty() {
            outln!(
                "Discarded stopped agents and commands for {}",
                names.join(", ")
            );
        }
    })
}

/// List `workspace`'s stopped `commands`, which no agent receives, in text
/// output.
pub(super) fn stopped_commands(ctx: &Context, workspace: &Workspace, commands: &[SavedCommand]) {
    if !commands.is_empty() && !ctx.json {
        outln!("{}", command_list(workspace, commands));
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

/// Tell the user `agent` starts a new session in a moved workspace.
pub(super) fn starting_moved(agent: impl Display) {
    eprintln!("shoal: starting a new {agent} session in the moved workspace");
}

/// Tell the user how `agent` starts in a copied workspace: from a fork of
/// its session when `forked`, otherwise as a new session.
pub(super) fn starting_copied(agent: impl Display, forked: bool) {
    match forked {
        true => eprintln!("shoal: starting {agent} from a fork of its session in the copy"),
        false => eprintln!("shoal: starting a new {agent} session in the copy"),
    }
}

/// List `workspace`'s stopped `commands`, which the restored agent was not
/// told about.
pub(super) fn commands_not_told(workspace: &Workspace, commands: &[SavedCommand]) {
    eprintln!("{}", command_list(workspace, commands));
}

/// The stopped `commands` of `workspace`, each as the command that reruns it.
fn command_list(workspace: &Workspace, commands: &[SavedCommand]) -> String {
    let mut text = format!("Stopped commands in {}, not restarted:", workspace.name);
    for command in commands {
        let mut words = vec!["shoal", "run", "--workspace", &workspace.name, "--"];
        words.extend(command.argv.iter().map(String::as_str));
        text.push_str(&format!("\n  {}", crate::shell::quote(&words)));
    }
    text
}

#[cfg(test)]
mod tests;
