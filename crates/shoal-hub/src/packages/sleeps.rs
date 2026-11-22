//! The hub's record of its machines' sleep. A machine going to sleep
//! announces it with a notice naming each space whose package the hub
//! keeps lacks changes, with since when, before it sends any package; a
//! machine whose link drops while its last report said its battery was
//! nearly empty counts as one that announced its sleep at that report,
//! with what that report said its packages lacked. A package its machine
//! sends after the notice makes its space's current. The hub forgets the
//! sleep once the machine's link opens again, and meanwhile takes the
//! machine's spaces with work over once it has stayed away long enough, in
//! [`taking_over`]. The hub keeps a sleep in its store, so a restart of
//! its own daemon meanwhile changes nothing.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::{
    availability::Availability,
    directory::Directory,
    sleep::{Lagging, Power},
};
use tokio::sync::Notify;

use crate::{Hub, daemon::Daemon, log, time::UnixTime};

/// What the hub keeps in memory of its machines' sleep.
#[derive(Default)]
pub(crate) struct Sleeps {
    /// How each machine with a battery was powered as it last reported,
    /// and when.
    powers: Mutex<HashMap<MemberId, (Power, UnixTime)>>,
    /// Wakes the look for spaces to take over, as once the hub holds an
    /// update for a machine that may sleep.
    look: Notify,
    /// The spaces of each sleeping machine whose takeover the hub tried,
    /// which it does not try again before that machine's link opens.
    tried: Mutex<HashMap<MemberId, HashSet<SpaceId>>>,
}

impl Sleeps {
    /// Keep how `member` is powered as it reports now, none without a
    /// battery.
    pub(crate) fn powered(&self, member: &MemberId, power: Option<Power>) {
        let mut powers = self.powers.lock().unwrap();
        match power {
            Some(power) => drop(powers.insert(member.clone(), (power, UnixTime::now()))),
            None => drop(powers.remove(member)),
        }
    }

    /// When `member` last reported, while that report said its battery was
    /// nearly empty.
    fn ran_out_at(&self, member: &MemberId) -> Option<UnixTime> {
        let powers = self.powers.lock().unwrap();
        (powers.get(member))
            .filter(|(power, _)| power.nearly_empty())
            .map(|(_, at)| *at)
    }

    /// Look for spaces to take over now.
    pub(crate) fn look(&self) {
        self.look.notify_one();
    }
}

impl Hub {
    /// As the hub: take `member`'s notice that it goes to sleep now, with
    /// the spaces whose packages lack changes, in place of any sleep it
    /// recorded of it before.
    pub(crate) async fn hear_sleep(&self, member: &MemberId, lagging: Vec<Lagging>) -> Result<()> {
        let member = member.clone();
        let now = UnixTime::now();
        (self.store)
            .write(move |tx| record(tx, &member, now, &lagging))
            .await?;
        self.sleeps.look();
        Ok(())
    }

    /// As the hub: forget `member`'s sleep, once its link opened.
    pub(crate) async fn hear_awake(&self, member: &MemberId) {
        self.sleeps.tried.lock().unwrap().remove(member);
        let id = member.clone();
        if let Err(error) = self.store.write(move |tx| forget(tx, &id)).await {
            log!("hub: could not forget the sleep of machine {member}: {error:#}");
        }
    }

    /// As the hub: count the drop of `member`'s link as a sleep it
    /// announced at its last report, when that report said its battery
    /// was nearly empty, unless it announced one already.
    pub(crate) async fn hear_drop(&self, daemon: &dyn Daemon, member: &MemberId) {
        let Some(at) = self.sleeps.ran_out_at(member) else {
            return;
        };
        let recorded = async {
            let directory = self.local_directory(daemon).await?;
            let lagging = reported_lags(&directory, member);
            let member = member.clone();
            (self.store)
                .write(move |tx| record_unless_asleep(tx, &member, at, &lagging))
                .await
        };
        match recorded.await {
            Ok(()) => self.sleeps.look(),
            Err(error) => {
                log!("hub: could not record that machine {member} ran out of battery: {error:#}")
            }
        }
    }
}

/// What the packages of `member`'s spaces lacked as `directory` lists
/// them: since when each space of an intermittent machine had changes its
/// package lacks, and for any other machine, which keeps no packages
/// current, an unknown lag for every space.
fn reported_lags(directory: &Directory, member: &MemberId) -> Vec<Lagging> {
    let intermittent = (directory.machines.iter())
        .find(|machine| &machine.id == member)
        .and_then(|machine| machine.availability)
        .is_some_and(|told| told.availability == Availability::Intermittent);
    (directory.spaces.iter())
        .filter(|listed| &listed.machine == member)
        .filter_map(|listed| {
            let since = listed.space.unsent_since;
            (!intermittent || since.is_some()).then(|| Lagging {
                space: listed.space.id.clone(),
                since: since.filter(|_| intermittent),
            })
        })
        .collect()
}

/// Record that `member` went to sleep at `at`, its packages lacking what
/// `lagging` says, in place of any sleep recorded of it.
fn record(
    tx: &Transaction<'_>,
    member: &MemberId,
    at: UnixTime,
    lagging: &[Lagging],
) -> Result<()> {
    tx.execute(
        "UPDATE hub_members SET asleep_at=?2 WHERE id=?1",
        params![member, at],
    )?;
    tx.execute("DELETE FROM hub_sleep_lags WHERE member_id=?1", [member])?;
    let mut insert = tx.prepare(
        "INSERT INTO hub_sleep_lags(space_id, member_id, since) VALUES (?1,?2,?3)
         ON CONFLICT(space_id) DO UPDATE SET member_id=excluded.member_id, since=excluded.since",
    )?;
    for lag in lagging {
        insert.execute(params![lag.space, member, lag.since])?;
    }
    Ok(())
}

/// Record the sleep [`record`] records, unless one of `member` is
/// recorded already.
fn record_unless_asleep(
    tx: &Transaction<'_>,
    member: &MemberId,
    at: UnixTime,
    lagging: &[Lagging],
) -> Result<()> {
    let asleep: Option<UnixTime> = tx.query_row(
        "SELECT asleep_at FROM hub_members WHERE id=?1",
        [member],
        |row| row.get(0),
    )?;
    match asleep {
        Some(_) => Ok(()),
        None => record(tx, member, at, lagging),
    }
}

/// Forget `member`'s sleep, as once its link opened.
fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute(
        "UPDATE hub_members SET asleep_at=NULL WHERE id=?1",
        [member],
    )?;
    tx.execute("DELETE FROM hub_sleep_lags WHERE member_id=?1", [member])?;
    Ok(())
}

/// Count the package of the space `id` that `member` sent current, once it
/// began to arrive at `arrived_at` no earlier than the notice of its sleep,
/// both by the hub's clock, so a machine whose clock differs still catches
/// up: one it sends for its sleep it builds after the notice.
pub(super) fn caught_up(
    tx: &Transaction<'_>,
    member: &MemberId,
    id: &SpaceId,
    arrived_at: UnixTime,
) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_sleep_lags WHERE space_id=?1 AND member_id=?2
            AND ?3 >= (SELECT asleep_at FROM hub_members WHERE id=?2)",
        params![id, member, arrived_at],
    )?;
    Ok(())
}

mod taking_over;
#[cfg(test)]
mod tests;
