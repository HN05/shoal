//! A machine's reports of its spaces, capacity and repositories to the hub.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Result, anyhow};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    directory::{RegisteredRepository, Report, SpaceSummary},
    link::{Call, Reply},
};
use tokio::{sync::watch, time::Instant};

use super::session::Calls;
use crate::{Hub, daemon::Daemon, directory::Held, log};

mod agents;
mod capacity;
mod summaries;

use capacity::Measured;

/// Lets a burst of changes settle into one report.
pub(crate) const SETTLE: Duration = Duration::from_millis(500);

/// How long a machine goes without a report before it reports that nothing
/// changed. The answer brings the numbers the hub changed on its own, as
/// when another machine ending a space makes this machine's copy the listed
/// one.
pub(crate) const QUIET: Duration = Duration::from_secs(5 * 60);

/// How long a machine waits to report all its spaces again after a report
/// failed.
pub(crate) const RETRY: Duration = Duration::from_secs(30);

/// How long a machine waits for the hub to take a report of a space
/// another machine looks for there before it goes on anyway.
const LISTED_WAIT: Duration = Duration::from_secs(30);

/// The spaces of this machine the hub took in its last report.
#[derive(Default)]
pub(crate) struct Reported(watch::Sender<HashSet<SpaceId>>);

/// Where a machine's reports go.
pub(crate) enum Sink<'a> {
    /// To the hub, over this machine's link.
    Link(&'a Calls),
    /// To this daemon's own directory, as the hub's machine `MemberId`.
    Hub(MemberId),
}

/// What a sink last took: each space's summary, with its JSON to compare.
type Sent = HashMap<SpaceId, (SpaceSummary, String)>;

impl Hub {
    /// Report this machine's spaces to `sink` until a report fails: all of
    /// them, then each change, and at least every [`QUIET`], each with the
    /// capacity and power this machine measures meanwhile.
    async fn report_spaces(&self, daemon: &dyn Daemon, sink: Sink<'_>) -> Result<()> {
        let (measuring, measured) = watch::channel(Measured::default());
        tokio::select! {
            reported = self.report_changes(daemon, sink, measured) => reported,
            never = self.keep_measuring(daemon, measuring) => match never {},
        }
    }

    /// Report this machine's spaces to `sink` as [`Self::report_spaces`]
    /// does, with the latest capacity and power `measured` holds, and again
    /// whenever they change.
    async fn report_changes(
        &self,
        daemon: &dyn Daemon,
        sink: Sink<'_>,
        mut measured: watch::Receiver<Measured>,
    ) -> Result<()> {
        // Watch before reading, so a change in between still reports.
        let mut stored = self.store.watch_changes();
        let mut agents = daemon.watch_agents();
        // Apart, so a registration read before its checkout had an origin
        // keeps no space's summary from naming it.
        let (mut origins, mut registered_origins) = Default::default();
        let mut sent = comparable(self.space_summaries(daemon, &mut origins).await?)?;
        let mut held = self.held().await?;
        let mut registered = (self.registrations(daemon, &mut registered_origins)).await?;
        let whole = sent.values().map(|(summary, _)| summary.clone()).collect();
        let report = (Report::Whole(whole), measured.borrow_and_update().clone());
        let holds = (held.clone(), Some(registered.clone()));
        self.report(daemon, &sink, report, holds).await?;
        self.reported.0.send_replace(sent.keys().cloned().collect());
        // Counted from the last report, so changes that report nothing do
        // not put the next one off.
        let mut reported = Instant::now();
        loop {
            let mut moved = false;
            tokio::select! {
                changed = stored.changed() => changed?,
                changed = agents.changed() => changed?,
                changed = measured.changed() => {
                    changed?;
                    moved = true;
                }
                () = tokio::time::sleep_until(reported + QUIET) => {}
            }
            tokio::time::sleep(SETTLE).await;
            stored.borrow_and_update();
            agents.borrow_and_update();
            let current = comparable(self.space_summaries(daemon, &mut origins).await?)?;
            let holds = self.held().await?;
            let registrations = (self.registrations(daemon, &mut registered_origins)).await?;
            let changes = changes(&sent, &current);
            let repositories = (registrations != registered).then(|| registrations.clone());
            if reported.elapsed() >= QUIET
                || changes.is_some()
                || holds != held
                || repositories.is_some()
                || moved
            {
                let changes = changes.unwrap_or_else(|| Report::Changes {
                    changed: Vec::new(),
                    removed: Vec::new(),
                });
                let report = (changes, measured.borrow_and_update().clone());
                self.report(daemon, &sink, report, (holds.clone(), repositories))
                    .await?;
                self.reported
                    .0
                    .send_replace(current.keys().cloned().collect());
                reported = Instant::now();
            }
            (sent, held, registered) = (current, holds, registrations);
        }
    }

