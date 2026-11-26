//! The hub's alert once a machine whose spaces had something to do stays
//! unreachable.
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
pub(super) struct Held {
    pub spaces: HashSet<SpaceId>,
    pub others: usize,
}

impl Hub {
    /// Alert the user to each intermittent machine that dropped while its
    /// spaces had something to do, once per drop, while this daemon is the
    /// hub, until it stops.
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

    /// Alert the user to each machine that dropped by `now` while its
    /// spaces had something to do, and was not alerted for that drop yet.
    async fn alert_drops(&self, daemon: &dyn Daemon, now: UnixTime) -> Result<()> {
        let directory = self.local_directory(daemon).await?;
        let asleep = self.store.read(super::sleeps::asleep_members).await?;
        for (machine, dropped_at) in dropped(&directory, now, &asleep) {
            let id = machine.id.clone();
            let held = self.store.read(move |tx| held(tx, &id, dropped_at)).await?;
            let busy = busy_spaces(&directory, &machine.id, &held.spaces);
            // An idle machine's drop alerts once something waits for it.
            if busy.is_empty() && held.others == 0 {
                continue;
            }
            if !self.take_drop(&machine.id, dropped_at).await? {
                continue;
            }
            let id = machine.id.clone();
            let kept = self.store.read(move |tx| packaged(tx, &id)).await?;
            let away = now.duration_since(dropped_at);
            let slept = asleep.contains(&machine.id);
            let away = (dropped_at, away, slept);
            let message = drop_message(machine, away, (&busy, &kept), held.others);
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

/// The machines of `directory` that have been unreachable for
/// [`UNREACHABLE_FOR`] at `now`, each with when the hub last saw it: the
/// intermittent ones, and any that announced its sleep, as `asleep` lists,
/// whose spaces the hub could not take over.
fn dropped<'a>(
    directory: &'a Directory,
    now: UnixTime,
    asleep: &HashSet<MemberId>,
) -> Vec<(&'a Member, UnixTime)> {
    (directory.machines.iter())
        .filter(|machine| !machine.hub && !machine.connected)
        .filter(|machine| {
            let availability = machine.availability.map(|told| told.availability);
            availability == Some(Availability::Intermittent) || asleep.contains(&machine.id)
        })
        .filter_map(|machine| Some((machine, machine.last_seen?)))
        .filter(|(_, seen)| now.duration_since(*seen) >= UNREACHABLE_FOR)
        .collect()
}

/// What the hub holds for `member` since `since`.
pub(super) fn held(tx: &Transaction<'_>, member: &MemberId, since: UnixTime) -> Result<Held> {
    let mut held = Held::default();
    for call in queue::held_since(tx, member, since)? {
        match queue::held_for(&call) {
            Some(space) => drop(held.spaces.insert(space.clone())),
            None => held.others += 1,
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
        .filter(|listed| had_work(listed, held))
        .collect()
}

/// Whether `listed` had something to do: an agent working as its machine
/// last reported, a queued agent, or an event among those `held` for it.
pub(super) fn had_work(listed: &Listed, held: &HashSet<SpaceId>) -> bool {
    let space = &listed.space;
    space.agent_state == Some(AgentState::Working)
        || space.status == ListStatus::Queued
        || held.contains(&space.id)
}

/// The alert that `machine` dropped while its `busy` spaces had something
/// to do. It names when the machine was last seen and how long it has been
/// away, whether it `slept`, the packages the hub `kept`, and the `others`
/// requests that wait for it.
fn drop_message(
    machine: &Member,
    (dropped_at, away, slept): (UnixTime, Duration, bool),
    (busy, kept): (&[&Listed], &HashMap<SpaceId, UnixTime>),
    others: usize,
) -> String {
    let mut message = match slept {
        true => format!(
            "Machine {} has slept for {}, and the hub could not take these of its spaces over to another machine.",
            machine.name,
            elapsed(away),
        ),
        false => format!(
            "Machine {} has been unreachable for {}; the hub starts nothing in its place, since it may still be working without the hub.",
            machine.name,
            elapsed(away),
        ),
    };
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
