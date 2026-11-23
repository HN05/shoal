//! A machine's reports of its alerts to the hub: at once, then whenever
//! what it reports changed, once a burst of changes settles, and at least
//! every [`QUIET`], each answered with the alert numbers the hub keeps for
//! it.
use anyhow::{Result, anyhow};
use shoal_protocol::hub::{
    alerts::AlertsReport,
    link::{Call, Reply},
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

impl Hub {
    /// Report this machine's alerts to `sink` until a report fails.
    pub(crate) async fn report_alerts(&self, daemon: &dyn Daemon, sink: &Sink<'_>) -> Result<()> {
        // Watch before reading, so a change in between still reports.
        let mut stored = self.store.watch_changes();
        let mut sent = self.report_alerts_once(daemon, sink).await?;
        let mut reported = Instant::now();
        loop {
            tokio::select! {
                changed = stored.changed() => changed?,
                () = tokio::time::sleep_until(reported + QUIET) => {}
            }
            tokio::time::sleep(SETTLE).await;
            stored.borrow_and_update();
            let current = self.store.read(numbering::reported).await?;
            if current != sent || reported.elapsed() >= QUIET {
                sent = self.report_alerts_once(daemon, sink).await?;
                reported = Instant::now();
            }
        }
    }

    /// Report this machine's alerts over its link to the hub for as long
    /// as the link stays open; after a report fails, it reports again once
    /// [`RETRY`] passes.
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

    /// Send `sink` what this machine reports of its alerts now and take the
    /// numbers it answers with, returning what the machine reports once it
    /// took them.
    async fn report_alerts_once(
        &self,
        daemon: &dyn Daemon,
        sink: &Sink<'_>,
    ) -> Result<AlertsReport> {
        let report = self.store.read(numbering::reported).await?;
        let answer = match sink {
            Sink::Hub(own) => self.record_alert_report(own, report.clone()).await?,
            Sink::Link(calls) => {
                let call = Call::ReportAlerts {
                    report: report.clone(),
                };
                match calls.call(call).await? {
                    Reply::AlertsReported(numbering) => numbering,
                    reply => return Err(anyhow!("the hub answered with an unexpected {reply:?}")),
                }
            }
        };
        self.take_alert_numbering(daemon, report, answer).await?;
        self.store.read(numbering::reported).await
    }
}
