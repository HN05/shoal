//! What a restart tells the user, a line for each selected agent as the daemon
//! answers for it.
use std::collections::HashMap;

use anyhow::Result;
use shoal_ids::space_ids::{SpaceId, SpaceNumber};

use super::target::Target;
use crate::cli::commands::space_selection::on_machine;
use crate::client::output::agents::{PendingAgent, Restarted, RestartedAgent, Skipped};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::{RestartOutcome, RestartWait},
    sessions::{Origin, OriginExt as _},
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
    places: HashMap<SpaceId, Origin>,
}

impl<'a> Progress<'a> {
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

    pub(super) fn skipped(&mut self, skipped: Skipped) {
        let place = self.next();
        if !self.ctx.json {
            let space = on_machine(&skipped.space, skipped.machine.as_deref());
            let message = format!("Skipped {space} {place}: {}", skipped.reason);
            outln!("{}", Palette::stdout(false).paint(Style::Warning, &message));
        }
        self.skipped.push(skipped);
    }

    pub(super) fn ended(&mut self, space: &Target, outcome: Result<RestartOutcome>) {
        let (name, shown, machine) = (&space.name, space.shown(), space.machine_name());
        match outcome {
            Ok(RestartOutcome::Restarted { agent, place }) => {
                let position = self.next();
                let before = self.places.get(&space.id);
                let line = restarted((name, &shown), &position, before, place.as_ref());
                let restarted = RestartedAgent {
                    space: name.clone(),
                    agent: Some(agent),
                    machine,
                    place,
                };
                self.report.done(self.ctx, &line, restarted);
            }
            Ok(RestartOutcome::Waits { agent, waits }) => {
                let position = self.next();
                if !self.ctx.json {
                    outln!(
                        "Pending {shown} {position}: {}",
                        waiting(&waits, space.number)
                    );
                }
                let space = name.clone();
                self.pending.push(PendingAgent {
                    space,
                    agent,
                    machine,
                    waits,
                });
            }
            Ok(RestartOutcome::Skipped { agent, reason }) => self.skipped(Skipped {
                space: name.clone(),
                agent,
                machine,
                reason,
            }),
            Err(error) => {
                let message = format!("Failed {shown} {}: {error:#}", self.next());
                self.report
                    .failed_saying(self.ctx, &message, &shown, &error);
            }
        }
    }

    /// The daemon goes on with the restarts it was asked for; exit 130.
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

fn restarted(
    (name, shown): (&str, &str),
    position: &str,
    before: Option<&Origin>,
    place: Option<&Origin>,
) -> String {
    let moved = place
        .filter(|place| before.is_none_or(|before| !before.same_place(place)))
        .and_then(|place| terminal::background_place(place, name));
    match moved {
        Some(place) => format!("Restarted {shown} {position} {place}"),
        None => format!("Restarted {shown} {position}"),
    }
}

fn waiting(waits: &RestartWait, number: SpaceNumber) -> String {
    match waits {
        RestartWait::Turn => {
            format!("restarts once its turn ends; shoal restart --cancel {number} cancels it")
        }
        RestartWait::Line => format!(
            "restarts in turn with the other sequential restarts; shoal restart --cancel {number} cancels it"
        ),
        RestartWait::Processes { issue } => format!(
            "restarts once its remaining processes exit: {issue}; shoal stop {number} cancels the restart"
        ),
    }
}

#[cfg(test)]
mod tests;
