//! What a machine does to another machine's alerts to the user: reading
//! and clearing them go to the hub, or wait in the outbox while the link
//! is down, whose copies take them at once and which passes them on to the
//! machine that keeps them, now while it is connected and else in the
//! answer to its next report; withdrawing one goes to that machine, which
//! decides it, and fails at once while it is offline.
use anyhow::{Result, anyhow};
use shoal_daemon_base::callers::Caller;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::Alert,
    hub::{
        alerts::AlertMarks,
        link::{Call, Reply},
    },
    protocol::{AlertMethod, Body, Operation},
};

use super::copies;
use crate::{Hub, daemon::Daemon, log};

impl Hub {
    /// Read or clear, as `marks` says, alerts to the user that the machine
    /// `machine` keeps.
    pub async fn mark_alerts(
        &self,
        daemon: &dyn Daemon,
        machine: &MemberId,
        marks: AlertMarks,
    ) -> Result<()> {
        if self.is_hub(daemon) {
            return self.take_alert_marks(machine, marks).await;
        }
        let machine = machine.clone();
        self.call_or_hold(Call::MarkAlerts { machine, marks }).await
    }

    /// As the hub: take `marks` of the alerts to the user that `machine`
    /// keeps into its copies, and pass them on to that machine while it is
    /// connected; the answer to its next report carries them otherwise.
    pub(crate) async fn take_alert_marks(
        &self,
        machine: &MemberId,
        marks: AlertMarks,
    ) -> Result<()> {
        let (member, taken) = (machine.clone(), marks.clone());
        let changed = (self.store)
            .write(move |tx| copies::mark(tx, &member, &taken))
            .await?;
        if let Some(changed) = changed {
            self.alert_changes.send(changed);
        }
        if machine == &self.own_member().await? {
            return self.notifications.take_marks(marks).await;
        }
        if let Some(calls) = self.links.calls(machine) {
            tokio::spawn(async move {
                match calls.call(Call::AlertsMarked { marks }).await {
                    Ok(Reply::Delivered) => {}
                    Ok(reply) => log!("hub: a machine answered marks with an unexpected {reply:?}"),
                    Err(error) => log!("hub: could not pass marks of alerts on: {error:#}"),
                }
            });
        }
        Ok(())
    }

    /// Withdraw the alert `id` to the user that the machine `machine`
    /// keeps, as `caller`, which that machine checks sent it: there at
    /// once, and failing at once, naming that machine, while it is
    /// offline.
    pub async fn withdraw_elsewhere(
        &self,
        daemon: &dyn Daemon,
        machine: &MemberId,
        id: i64,
        caller: Option<&Caller>,
    ) -> Result<Alert> {
        let view = self.hub_view(daemon, None).await?;
        let member = (view.as_ref())
            .and_then(|view| view.directory.machine(machine).cloned())
            .ok_or_else(|| anyhow!("the hub lists no machine {machine}, which kept alert {id}"))?;
        let withdraw = AlertMethod::WithdrawAlert {
            id,
            space: None,
            from: None,
        };
        let operation = Operation::Alert(withdraw);
        match self.ask_machine(daemon, &member, operation, caller).await? {
            Body::Alert(alert) => Ok(alert),
            body => Err(anyhow!(
                "machine {} answered with an unexpected {body:?}",
                member.name
            )),
        }
    }
}
