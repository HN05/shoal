//! A machine's reports of its alerts to the hub.
use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use shoal_daemon_base::notifications;
use shoal_protocol::{
    alerts::Alert,
    hub::{
        alerts::{AlertsReport, UsersAlerts},
        link::{Call, Reply},
    },
};
use tokio::time::Instant;

use super::numbering;
use crate::{
    Hub,
    daemon::Daemon,
    log,
    reports::{QUIET, RETRY, SETTLE, Sink},
    session::Calls,
};

/// What a machine reports of its alerts: what it reports of their numbers,
/// and the user's alerts it keeps, by number, as other machines read them.
#[derive(Default, PartialEq)]
struct Reported {
    numbers: AlertsReport,
    users: BTreeMap<i64, Alert>,
}

impl Hub {
    /// Report this machine's alerts to `sink` until a report fails.
    pub(crate) async fn report_alerts(&self, daemon: &dyn Daemon, sink: &Sink<'_>) -> Result<()> {
        // Watch before reading, so a change in between still reports.
        let mut stored = self.store.watch_changes();
        let current = self.reported_alerts(daemon).await?;
        let users = UsersAlerts::Whole(current.users.values().cloned().collect());
        let mut sent = self
            .report_alerts_once(daemon, sink, current, users)
            .await?;
        let mut reported = Instant::now();
        loop {
            tokio::select! {
                changed = stored.changed() => changed?,
                () = tokio::time::sleep_until(reported + QUIET) => {}
            }
            tokio::time::sleep(SETTLE).await;
            stored.borrow_and_update();
            let current = self.reported_alerts(daemon).await?;
            if current != sent || reported.elapsed() >= QUIET {
                let users = changes(&sent.users, &current.users);
                sent = (self.report_alerts_once(daemon, sink, current, users)).await?;
                reported = Instant::now();
            }
        }
    }

    /// Report this machine's alerts over its link to the hub for as long
    /// as the link stays open; after a report fails, it reports them all
    /// again once [`RETRY`] passes.
    pub(crate) async fn keep_reporting_alerts(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
    ) -> std::convert::Infallible {
        loop {
            if let Err(error) = self.report_alerts(daemon, &Sink::Link(calls)).await {
                log!("hub: could not report this machine's alerts: {error:#}");
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// What this machine reports of its alerts now, its files named as
    /// kept on this machine.
    async fn reported_alerts(&self, daemon: &dyn Daemon) -> Result<Reported> {
        let machine = self.machine_name(daemon).await;
        let (numbers, kept) = (self.store)
            .read(|tx| Ok((numbering::reported(tx)?, notifications::kept(tx)?)))
            .await?;
        let users = (kept.into_iter())
            .map(|mut alert| {
                alert.files_kept_on(&machine);
                (alert.id, alert)
            })
            .collect();
        Ok(Reported { numbers, users })
    }

    /// Send `sink` `current`, with `users` for the user's alerts, and take the
    /// numbers it answers with. Returns what the machine reports once it took
    /// them, with the user's alerts as the hub has them.
    async fn report_alerts_once(
        &self,
        daemon: &dyn Daemon,
        sink: &Sink<'_>,
        current: Reported,
        users: UsersAlerts,
    ) -> Result<Reported> {
        let report = AlertsReport {
            users: Some(users),
            ..current.numbers.clone()
        };
        let answer = match sink {
            Sink::Hub(own) => self.record_alert_report(own, report).await?,
            Sink::Link(calls) => match calls.call(Call::ReportAlerts { report }).await? {
                Reply::AlertsReported(numbering) => numbering,
                reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
            },
        };
        let numbers = current.numbers.clone();
        self.take_alert_numbering(daemon, numbers, answer).await?;
        let numbers = self.store.read(numbering::reported).await?;
        Ok(Reported {
            numbers,
            users: current.users,
        })
    }
}

fn changes(before: &BTreeMap<i64, Alert>, after: &BTreeMap<i64, Alert>) -> UsersAlerts {
    let changed = (after.iter())
        .filter(|(id, alert)| before.get(*id) != Some(*alert))
        .map(|(_, alert)| alert.clone())
        .collect();
    let removed = (before.keys())
        .filter(|id| !after.contains_key(*id))
        .copied()
        .collect();
    UsersAlerts::Changes { changed, removed }
}
