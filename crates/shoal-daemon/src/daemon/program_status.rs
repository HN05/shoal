//! The space's status for the terminal its agent runs in, through the
//! Program Status Protocol: the daemon decides each report from the space's
//! records and sends it to the agent's execution wrapper whenever it
//! changes, once the wrapper says its terminal reads the protocol. The
//! daemon never writes to a terminal itself.
mod decision;
mod facts;

use std::{future::Future, time::Duration};

use shoal_protocol::program_status::ProgramStatus;
use tokio::sync::watch;

use super::{execution_connection::Controls, manager::Manager};
use crate::{agent::Agent, model::Space, protocol::Control};
use shoal_ids::execution_ids::ExecutionId;

/// Lets a burst of record changes settle into one report.
const SETTLE: Duration = Duration::from_millis(200);

/// The reports of one connected execution.
pub(super) struct Reports {
    pub(super) space: Space,
    pub(super) execution: ExecutionId,
    /// The agent the execution runs; a command's space reports nothing.
    pub(super) agent: Option<Agent>,
}

impl Reports {
    /// Send the space's status through `controls` each time it changes,
    /// once `wanted` says the wrapper reports it, until `ended`.
    pub(super) async fn run(
        self,
        manager: &Manager,
        controls: &Controls,
        mut wanted: watch::Receiver<bool>,
        mut ended: watch::Receiver<bool>,
    ) {
        let Some(agent) = &self.agent else {
            return;
        };
        // Watch before reading, so a change in between still reports.
        let mut changes = manager.store.watch_changes();
        let wanted = async { wanted.wait_for(|wanted| *wanted).await.ok().map(drop) };
        if !before_end(&mut ended, wanted).await {
            return;
        }
        let reports_itself = self.reports_itself(manager, agent).await;
        let mut sent = None;
        while self
            .send_change(manager, controls, reports_itself, &mut sent)
            .await
            && before_end(&mut ended, settled(&mut changes)).await
        {}
    }

    /// Send the space's status unless it is the one `sent`; whether the
    /// wrapper still hears it.
    async fn send_change(
        &self,
        manager: &Manager,
        controls: &Controls,
        reports_itself: bool,
        sent: &mut Option<ProgramStatus>,
    ) -> bool {
        let status = self.status(manager, reports_itself).await;
        let Some(status) = status.filter(|status| sent.as_ref() != Some(status)) else {
            return true;
        };
        let control = Control::ProgramStatus {
            status: status.clone(),
        };
        *sent = Some(status);
        controls.send(&control).await.is_ok()
    }

    /// Whether `agent` reports its own turns to the terminal.
    async fn reports_itself(&self, manager: &Manager, agent: &Agent) -> bool {
        let settings = manager.space_settings(&self.space).await;
        // Without its settings, Shoal reports what it knows of the agent too.
        settings.is_ok_and(|settings| {
            crate::agents::agent_integration(&settings, agent).reports_program_status()
        })
    }

    /// The space's status now; none once the space is gone or its records
    /// cannot be read.
    async fn status(&self, manager: &Manager, reports_itself: bool) -> Option<ProgramStatus> {
        let (space, execution) = (self.space.id.clone(), self.execution.clone());
        let facts = manager
            .store
            .read(move |db| facts::read(db, &space, &execution))
            .await;
        let facts = facts.ok()??;
        Some(decision::status(&facts, reports_itself))
    }
}

/// The next change of the store, once a burst of changes settles; none once
/// the store stops.
async fn settled(changes: &mut watch::Receiver<()>) -> Option<()> {
    changes.changed().await.ok()?;
    tokio::time::sleep(SETTLE).await;
    changes.borrow_and_update();
    Some(())
}

/// Whether `wait` gives a value before the connection ends. A wait that
/// gives none, such as on a closed channel, waits for the end.
async fn before_end(
    ended: &mut watch::Receiver<bool>,
    wait: impl Future<Output = Option<()>>,
) -> bool {
    let waited = tokio::select! {
        biased;
        _ = ended.wait_for(|ended| *ended) => return false,
        waited = wait => waited,
    };
    if waited.is_none() {
        let _ = ended.wait_for(|ended| *ended).await;
    }
    waited.is_some()
}

#[cfg(test)]
mod tests;
