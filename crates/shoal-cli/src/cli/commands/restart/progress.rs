//! What a restart tells the user: a numbered line for each selected agent as
//! the daemon answers for it or it is skipped, what goes on when the command
//! is interrupted, and with `--json` the object that lists them.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

use crate::client::output::agents::{PendingAgent, Restarted, RestartedAgent, Skipped};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::{RestartOutcome, RestartWait, Space},
    sessions::Origin,
    terminal,
};

pub(super) struct Progress<'a> {
    ctx: &'a Context,
    report: ui::Report<RestartedAgent>,
    pending: Vec<PendingAgent>,
    skipped: Vec<Skipped>,
    /// How many agents the command reported on so far, and how many it
    /// selected.
    position: usize,
    total: usize,
    /// Where the agent of each selected space ran before its restart.
    places: HashMap<SpaceId, Origin>,
}

impl<'a> Progress<'a> {
    /// Progress of restarting `total` agents, which ran in `places`.
    pub(super) fn new(ctx: &'a Context, total: usize, places: HashMap<SpaceId, Origin>) -> Self {
        Self {
            ctx,
            report: ui::Report::new("restart"),
            pending: Vec::new(),
            skipped: Vec::new(),
            position: 0,
            total,
            places,
        }
    }

    /// The next agent's place among those selected, as `(n/total)`.
    fn next(&mut self) -> String {
        self.position += 1;
        format!("({}/{})", self.position, self.total)
    }

    /// Report an agent the command does not restart.
    pub(super) fn skipped(&mut self, skipped: Skipped) {
        let place = self.next();
        if !self.ctx.json {
            let message = format!("Skipped {} {place}: {}", skipped.space, skipped.reason);
            outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
        }
        self.skipped.push(skipped);
    }

    /// Report what the daemon did with the restart of the agent of `space`.
    pub(super) fn ended(&mut self, space: &Space, outcome: Result<RestartOutcome>) {
        let name = &space.name;
        match outcome {
            Ok(RestartOutcome::Restarted { agent, place }) => {
                let position = self.next();
                let before = self.places.get(&space.id);
                let line = restarted(name, &position, before, place.as_ref());
                let restarted = RestartedAgent {
                    space: name.clone(),
                    agent: Some(agent),
                    place,
                };
                self.report.done(self.ctx, &line, restarted);
            }
            Ok(RestartOutcome::Waits { agent, waits }) => {
                let position = self.next();
                if !self.ctx.json {
                    outln!(
                        "Pending {name} {position}: {}",
                        waiting(&waits, space.number)
                    );
                }
                let space = name.clone();
                self.pending.push(PendingAgent {
                    space,
                    agent,
                    waits,
                });
            }
            Ok(RestartOutcome::Skipped { agent, reason }) => self.skipped(Skipped {
                space: name.clone(),
                agent,
                reason,
            }),
            Err(error) => {
                let message = format!("Failed {name} {}: {error:#}", self.next());
                self.report.failed_saying(self.ctx, &message, name, &error);
            }
        }
    }

    /// Report that the command was interrupted, while the daemon goes on
    /// with the restarts it was asked for; exit 130.
    pub(super) fn interrupted(self) -> Result<i32> {
        if !self.ctx.json {
            eprintln!("Interrupted; the daemon goes on with the restarts it was asked for");
        }
        self.report(true)?;
        Ok(130)
    }

    /// Print the JSON object, exiting 1 when any restart failed.
    pub(super) fn finish(self) -> Result<i32> {
        self.report(false)
    }

    /// Print the JSON object of a command, `interrupted` or not.
    fn report(self, interrupted: bool) -> Result<i32> {
        let (pending, skipped) = (self.pending, self.skipped);
        self.report
            .finish(self.ctx, "", |restarted, failed| Restarted {
                restarted,
                pending,
                failed,
                skipped,
                interrupted,
            })
    }
}

/// The line for the agent of `name`, `position` among those selected, which
/// ran in `before` and runs in `place` now: it names that place as a
/// background start does when the agent moved there.
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

/// What the restart of the agent of the space numbered `number` waits for.
fn waiting(waits: &RestartWait, number: SpaceNumber) -> String {
    match waits {
        RestartWait::Turn => "restarts once its turn ends".into(),
        RestartWait::Processes { issue } => format!(
            "restarts once its remaining processes exit: {issue}; shoal stop {number} cancels the restart"
        ),
    }
}

#[cfg(test)]
mod tests;
