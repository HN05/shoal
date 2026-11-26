//! What `shoal resume` tells the user.
use crate::execution::recovery::changes::Change;
use anyhow::{Result, ensure};
use std::fmt::Display;

use crate::cli::commands::{restore::reopen::ReopenedExt, space_selection::on_machine};
use crate::client::output::{
    agents::Skipped,
    batch::Failure,
    restore::Reopened,
    resume::{Queued, QueuedElsewhere, StoppedCommands},
};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    execution::recovery::{SavedCommand, SavedSession},
    model::Space,
    state::QueueState,
};
use shoal_ids::space_ids::SpaceNumber;
use shoal_protocol::space_names::SpaceName;
use shoal_workers::resume::Resuming;

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
        stopped_commands(self.0, (space.number, &space.name, None), commands);
    }

    fn failed(&self, space: &str, error: &anyhow::Error) -> Failure {
        ui::failure(self.0, "resume", space, error)
    }
}

pub(super) fn resumed(ctx: &Context, resumed: &Queued) -> Result<i32> {
    if !resumed.reports_anything() {
        ctx.emit("No agents to resume", Queued::default())?;
        return Ok(0);
    }
    ctx.show(resumed, |resumed| {
        reopened_lines(ctx, &resumed.reopened);
        skipped_lines(&resumed.skipped);
        for continued in &resumed.continued {
            let agent = (continued.agent.as_ref()).map_or("the agent", |agent| agent.name());
            let space = on_machine(&continued.space, continued.machine.as_deref());
            outln!("Continued the turn of {agent} in {space}");
        }
        for elsewhere in &resumed.elsewhere {
            outln!("{}", queued_elsewhere(elsewhere));
        }
        if let Some(summary) = resumed.summary() {
            outln!("{summary}");
        }
    })?;
    Ok(if resumed.failed.is_empty() { 0 } else { 1 })
}

/// Tell why each of the `skipped` spaces was left, as of an offline
/// machine.
pub(in crate::cli::commands) fn skipped_lines(skipped: &[Skipped]) {
    for skipped in skipped {
        let space = on_machine(&skipped.space, skipped.machine.as_deref());
        let message = format!("Skipped {space}: {}", skipped.reason);
        outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
    }
}

pub(in crate::cli::commands) fn queued(ctx: &Context, queued: &Queued) -> Result<()> {
    ctx.show(queued, |queued| {
        reopened_lines(ctx, &queued.reopened);
        if let Some(summary) = queued.summary() {
            outln!("{summary}");
        }
    })
}

fn reopened_lines(ctx: &Context, reopened: &[Reopened]) {
    for reopened in reopened {
        outln!("{}", reopened.message(ctx, "Restored"));
    }
}

pub(crate) trait QueuedExt {
    fn summary(&self) -> Option<String>;

    fn reports_anything(&self) -> bool;
}

impl QueuedExt for Queued {
    fn reports_anything(&self) -> bool {
        let Self {
            queued,
            commands,
            reopened,
            continued,
            elsewhere,
            skipped,
            failed,
        } = self;
        !(queued.is_empty()
            && commands.is_empty()
            && reopened.is_empty()
            && continued.is_empty()
            && elsewhere.is_empty()
            && skipped.is_empty()
            && failed.is_empty())
    }

    fn summary(&self) -> Option<String> {
        let starting = (self.queued.iter()).all(|entry| entry.state == QueueState::Starting);
        (!self.queued.is_empty()).then(|| summary(self.queued.len(), starting))
    }
}

fn summary(count: usize, starting: bool) -> String {
    format!("{}; shoal queue shows them", agents(count, starting))
}

fn queued_elsewhere(elsewhere: &QueuedElsewhere) -> String {
    let queued = &elsewhere.queued;
    let starting = (queued.iter()).all(|entry| entry.state == QueueState::Starting);
    let agents = agents(queued.len(), starting);
    format!("{agents} in {} on {}", elsewhere.space, elsewhere.machine)
}

fn agents(count: usize, starting: bool) -> String {
    let agents = if count == 1 { "agent" } else { "agents" };
    let done = if starting { "Starting" } else { "Queued" };
    format!("{done} {count} {agents}")
}

pub(in crate::cli::commands) fn stopped_commands(
    ctx: &Context,
    (number, name, machine): (SpaceNumber, &SpaceName, Option<&str>),
    commands: &[Vec<String>],
) {
    if !commands.is_empty() && !ctx.json {
        outln!("{}", command_list((number, name), machine, commands));
    }
}

pub(super) fn commands_only(ctx: &Context, report: &StoppedCommands) -> Result<()> {
    ctx.show(report, |_| {})
}

pub(super) fn restoring(agent: impl Display, reason: &str) {
    eprintln!("shoal: restoring {agent}, stopped: {reason}");
}

pub(super) fn continuing(agent: impl Display) {
    eprintln!("shoal: continuing the last {agent} session");
}

/// The user converted a converted space, so its agent's start needs no notice.
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

pub(super) fn commands_not_told(space: &Space, commands: &[SavedCommand]) {
    let commands: Vec<_> = commands
        .iter()
        .map(|command| command.argv.clone())
        .collect();
    eprintln!(
        "{}",
        command_list((space.number, &space.name), None, &commands)
    );
}

fn command_list(
    (number, name): (SpaceNumber, &SpaceName),
    machine: Option<&str>,
    commands: &[Vec<String>],
) -> String {
    let label = shoal_protocol::space_names::label(number, name);
    let label = on_machine(&label, machine);
    let mut text = format!("Stopped commands in {label}, not restarted:");
    for argv in commands {
        text.push_str(&format!("\n  {}", rerun(number, argv)));
    }
    text
}

/// The command selects the space by its number, since names select nothing.
fn rerun(number: SpaceNumber, argv: &[String]) -> String {
    let number = number.to_string();
    let mut words = vec!["shoal", "run", "--workspace", &number, "--"];
    words.extend(argv.iter().map(String::as_str));
    crate::shell::quote(&words)
}

#[cfg(test)]
mod tests;
