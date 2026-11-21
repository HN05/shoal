//! The hub's alert once an intermittent machine has stayed unreachable for
//! [`UNREACHABLE_FOR`] without announcing a sleep or shutdown, while its
//! spaces had something to do: an agent working when its link dropped, a
//! queued agent, or an update the hub holds for one since. It names each
//! such space, how far behind the hub's package of it is, and the restore
//! that takes it over, and starts nothing, since a machine that lost only
//! the hub keeps working.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_daemon_base::{alerts::About, log::elapsed};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        Member,
        availability::Availability,
        directory::{Directory, Listed},
        link::Call,
    },
    list_status::ListStatus,
    state::AgentState,
};

use crate::{Hub, daemon::Daemon, log, queue, time::UnixTime};

/// How long a machine stays unreachable before the hub alerts the user.
const UNREACHABLE_FOR: Duration = Duration::from_secs(30 * 60);

/// How often the hub looks for machines that dropped.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// What the hub holds for a dropped machine since its link dropped: the
/// spaces the events it holds are for, and how many other requests wait.
#[derive(Debug, Default, PartialEq)]
struct Held {
    spaces: HashSet<SpaceId>,
    others: usize,
}

impl Hub {
    /// Alert the user to each intermittent machine that dropped without a
    /// word while its spaces had something to do, once per drop, while this
    /// daemon is the hub, until it stops.
    pub(crate) async fn keep_alerting_drops(&self, daemon: &dyn Daemon) {
        let mut every = tokio::time::interval(LOOK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if !self.is_hub(daemon) {
                continue;
            }
            if let Err(error) = self.alert_drops(daemon, UnixTime::now()).await {
                log!("hub: could not look for machines that dropped: {error:#}");
            }
        }
    }

    /// Alert the user to each machine that dropped by `now` and was not
    /// alerted for that drop yet.
    async fn alert_drops(&self, daemon: &dyn Daemon, now: UnixTime) -> Result<()> {
        let directory = self.local_directory(daemon).await?;
        for (machine, dropped_at) in dropped(&directory, now) {
            if !self.take_drop(&machine.id, dropped_at).await? {
                continue;
            }
            let id = machine.id.clone();
            let held = self.store.read(move |tx| held(tx, &id, dropped_at)).await?;
            let busy = busy_spaces(&directory, &machine.id, &held.spaces);
            if busy.is_empty() && held.others == 0 {
                continue;
            }
            let id = machine.id.clone();
            let kept = self.store.read(move |tx| packaged(tx, &id)).await?;
            let away = now.duration_since(dropped_at);
            let message = drop_message(machine, (dropped_at, away), (&busy, &kept), held.others);
            (self.notify(
                daemon,
                About::named(None),
                AlertKind::MachineUnreachable,
                message,
            ))
            .await;
        }
        Ok(())
    }

    /// Whether the drop of `member` at `dropped_at` is not alerted yet,
    /// which it is from now on.
    async fn take_drop(&self, member: &MemberId, dropped_at: UnixTime) -> Result<bool> {
        let member = member.clone();
        let taken = self.store.write(move |tx| {
            let changed = tx.execute(
                "UPDATE hub_members SET drop_alerted=?2
                 WHERE id=?1 AND (drop_alerted IS NULL OR drop_alerted<>?2)",
                params![member, dropped_at],
            )?;
            Ok(changed > 0)
        });
        taken.await
    }
}

/// The intermittent machines of `directory` that have been unreachable for
/// [`UNREACHABLE_FOR`] at `now`, each with when the hub last saw it.
fn dropped(directory: &Directory, now: UnixTime) -> Vec<(&Member, UnixTime)> {
    (directory.machines.iter())
        .filter(|machine| !machine.hub && !machine.connected)
        .filter(|machine| {
            let availability = machine.availability.map(|told| told.availability);
            availability == Some(Availability::Intermittent)
        })
        .filter_map(|machine| Some((machine, machine.last_seen?)))
        .filter(|(_, seen)| now.duration_since(*seen) >= UNREACHABLE_FOR)
        .collect()
}

/// What the hub holds for `member` since `since`.
fn held(tx: &Transaction<'_>, member: &MemberId, since: UnixTime) -> Result<Held> {
    let mut held = Held::default();
    for call in queue::held_since(tx, member, since)? {
        match call {
            Call::Tell { told } => drop(held.spaces.insert(told.above)),
            _ => held.others += 1,
        }
    }
    Ok(held)
}

/// When `member` built the package the hub keeps of each of its spaces.
fn packaged(tx: &Transaction<'_>, member: &MemberId) -> Result<HashMap<SpaceId, UnixTime>> {
    let mut statement =
        tx.prepare("SELECT space_id, built_at FROM hub_packages WHERE member_id=?1")?;
    let rows = statement.query_map([member], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The spaces of `machine` in `directory` that had something to do: an
/// agent working as its machine last reported, a queued agent, or an event
/// among those `held` for it.
fn busy_spaces<'a>(
    directory: &'a Directory,
    machine: &MemberId,
    held: &HashSet<SpaceId>,
) -> Vec<&'a Listed> {
    (directory.spaces.iter())
        .filter(|listed| &listed.machine == machine)
        .filter(|listed| {
            let space = &listed.space;
            space.agent_state == Some(AgentState::Working)
                || space.status == ListStatus::Queued
                || held.contains(&space.id)
        })
        .collect()
}

/// The alert that `machine`, last seen at `dropped_at` and unreachable
/// for `away` since, dropped while the `busy` spaces had something to do,
/// of which the hub `kept` packages, with `others` requests waiting for it
/// besides.
fn drop_message(
    machine: &Member,
    (dropped_at, away): (UnixTime, Duration),
    (busy, kept): (&[&Listed], &HashMap<SpaceId, UnixTime>),
    others: usize,
) -> String {
    let mut message = format!(
        "Machine {} has been unreachable for {} without announcing a sleep or shutdown; the hub starts nothing in its place, since it may still be working without the hub.",
        machine.name,
        elapsed(away),
    );
    for listed in busy {
        let space = &listed.space;
        let package = match (kept.contains_key(&space.id), space.unsent_since) {
            (false, _) => "the hub keeps no package of it".to_owned(),
            (true, None) => format!(
                "its package is current; shoal restore {} takes it over",
                space.number
            ),
            (true, Some(since)) => format!(
                "its package lacks the last {} of changes; shoal restore {} takes it over",
                elapsed(dropped_at.duration_since(since)),
                space.number
            ),
        };
        message.push_str(&format!(" {} ({}): {package}.", space.name, space.number));
    }
    if others > 0 {
        message.push_str(&format!(" {others} other requests wait for it at the hub."));
    }
    message
}

#[cfg(test)]
mod tests;
