//! Taking a sleeping machine's spaces with work over once it has stayed
//! away.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_daemon_base::{alerts::About, log::elapsed};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    alerts::AlertKind,
    hub::{
        directory::{Directory, Listed},
        packages::Departure,
    },
};
use shoal_schedule::placement::snapshot::Request;

use super::{
    super::{Continuation, drops},
    Sleeps,
};
use crate::{Hub, daemon::Daemon, log, queue, time::UnixTime};

/// How long a sleeping machine stays away before the hub takes its spaces
/// over.
const AWAY_FOR: Duration = Duration::from_secs(10 * 60);

/// How much older than the notice a space's oldest change its package
/// lacks may be for the hub to take the space over from that package.
const LAG_LIMIT: Duration = Duration::from_secs(15 * 60);

/// Why the spaces this takes over leave their machine.
const SLEPT: Departure = Departure::Slept;

/// How often the hub looks for spaces to take over, besides when it holds
/// a new update.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// Spaces that go to one machine together: a space, then the chats in its
/// worktree, placed by the first one's start.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Group {
    pub start: Request,
    pub spaces: Vec<SpaceId>,
}

/// A sleeping machine: since when, and since when the packages of its
/// spaces lack changes, none for an unknown lag, of those that lack any.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Sleeper {
    pub member: MemberId,
    pub asleep_at: UnixTime,
    pub lags: HashMap<SpaceId, Option<UnixTime>>,
}

fn sleepers(tx: &Transaction<'_>) -> Result<Vec<Sleeper>> {
    let mut members =
        tx.prepare("SELECT id, asleep_at FROM hub_members WHERE asleep_at IS NOT NULL")?;
    let asleep = members.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    let mut lags = tx.prepare("SELECT space_id, since FROM hub_sleep_lags WHERE member_id=?1")?;
    let mut sleepers = Vec::new();
    for asleep in asleep {
        let (member, asleep_at): (MemberId, UnixTime) = asleep?;
        let rows = lags.query_map([&member], |row| Ok((row.get(0)?, row.get(1)?)))?;
        sleepers.push(Sleeper {
            lags: rows.collect::<rusqlite::Result<_>>()?,
            member,
            asleep_at,
        });
    }
    Ok(sleepers)
}

impl Sleeps {
    /// Record that the hub tried to take the space `id` of `member` over,
    /// false when it already did during this sleep.
    fn try_once(&self, member: &MemberId, id: &SpaceId) -> bool {
        let mut tried = self.tried.lock().unwrap();
        tried.entry(member.clone()).or_default().insert(id.clone())
    }
}

impl Hub {
    /// Take sleeping machines' spaces over as they become due, every
    /// [`LOOK_EVERY`] and whenever the hub holds a new update, while this
    /// daemon is the hub, until it stops.
    pub(crate) async fn keep_taking_over_sleepers(self: &Arc<Self>, daemon: &Arc<dyn Daemon>) {
        let mut every = tokio::time::interval(LOOK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = every.tick() => {}
                () = self.sleeps.look.notified() => {}
            }
            if !self.is_hub(&**daemon) {
                continue;
            }
            if let Err(error) = self.take_over_sleepers(daemon, UnixTime::now()).await {
                log!("hub: could not look for sleeping machines' spaces: {error:#}");
            }
        }
    }

    /// Start taking over, in the background, each space of a machine that
    /// has slept [`AWAY_FOR`] by `now` that is due and not tried yet.
    async fn take_over_sleepers(
        self: &Arc<Self>,
        daemon: &Arc<dyn Daemon>,
        now: UnixTime,
    ) -> Result<()> {
        let connected = self.links.connected();
        let sleepers: Vec<Sleeper> = (self.store.read(sleepers).await?.into_iter())
            .filter(|sleeper| !connected.contains(&sleeper.member))
            .filter(|sleeper| now.duration_since(sleeper.asleep_at) >= AWAY_FOR)
            .collect();
        if sleepers.is_empty() {
            return Ok(());
        }
        let directory = self.local_directory(&**daemon).await?;
        for sleeper in sleepers {
            let (member, since) = (sleeper.member.clone(), sleeper.asleep_at);
            let (kept, held) = (self.store)
                .read(move |tx| {
                    Ok((
                        kept_starts(tx, &member)?,
                        drops::held(tx, &member, since)?.spaces,
                    ))
                })
                .await?;
            for group in movable(&directory, &sleeper, &kept, &held) {
                let fresh =
                    (group.spaces.iter()).all(|id| self.sleeps.try_once(&sleeper.member, id));
                if fresh {
                    let labels = group
                        .spaces
                        .iter()
                        .map(|id| label(&directory, id))
                        .collect();
                    let (hub, daemon, from) =
                        (self.clone(), daemon.clone(), sleeper.member.clone());
                    let asleep_at = sleeper.asleep_at;
                    tokio::spawn(async move {
                        let from = (&from, asleep_at);
                        hub.take_over_group(&*daemon, from, (group, labels)).await
                    });
                }
            }
        }
        Ok(())
    }

    /// Move `group`, spaces of the machine `from`, asleep since
    /// `asleep_at`, that the user knows by `labels`, to the machine
    /// placement chooses, one after another, telling the user where they
    /// went; what does not go stays on `from`.
    async fn take_over_group(
        &self,
        daemon: &dyn Daemon,
        (from, asleep_at): (&MemberId, UnixTime),
        (group, labels): (Group, Vec<String>),
    ) {
        let decision = match self.place_elsewhere(daemon, from, group.start).await {
            Ok(Some(decision)) => decision,
            Ok(None) => {
                return log!(
                    "hub: no other machine can take space {} of sleeping machine {from}",
                    group.spaces[0]
                );
            }
            Err(error) => {
                return log!("hub: could not place space {}: {error:#}", group.spaces[0]);
            }
        };
        let mut went = Vec::new();
        for (id, label) in group.spaces.iter().zip(&labels) {
            let continued = self.ask_to_continue(daemon, id, (from, &decision.machine), SLEPT);
            if let Err(error) = continued.await {
                log!("hub: space {id} of sleeping machine {from} did not continue: {error:#}");
                let failed = (&decision, SLEPT);
                let message = (self.failed_message(daemon, id, from, failed, &error)).await;
                (self.notify(
                    daemon,
                    About::named(None),
                    AlertKind::HandoverFailed,
                    message,
                ))
                .await;
                break;
            }
            self.redirect_held(daemon, id, (from, &decision.machine))
                .await;
            went.push(label.as_str());
        }
        if !went.is_empty() {
            let slept = UnixTime::now().duration_since(asleep_at);
            let message = self
                .taken_over_message(daemon, (from, slept), &went, &decision.name)
                .await;
            (self.notify(
                daemon,
                About::named(None),
                AlertKind::SpacesTakenOver,
                message,
            ))
            .await;
        }
    }

    /// Hold for `to` what the hub held for the space `id` on `from`, which
    /// `to` took over, and deliver it there, here for the hub's own machine,
    /// which has no link to deliver it over.
    async fn redirect_held(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        (from, to): (&MemberId, &MemberId),
    ) {
        let (space, from_id, to_id) = (id.clone(), from.clone(), to.clone());
        let moved = self
            .store
            .write(move |tx| queue::redirect(tx, &space, &from_id, &to_id));
        if let Err(error) = moved.await {
            return log!("hub: could not pass on what it held for space {id}: {error:#}");
        }
        match self.own_member().await {
            Ok(own) if &own == to => self.deliver_held_here(daemon).await,
            _ => self.wake_delivery(to),
        }
    }

    /// What the user reads of the spaces `went`, which the hub took over
    /// from the machine `from`, asleep for `slept`, to the machine named
    /// `to`.
    async fn taken_over_message(
        &self,
        daemon: &dyn Daemon,
        (from, slept): (&MemberId, Duration),
        went: &[&str],
        to: &str,
    ) -> String {
        let from = self.member_name(daemon, from).await;
        format!(
            "Machine {from} has slept for {} while its spaces had work to do, so {} continue on machine {to}; {from} removes its copies once it wakes",
            elapsed(slept),
            went.join(", "),
        )
    }
}

