//! A machine's reports of its spaces to the hub: every space once its link
//! opens, which replaces what the hub had for it, then what changed, once a
//! burst of changes settles. The hub's own machine reports to its own
//! directory the same way, for as long as it takes the hub role.
use std::{collections::HashMap, time::Duration};

use anyhow::{Result, anyhow};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    directory::{Report, SpaceSummary},
    link::{Call, Reply},
};

use super::session::Calls;
use crate::daemon::{log, workspace::Manager};

mod summaries;

/// Lets a burst of changes settle into one report.
const SETTLE: Duration = Duration::from_millis(500);

/// How long a machine waits to report all its spaces again after a report
/// failed.
const RETRY: Duration = Duration::from_secs(30);

/// Where a machine's reports go.
enum Sink<'a> {
    /// To the hub, over this machine's link.
    Link(&'a Calls),
    /// To this daemon's own directory, as the hub's machine `MemberId`.
    Hub(MemberId),
}

/// What a sink last took: each space's summary, with its JSON to compare.
type Sent = HashMap<SpaceId, (SpaceSummary, String)>;

impl Manager {
    /// Report this machine's spaces to `sink` until a report fails: all of
    /// them, then each change.
    async fn report_spaces(&self, sink: Sink<'_>) -> Result<()> {
        // Watch before reading, so a change in between still reports.
        let mut stored = self.store.watch_changes();
        let mut agents = self.signals.watch_agents();
        let mut origins = summaries::Origins::default();
        let mut sent = comparable(self.space_summaries(&mut origins).await?)?;
        let whole = sent.values().map(|(summary, _)| summary.clone()).collect();
        self.report(&sink, Report::Whole(whole)).await?;
        loop {
            tokio::select! {
                changed = stored.changed() => changed?,
                changed = agents.changed() => changed?,
            }
            tokio::time::sleep(SETTLE).await;
            stored.borrow_and_update();
            agents.borrow_and_update();
            let current = comparable(self.space_summaries(&mut origins).await?)?;
            if let Some(changes) = changes(&sent, &current) {
                self.report(&sink, changes).await?;
            }
            sent = current;
        }
    }

    /// Report the hub's own machine's spaces to its directory while this
    /// daemon takes the hub role, until the daemon stops; while it does
    /// not, number them on its own unless it joined a hub.
    pub(super) async fn report_own_spaces(&self) {
        let mut reloaded = self.config_state.watch();
        loop {
            if self.is_hub() {
                let reported = async {
                    let own = self.own_member().await?;
                    self.report_spaces(Sink::Hub(own)).await
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
    pub(super) async fn keep_reporting(&self, calls: &Calls) -> std::convert::Infallible {
        loop {
            if let Err(error) = self.report_spaces(Sink::Link(calls)).await {
                log!("hub: could not report this machine's spaces: {error:#}");
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// Send `report` to `sink`, and number this machine's new spaces from
    /// what the hub answers.
    async fn report(&self, sink: &Sink<'_>, report: Report) -> Result<()> {
        let numbering = match sink {
            Sink::Hub(own) => self.record_report(own, report).await?,
            Sink::Link(calls) => match calls.call(Call::Report(report)).await? {
                Reply::Reported(numbering) => numbering,
                reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
            },
        };
        self.take_numbering(numbering).await
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
