//! What a run tells the user: a numbered line for each selected agent as
//! its restart ends or is skipped, whose turns it waits for, what goes on
//! when it is interrupted, and with `--json` the object that lists them.
use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceNumber;
use crate::client::space_names::{self, SpaceName};
use std::collections::HashMap;

use anyhow::Result;

use crate::client::output::agents::{Restarted, RestartedAgent, Skipped};
use crate::client::restarts::{self, Restart};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::RunningAgent,
    sessions::Origin,
    terminal,
};

pub(super) struct Progress<'a> {
    ctx: &'a Context,
    report: ui::Report<RestartedAgent>,
    skipped: Vec<Skipped>,
    /// How many agents the run reported on so far, and how many it selected.
    position: usize,
    total: usize,
    /// The number and name of each agent's space to restart, by execution.
    spaces: HashMap<ExecutionId, (SpaceNumber, SpaceName)>,
}

impl<'a> Progress<'a> {
    /// Progress of restarting `restarts` and skipping `skipped` more.
    pub(super) fn new(ctx: &'a Context, restarts: &[Restart], skipped: usize) -> Self {
        Self {
            ctx,
            report: ui::Report::new("restart"),
            skipped: Vec::new(),
            position: 0,
            total: restarts.len() + skipped,
            spaces: restarts
                .iter()
                .map(|restart| {
                    let execution = restart.agent.execution.clone();
                    let space = (restart.space.number(), restart.space.name().clone());
                    (execution, space)
                })
                .collect(),
        }
    }

    /// The next agent's place in the run, as `(n/total)`.
    fn next(&mut self) -> String {
        self.position += 1;
        format!("({}/{})", self.position, self.total)
    }

    /// Report that the run was interrupted, naming the agents of the run in
    /// `agents` whose restart goes on without it and the command, selecting
    /// only `agent` agents as the run did, that reports on them; exit 130.
    pub(super) fn interrupted(
        self,
        agents: &[RunningAgent],
        agent: Option<&crate::agent::Agent>,
    ) -> Result<i32> {
        let pending: Vec<(SpaceNumber, SpaceName)> = agents
            .iter()
            .filter(|agent| agent.restart.is_some())
            .filter_map(|agent| self.space_of(agent))
            .collect();
        if !self.ctx.json {
            eprintln!("{}", interruption(&pending, agent));
        }
        let pending = pending.into_iter().map(|(_, name)| name).collect();
        self.report(true, pending)?;
        Ok(130)
    }

    /// The space of `agent`, selected as the execution it runs or ran as.
    fn space_of(&self, agent: &RunningAgent) -> Option<(SpaceNumber, SpaceName)> {
        std::iter::once(&agent.execution)
            .chain(&agent.restarted_from)
            .find_map(|execution| self.spaces.get(execution).cloned())
    }

    /// Print the JSON object, exiting 1 when any restart failed.
    pub(super) fn finish(self) -> Result<i32> {
        self.report(false, Vec::new())
    }

    /// Print the JSON object, of a run `interrupted` with the restarts of
    /// `pending` going on without it.
    fn report(self, interrupted: bool, pending: Vec<SpaceName>) -> Result<i32> {
        let skipped = self.skipped;
        self.report
            .finish(self.ctx, "", |restarted, failed| Restarted {
                restarted,
                failed,
                skipped,
                interrupted,
                pending,
            })
    }
}

impl restarts::Progress for Progress<'_> {
    /// Report an agent the run does not restart.
    fn skipped(&mut self, skipped: Skipped) {
        let place = self.next();
        if !self.ctx.json {
            let message = format!("Skipped {} {place}: {}", skipped.space, skipped.reason);
            outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
        }
        self.skipped.push(skipped);
    }

    /// Report how the restart of `restart` ended, with the place its agent
    /// runs in now when it runs again.
    fn ended(&mut self, restart: &Restart, result: Result<Option<Origin>>) {
        let position = self.next();
        let name = restart.space.name();
        match result {
            Ok(place) => self.report.done(
                self.ctx,
                &restarted(
                    name,
                    &position,
                    restart.agent.origin.as_ref(),
                    place.as_ref(),
                ),
                RestartedAgent {
                    space: name.clone(),
                    agent: restart.agent.agent.clone(),
                    place,
                },
            ),
            Err(error) => {
                let (message, error) = match restarts::waits(&error) {
                    Some(waits) => (format!("Pending {name} {position}: {waits}"), error),
                    None => (format!("Failed {name} {position}: {error:#}"), error),
                };
                self.report.failed_saying(self.ctx, &message, name, &error);
            }
        }
    }

    /// Say whose turns the run waits for.
    fn waiting(&mut self, spaces: &[&SpaceName]) {
        if !self.ctx.json {
            let names: Vec<_> = spaces.iter().map(|name| name.as_str()).collect();
            eprintln!("Waiting for the turns of {} to end", names.join(", "));
        }
    }
}

/// The line for the agent of `name`, `position` in the run, which ran in
/// `before` and runs in `place` now: it names that place as a background
/// start does when the agent moved there.
fn restarted(
    name: &str,
    position: &str,
    before: Option<&Origin>,
    place: Option<&Origin>,
) -> String {
    let moved = place
        .filter(|place| before.is_none_or(|before| !before.same_place(place)))
        .and_then(|place| terminal::background_place(place, name));
    match moved {
        Some(place) => format!("Restarted {name} {position} {place}"),
        None => format!("Restarted {name} {position}"),
    }
}

fn interruption(
    pending: &[(SpaceNumber, SpaceName)],
    agent: Option<&crate::agent::Agent>,
) -> String {
    if pending.is_empty() {
        return "Interrupted".into();
    }
    let filter = agent
        .map(|agent| format!(" --agent {agent}"))
        .unwrap_or_default();
    let joined = |each: fn(&(SpaceNumber, SpaceName)) -> String, separator| {
        pending.iter().map(each).collect::<Vec<_>>().join(separator)
    };
    format!(
        "Interrupted; the restarts of {} go on, and shoal restart {}{filter} reports on them",
        joined(|(number, name)| space_names::label(*number, name), ", "),
        joined(|(number, _)| number.to_string(), " "),
    )
}

#[cfg(test)]
mod tests;