/// How the user knows the space `id`: by its name and number as
/// `directory` lists it, else its ID.
fn label(directory: &Directory, id: &SpaceId) -> String {
    (directory.spaces.iter())
        .find(|listed| &listed.space.id == id)
        .map_or_else(
            || id.to_string(),
            |listed| format!("{} ({})", listed.space.name, listed.space.number),
        )
}

/// The groups of `sleeper`'s spaces in `directory` that go to another
/// machine: those with something to do, by its last report or an update
/// `held` for one, whose packages, as `kept`, are each current enough and
/// whose first space has a start.
pub(super) fn movable(
    directory: &Directory,
    sleeper: &Sleeper,
    kept: &HashMap<SpaceId, Continuation>,
    held: &HashSet<SpaceId>,
) -> Vec<Group> {
    let listed: HashMap<&SpaceId, &Listed> = (directory.spaces.iter())
        .filter(|listed| listed.machine == sleeper.member)
        .map(|listed| (&listed.space.id, listed))
        .collect();
    let mut within: HashMap<&SpaceId, Vec<&SpaceId>> = HashMap::new();
    for (id, kept) in kept {
        if let Some(workspace) = &kept.within
            && listed.contains_key(id)
        {
            within.entry(workspace).or_default().push(id);
        }
    }
    let mut groups = Vec::new();
    for (id, kept_start) in kept {
        if kept_start.within.is_some() || !listed.contains_key(id) {
            continue;
        }
        let mut spaces = vec![id.clone()];
        let mut chats: Vec<SpaceId> = (within.get(id).into_iter().flatten())
            .map(|chat| (*chat).clone())
            .collect();
        chats.sort();
        spaces.extend(chats);
        let busy = (spaces.iter()).any(|space| drops::had_work(listed[space], held));
        let current = (spaces.iter()).all(|space| current_enough(sleeper, space));
        if let (true, true, Some(start)) = (busy, current, kept_start.start.clone()) {
            groups.push(Group { start, spaces });
        }
    }
    groups.sort_by(|one, other| one.spaces.cmp(&other.spaces));
    groups
}

/// Whether the package of the space `id` of `sleeper` is current, or
/// lacked only changes from less than [`LAG_LIMIT`] before the notice.
fn current_enough(sleeper: &Sleeper, id: &SpaceId) -> bool {
    match sleeper.lags.get(id) {
        None => true,
        Some(None) => false,
        Some(Some(since)) => sleeper.asleep_at.duration_since(*since) < LAG_LIMIT,
    }
}

/// The start and workspace the hub keeps with each package `member` sent.
fn kept_starts(tx: &Transaction<'_>, member: &MemberId) -> Result<HashMap<SpaceId, Continuation>> {
    let mut statement =
        tx.prepare("SELECT space_id, start, within FROM hub_packages WHERE member_id=?1")?;
    let rows = statement.query_map(params![member], |row| {
        let start: Option<String> = row.get(1)?;
        Ok((row.get(0)?, start, row.get(2)?))
    })?;
    let mut kept = HashMap::new();
    for row in rows {
        let (id, start, within) = row?;
        let start = start.and_then(|start| serde_json::from_str(&start).ok());
        kept.insert(id, Continuation { start, within });
    }
    Ok(kept)
}

#[cfg(test)]
mod tests;
