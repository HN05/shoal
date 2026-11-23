//! The alerts a hub's machines keep, numbered from one sequence across
//! them: each machine reports what the hub needs of its alerts, at once
//! once its link opens and then as they change, and numbers its new ones
//! from the spares the answer brings. The hub's own machine reports to the
//! hub here the same way.
use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::alerts::{AlertNumbering, AlertsReport},
};

use crate::{Hub, daemon::Daemon, log};

pub(crate) mod numbering;
mod reporting;
pub(crate) mod sequence;

impl Hub {
    /// As the hub: take `member`'s report of its alerts, and answer with
    /// the alert numbers it keeps for it.
    pub(crate) async fn record_alert_report(
        &self,
        member: &MemberId,
        report: AlertsReport,
    ) -> Result<AlertNumbering> {
        let member = member.clone();
        (self.store)
            .write(move |tx| sequence::number(tx, &member, &report))
            .await
    }

    /// Number this machine's alerts as `numbering`, the hub's answer to
    /// `reported`, says, telling the agent of the space that sent each
    /// unread alert it renumbers.
    async fn take_alert_numbering(
        &self,
        daemon: &dyn Daemon,
        reported: AlertsReport,
        numbering: AlertNumbering,
    ) -> Result<()> {
        let records = self.records.clone();
        let renumbered = (self.store)
            .write(move |tx| numbering::take(tx, &*records, &reported, &numbering))
            .await?;
        for alert in renumbered {
            self.announce_alert_renumbering(daemon, alert).await;
        }
        Ok(())
    }

    /// Tell the agent of the space that sent `alert`, which is unread, its
    /// new number, by which it withdraws it.
    async fn announce_alert_renumbering(&self, daemon: &dyn Daemon, alert: numbering::Renumbered) {
        let numbering::Renumbered {
            from,
            to,
            sender,
            message,
        } = alert;
        let message = format!(
            "Your alert {from} is alert {to} now, since an alert on another machine of the hub took {from} first; withdraw it as {to}: {message}"
        );
        let message = shoal_protocol::validate::shorten(message, 512);
        let kind = AlertKind::AlertRenumbered;
        let told = match daemon.find_space(&sender).await {
            Ok(Some(_)) => daemon.message_next_turn(&sender, kind, message).await,
            Ok(None) => self.message_elsewhere(daemon, &sender, kind, message).await,
            Err(error) => Err(error),
        };
        if let Err(error) = told {
            log!("hub: could not tell space {sender} its alert's new number: {error:#}");
        }
    }
}
