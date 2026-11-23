//! The alerts a hub's machines keep, numbered from one sequence across
//! them: each machine reports what the hub needs of its alerts, at once
//! once its link opens and then as they change, the user's alerts among
//! them, and numbers its new ones from the spares the answer brings. The
//! hub keeps a copy of every machine's alerts to the user, which every
//! machine follows. The hub's own machine reports to the hub here the same
//! way.
use anyhow::Result;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    alerts::AlertKind,
    hub::alerts::{AlertCopies, AlertNumbering, AlertsReport},
};

use crate::{Hub, daemon::Daemon, log};

pub(crate) mod copies;
mod following;
mod marking;
pub(crate) mod numbering;
mod reporting;
pub(crate) mod sequence;

/// As the hub: drop the alerts of `member`, which it revoked, in the
/// transaction that revokes it, and the numbers it kept for them.
pub(crate) fn forget(tx: &rusqlite::Transaction<'_>, member: &MemberId) -> Result<()> {
    copies::forget(tx, member)?;
    sequence::forget(tx, member)
}

impl Hub {
    /// As the hub: take `member`'s report of its alerts, and answer with
    /// the alert numbers it keeps for it.
    pub(crate) async fn record_alert_report(
        &self,
        member: &MemberId,
        mut report: AlertsReport,
    ) -> Result<AlertNumbering> {
        let member = member.clone();
        let users = report.users.take();
        let (numbering, changed) = (self.store)
            .write(move |tx| {
                let changed = match users {
                    Some(users) => copies::record(tx, &member, users)?,
                    None => None,
                };
                let mut numbering = sequence::number(tx, &member, &report)?;
                numbering.marked = copies::pending(tx, &member)?;
                Ok((numbering, changed))
            })
            .await?;
        if let Some(changed) = changed {
            self.alert_changes.send(changed);
        }
        Ok(numbering)
    }

    /// As the hub: tell every machine following the alerts that `member`'s
    /// went, once the hub revoked it.
    pub(crate) fn alerts_dropped(&self, member: &MemberId) {
        let machine = member.clone();
        self.alert_changes.send(AlertCopies::Dropped { machine });
    }

    /// Number this machine's alerts as `numbering`, the hub's answer to
    /// `reported`, says, telling the agent of the space that sent each
    /// unread alert it renumbers, and read or delete those to the user
    /// other machines read or cleared.
    async fn take_alert_numbering(
        &self,
        daemon: &dyn Daemon,
        reported: AlertsReport,
        mut numbering: AlertNumbering,
    ) -> Result<()> {
        let marked = std::mem::take(&mut numbering.marked);
        self.notifications.take_marks(marked).await?;
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
