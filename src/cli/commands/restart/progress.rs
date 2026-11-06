//! What a run tells the user: a numbered line for each selected agent as
//! its restart ends or is skipped, what goes on when it is interrupted, and
//! with `--json` the object that lists them.
use std::collections::HashMap;

use anyhow::Result;

use super::plan::Restart;
use crate::client::output::agents::{Restarted, RestartedAgent, Skipped};
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

pub(super) struct Progress {
    report: ui::Report<RestartedAgent>,
    skipped: Vec<Skipped>,
    /// How many agents the run reported on so far, and how many it selected.
    position: usize,
    total: usize,
    /// The space of each agent to restart, by execution.
    spaces: HashMap<String, String>,
}

impl Progress {
    /// Progress of restarting `restarts` and skipping `skipped` more.
    pub(super) fn new(restarts: &[Restart], skipped: usize) -> Self {
        Self {
            report: ui::Report::new("restart"),
            skipped: Vec::new(),
            position: 0,
            total: restarts.len() + skipped,
            spaces: restarts
                .iter()
                .map(|restart| {
                    let execution = restart.agent.execution.clone();
                    (execution, restart.space.name().to_owned())
                })
                .collect(),
        }
    }

    /// The next agent's place in the run, as `(n/total)`.
    fn next(&mut self) -> String {
        self.position += 1;
        format!("({}/{})", self.position, self.total)
    }

    /// Report an agent the run does not restart.
    pub(super) fn skip(&mut self, ctx: &Context, skipped: Skipped) {
        let place = self.next();
        if !ctx.json {
            let message = format!("Skipped {} {place}: {}", skipped.space, skipped.reason);
            outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
        }
        self.skipped.push(skipped);
    }

    /// Report how the restart of `restart` ended, with the place its agent
    /// runs in now when it runs again.
    pub(super) fn ended(
        &mut self,
        ctx: &Context,
        restart: &Restart,
        result: Result<Option<Origin>>,
    ) {
        let position = self.next();
        let name = restart.space.name();
        match result {
            Ok(place) => self.report.done(
                ctx,
                &restarted(
                    name,
                    &position,
                    restart.agent.origin.as_ref(),
                    place.as_ref(),
                ),
                RestartedAgent {
                    space: name.to_owned(),
                    agent: restart.agent.agent.clone(),
                    place,
                },
            ),
            Err(error) => {
                let message = match super::waits(&error) {
                    Some(waits) => format!("Pending {name} {position}: {waits}"),
                    None => format!("Failed {name} {position}: {error:#}"),
                };
                self.report.failed_saying(ctx, &message, name, &error);
            }
        }
    }

    /// Report that the run was interrupted, naming the agents of the run in
    /// `agents` whose restart goes on without it and the command, selecting
    /// only `agent` agents as the run did, that reports on them; exit 130.
    pub(super) fn interrupted(
        self,
        ctx: &Context,
        agents: &[RunningAgent],
        agent: Option<&str>,
    ) -> Result<i32> {
        let pending: Vec<String> = agents
            .iter()
            .filter(|agent| agent.restart.is_some())
            .filter_map(|agent| self.space_of(agent))
            .collect();
        if !ctx.json {
            eprintln!("{}", interruption(&pending, agent));
        }
        self.report(ctx, true, pending)?;
        Ok(130)
    }

    /// The space of `agent`, selected as the execution it runs or ran as.
    fn space_of(&self, agent: &RunningAgent) -> Option<String> {
        std::iter::once(&agent.execution)
            .chain(&agent.restarted_from)
            .find_map(|execution| self.spaces.get(execution).cloned())
    }

    /// Print the JSON object, exiting 1 when any restart failed.
    pub(super) fn finish(self, ctx: &Context) -> Result<i32> {
        self.report(ctx, false, Vec::new())
    }

    /// Print the JSON object, of a run `interrupted` with the restarts of
    /// `pending` going on without it.
    fn report(self, ctx: &Context, interrupted: bool, pending: Vec<String>) -> Result<i32> {
        let skipped = self.skipped;
        self.report.finish(ctx, "", |restarted, failed| Restarted {
            restarted,
            failed,
            skipped,
            interrupted,
            pending,
        })
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

fn interruption(pending: &[String], agent: Option<&str>) -> String {
    if pending.is_empty() {
        return "Interrupted".into();
    }
    let filter = agent
        .map(|agent| format!(" --agent {agent}"))
        .unwrap_or_default();
    format!(
        "Interrupted; the restarts of {} go on, and shoal restart {}{filter} reports on them",
        pending.join(", "),
        pending.join(" ")
    )
}

#[cfg(test)]
mod tests;