    /// Wait until the hub took a report of this machine's space `id`, so other
    /// machines find it there. A machine that started a space for another waits
    /// before it answers, so that machine can follow, attach to and name it at
    /// once. One that asks another to start a space below waits before it asks.
    /// It stops waiting after [`LISTED_WAIT`], as when the link went down.
    pub async fn listed(&self, id: &SpaceId) {
        let mut reported = self.reported.0.subscribe();
        let listed = reported.wait_for(|reported| reported.contains(id));
        let _ = tokio::time::timeout(LISTED_WAIT, listed).await;
    }

    /// Report the hub's own machine's spaces and alerts to the hub while
    /// this daemon takes the hub role, until the daemon stops; while it
    /// does not, number them on its own unless it joined a hub.
    pub(super) async fn report_own_spaces(&self, daemon: &dyn Daemon) {
        let mut reloaded = self.config.watch();
        loop {
            if self.is_hub(daemon) {
                let reported = async {
                    let own = self.own_member().await?;
                    let alerts = Sink::Hub(own.clone());
                    tokio::select! {
                        reported = self.report_spaces(daemon, Sink::Hub(own)) => reported,
                        reported = self.report_alerts(daemon, &alerts) => reported,
                    }
                };
                tokio::select! {
                    result = reported => {
                        if let Err(error) = result {
                            log!("hub: could not report this machine's spaces: {error:#}");
                        }
                        tokio::time::sleep(RETRY).await;
                        continue;
                    }
                    changed = reloaded.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                }
            } else {
                if let Err(error) = self.forget_numbering_unless_joined().await {
                    log!("hub: could not stop numbering from the hub's sequence: {error:#}");
                }
                if reloaded.changed().await.is_err() {
                    return;
                }
            }
            reloaded.borrow_and_update();
        }
    }

    /// Report this machine's spaces over its link to the hub for as long
    /// as the link stays open; after a report fails, it reports them all
    /// again once [`RETRY`] passes.
    pub(super) async fn keep_reporting(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
    ) -> std::convert::Infallible {
        loop {
            if let Err(error) = self.report_spaces(daemon, Sink::Link(calls)).await {
                log!("hub: could not report this machine's spaces: {error:#}");
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// What this machine holds of the spaces it has not.
    async fn held(&self) -> Result<Held> {
        let records = self.records.clone();
        (self.store)
            .read(move |db| {
                Ok(Held {
                    reserved: records.reserved_numbers(db)?,
                    kept: records.kept_removed(db)?,
                })
            })
            .await
    }

    /// Send `report` to `sink`, with what this machine `measured` of its
    /// capacity and power, what it `held` of the spaces it does not have, and
    /// its `repositories`, none when unchanged. Numbers this machine's spaces
    /// from the hub's answer.
    async fn report(
        &self,
        daemon: &dyn Daemon,
        sink: &Sink<'_>,
        (report, measured): (Report, Measured),
        (held, repositories): (Held, Option<Vec<RegisteredRepository>>),
    ) -> Result<()> {
        let Measured { capacity, power } = measured;
        let numbering = match sink {
            Sink::Hub(own) => {
                self.capacities.keep(own, &report, capacity);
                let held = (held, repositories);
                self.record_report(daemon, own, report, held).await?
            }
            Sink::Link(calls) => {
                let call = Call::Report {
                    report,
                    reserved: held.reserved,
                    kept: held.kept,
                    capacity,
                    repositories,
                    power,
                    sent_at: crate::time::UnixTime::now(),
                };
                match calls.call(call).await? {
                    Reply::Reported(numbering) => numbering,
                    reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
                }
            }
        };
        self.take_numbering(daemon, numbering).await
    }
}

/// `summaries` with the JSON each compares by.
fn comparable(summaries: HashMap<SpaceId, SpaceSummary>) -> Result<Sent> {
    (summaries.into_iter())
        .map(|(id, summary)| {
            let json = serde_json::to_string(&summary)?;
            Ok((id, (summary, json)))
        })
        .collect()
}

/// What changed from `before` to `after`, none when nothing did.
fn changes(before: &Sent, after: &Sent) -> Option<Report> {
    let changed: Vec<SpaceSummary> = (after.iter())
        .filter(|(id, (_, json))| before.get(*id).is_none_or(|(_, sent)| sent != json))
        .map(|(_, (summary, _))| summary.clone())
        .collect();
    let removed: Vec<SpaceId> = (before.keys())
        .filter(|id| !after.contains_key(*id))
        .cloned()
        .collect();
    (!changed.is_empty() || !removed.is_empty()).then_some(Report::Changes { changed, removed })
}

#[cfg(test)]
mod tests;
