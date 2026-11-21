//! A returning machine retiring the spaces other machines took over from
//! the hub's packages of them while it was offline: once its link opens,
//! the hub names them, and the machine ends each here, keeping its work in
//! the removal record, and alerts the user to one whose copy changed after
//! the package the takeover went by. The hub then forgets the takeover and
//! the machine's package of the space.
use std::sync::Arc;

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_daemon_base::alerts::About;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::AlertKind,
    hub::link::{Call, Reply},
    model::Space,
};

use super::sending;
use crate::{Hub, answers::unexpected, daemon::Daemon, log, session::Calls};

impl Hub {
    /// As the hub: have `member`, whose link `calls` just opened, retire
    /// the spaces other machines took over from it, then forget those
    /// takeovers and its packages of the spaces it retired. One it did not
    /// retire waits for its next link.
    pub(crate) async fn retire_taken_over(self: Arc<Self>, member: MemberId, calls: Arc<Calls>) {
        if let Err(error) = self.have_retired(&member, &calls).await {
            log!("hub: could not have machine {member} retire the spaces taken over: {error:#}");
        }
    }

    /// Have `member` retire the spaces other machines took over from it,
    /// over `calls`, and forget what it retired.
    async fn have_retired(&self, member: &MemberId, calls: &Calls) -> Result<()> {
        let from = member.clone();
        let spaces = self.store.read(move |tx| taken_from(tx, &from)).await?;
        if spaces.is_empty() {
            return Ok(());
        }
        let retired = match calls.call(Call::Retire { spaces }).await? {
            Reply::Retired(retired) => retired,
            reply => return Err(unexpected(reply)),
        };
        let member = member.clone();
        (self.store)
            .write(move |tx| forget_retired(tx, &member, &retired))
            .await?;
        self.delete_ended_packages().await;
        Ok(())
    }

    /// As a returning machine: end each of `spaces`, which other machines
    /// took over from the hub's packages while this one was offline,
    /// alerting the user to each whose copy here changed after the package
    /// the takeover went by; the spaces it no longer has.
    pub(crate) async fn retire(&self, daemon: &dyn Daemon, spaces: Vec<SpaceId>) -> Vec<SpaceId> {
        let mut retired = Vec::new();
        for space in spaces {
            match self.retire_one(daemon, &space).await {
                Ok(()) => retired.push(space),
                Err(error) => log!(
                    "hub: could not retire space {space}, which another machine took over: {error:#}"
                ),
            }
        }
        retired
    }

    /// End the space `id` here, if this machine still has it, alerting the
    /// user when its copy changed after the package the hub kept.
    async fn retire_one(&self, daemon: &dyn Daemon, id: &SpaceId) -> Result<()> {
        let Some(space) = daemon.find_space(id).await? else {
            return Ok(());
        };
        let diverged = self.diverged(daemon, &space).await;
        daemon.retire_space(&space).await?;
        if diverged {
            let about = About::space(&space.name, &space.id);
            (self.notify(
                daemon,
                about,
                AlertKind::SpaceDiverged,
                diverged_message(&space),
            ))
            .await;
        }
        Ok(())
    }

    /// Whether `space` changed after the last package of it the hub kept,
    /// which a takeover went by, or cannot tell.
    async fn diverged(&self, daemon: &dyn Daemon, space: &Space) -> bool {
        let id = space.id.clone();
        let sent = self.store.read(move |tx| sending::sent_state(tx, &id));
        match (sent.await, daemon.package_state(space).await) {
            (Ok(Some(sent)), Ok(now)) => sent != now,
            _ => true,
        }
    }
}

/// What the user reads of `space`, which another machine took over and
/// which changed here after the package the takeover went by.
fn diverged_message(space: &Space) -> String {
    let (name, id) = (&space.name, &space.id);
    format!(
        "{name} ({id}) changed on this machine after the package another machine took it over from, so the two diverged; this machine removed its copy, keeping that work, and shoal start --copy {id} opens it as a copy"
    )
}

/// The spaces other machines took over from `member`.
fn taken_from(tx: &Transaction<'_>, member: &MemberId) -> Result<Vec<SpaceId>> {
    let mut statement = tx.prepare("SELECT space_id FROM hub_takeovers WHERE member_id=?1")?;
    let rows = statement.query_map([member], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Forget the takeovers of the spaces `member` `retired`, and its packages
/// of them.
fn forget_retired(tx: &Transaction<'_>, member: &MemberId, retired: &[SpaceId]) -> Result<()> {
    for space in retired {
        tx.execute(
            "DELETE FROM hub_takeovers WHERE space_id=?1 AND member_id=?2",
            params![space, member],
        )?;
        tx.execute(
            "DELETE FROM hub_packages WHERE space_id=?1 AND member_id=?2",
            params![space, member],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
