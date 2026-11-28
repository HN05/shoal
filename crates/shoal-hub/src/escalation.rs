//! Passing a thread or approval request past a coordinator on a machine
//! that is offline or lost its agent.
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::Result;
use rusqlite::{Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    alerts::AlertKind,
    approvals::DecisionStatus,
    hub::{
        directory::Directory,
        routing::{Told, ToldAbout, ToldRecord},
    },
    threads::Thread,
};

use crate::{Hub, daemon::Daemon, log, time::UnixTime};
use shoal_daemon_base::alerts::{About, NewAlert};

/// How often a machine looks for what waits for an offline coordinator,
/// besides once its stored state settles after a change.
const SWEEP: Duration = Duration::from_secs(60);

/// Lets a burst of changes settle into one look.
const SETTLE: Duration = Duration::from_secs(1);

impl Hub {
    /// Pass on what waits too long for an offline coordinator, every
    /// [`SWEEP`] and after each change, until the daemon stops.
    pub(super) async fn escalate_waiting(&self, daemon: &dyn Daemon) {
        let mut stored = self.store.watch_changes();
        let mut sweep = tokio::time::interval(SWEEP);
        loop {
            tokio::select! {
                _ = sweep.tick() => {}
                changed = stored.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    tokio::time::sleep(SETTLE).await;
                    stored.borrow_and_update();
                }
            }
            if let Err(error) = self.escalate_once(daemon).await {
                log!("hub: could not pass on what waits for an offline coordinator: {error:#}");
            }
        }
    }

    async fn escalate_once(&self, daemon: &dyn Daemon) -> Result<()> {
        let Some(settings) = self.hub_settings(daemon) else {
            return Ok(());
        };
        let after = Duration::from_secs(settings.escalate_after_minutes * 60);
        // Not in a hub, or cut off from it, a machine cannot tell who is
        // offline.
        let Ok(Some((own, directory))) = self.hub_directory(daemon).await else {
            return Ok(());
        };
        let now = UnixTime::now();
        let deciders = Deciders {
            own: &own,
            directory: &directory,
            now,
            after,
        };
        for request in daemon.approval_requests(None).await? {
            let Some(decider) = request.decider.as_ref() else {
                continue;
            };
            // This machine's own rules cover a decider here, which the hub
            // may not list yet.
            if request.status != DecisionStatus::Pending
                || daemon.find_space(decider).await?.is_some()
            {
                continue;
            }
            match deciders.unavailable(decider, request.created_at) {
                Some(Unavailable::Offline) => daemon.pass_over_offline(&request).await?,
                Some(Unavailable::Gone) => daemon.pass_to_user(&request).await?,
                None => {}
            }
        }
        let offline = offline_since(&directory);
        let waited = |created: UnixTime, decider: &SpaceId| {
            (offline.get(decider))
                .is_some_and(|since| now.duration_since(created.max(*since)) >= after)
        };
        let waiting: Vec<Waiting> = (daemon.open_threads().await?.into_iter())
            .filter_map(Waiting::of)
            .collect();
        let kept = waiting.clone();
        self.store
            .write(move |tx| forget_answered(tx, &kept))
            .await?;
        for waiting in waiting {
            let Some(holder) = waiting.thread.holder.space() else {
                continue;
            };
            if waited(waiting.since, &holder.id) && !self.escalated(&waiting).await? {
                self.pass_thread_on(daemon, &waiting).await?;
            }
        }
        Ok(())
    }

    /// Tell the next space above `waiting`'s space with an agent on a
    /// connected machine, or else the user, of its latest post, once.
    async fn pass_thread_on(&self, daemon: &dyn Daemon, waiting: &Waiting) -> Result<()> {
        let thread = &waiting.thread;
        let online = self
            .deciders_elsewhere(daemon, &thread.space_id, true)
            .await;
        match online.into_iter().next() {
            Some(above) => {
                let told = Told {
                    above,
                    about: Some(ToldAbout {
                        space: thread.space_id.clone(),
                        name: thread.space.clone(),
                    }),
                    for_user: true,
                    record: ToldRecord::Alert {
                        kind: AlertKind::ThreadPosted,
                        message: thread.notice(),
                        files: Vec::new(),
                    },
                };
                self.send_told(daemon, told).await?;
            }
            None => {
                let kind = AlertKind::ThreadPosted;
                let about = About::space(&thread.space, &thread.space_id);
                let alert = NewAlert::shoal(kind, about, thread.notice());
                self.notifications.notify_user(alert).await;
            }
        }
        let waiting = waiting.clone();
        self.store
            .write(move |tx| record_escalated(tx, &waiting))
            .await
    }

    /// Whether `waiting` went on past its offline holder already.
    async fn escalated(&self, waiting: &Waiting) -> Result<bool> {
        let waiting = waiting.clone();
        self.store
            .read(move |tx| {
                shoal_store::exists(
                    tx,
                    "SELECT 1 FROM hub_escalated_threads
                        WHERE space_id=?1 AND number=?2 AND created_at=?3",
                    params![
                        waiting.thread.space_id,
                        waiting.thread.number.item,
                        waiting.since
                    ],
                )
            })
            .await
    }
}

/// A thread that waits for its holder, since its opener's latest post.
#[derive(Debug, Clone)]
struct Waiting {
    thread: Thread,
    since: UnixTime,
}

impl Waiting {
    fn of(thread: Thread) -> Option<Self> {
        let since = thread.last_turn()?.created_at;
        thread.is_waiting().then_some(Self { thread, since })
    }

    /// The escalation that passes the thread on for its latest post.
    fn escalation(&self) -> Escalation {
        (
            self.thread.space_id.clone(),
            self.thread.number.item,
            self.since,
        )
    }
}

/// What keeps a request's decider on another machine from deciding it.
#[derive(Debug, PartialEq, Eq)]
enum Unavailable {
    /// Its machine has been offline for too long; the next space above with
    /// an agent on a connected machine decides it.
    Offline,
    /// It no longer has an agent, or is gone, which gives its undecided
    /// requests to the user, as a chat's agent stopping here does.
    Gone,
}

/// The hub's directory as this machine, `own`, sees deciders in it.
struct Deciders<'a> {
    own: &'a shoal_ids::member_ids::MemberId,
    directory: &'a Directory,
    now: UnixTime,
    after: Duration,
}

impl Deciders<'_> {
    /// What keeps `decider`, a space elsewhere that a request made at
    /// `created` waits for, from deciding it; none for one that can.
    fn unavailable(&self, decider: &SpaceId, created: UnixTime) -> Option<Unavailable> {
        let listed = (self.directory.spaces.iter()).find(|listed| &listed.space.id == decider);
        let Some(listed) = listed else {
            return Some(Unavailable::Gone);
        };
        if &listed.machine == self.own {
            return None;
        }
        if !listed.space.agent {
            return Some(Unavailable::Gone);
        }
        let machine = self.directory.machine(&listed.machine)?;
        if machine.connected {
            return None;
        }
        let since = machine.last_seen.unwrap_or(machine.joined_at);
        (self.now.duration_since(created.max(since)) >= self.after).then_some(Unavailable::Offline)
    }
}

/// When the hub last saw the machine of each space on an offline machine.
fn offline_since(directory: &Directory) -> HashMap<SpaceId, UnixTime> {
    (directory.spaces.iter())
        .filter_map(|listed| {
            let machine = directory.machine(&listed.machine)?;
            let since = machine.last_seen.unwrap_or(machine.joined_at);
            (!machine.connected).then(|| (listed.space.id.clone(), since))
        })
        .collect()
}

fn record_escalated(tx: &Transaction<'_>, waiting: &Waiting) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO hub_escalated_threads (space_id, number, created_at)
            VALUES (?1, ?2, ?3)",
        params![
            waiting.thread.space_id,
            waiting.thread.number.item,
            waiting.since
        ],
    )?;
    Ok(())
}

/// Forget the escalations of the threads that no longer wait since the
/// post they were passed on for.
fn forget_answered(tx: &Transaction<'_>, waiting: &[Waiting]) -> Result<()> {
    let waiting: HashSet<_> = waiting.iter().map(Waiting::escalation).collect();
    for answered in escalations(tx)?.difference(&waiting) {
        forget_escalation(tx, answered)?;
    }
    Ok(())
}

fn forget_escalation(
    tx: &Transaction<'_>,
    (space_id, number, created_at): &Escalation,
) -> Result<()> {
    tx.execute(
        "DELETE FROM hub_escalated_threads
            WHERE space_id=?1 AND number=?2 AND created_at=?3",
        params![space_id, number, created_at],
    )?;
    Ok(())
}

/// A thread's escalation: its space, its number in the space and when the
/// post it was passed on for was made.
type Escalation = (SpaceId, u32, UnixTime);

/// Every recorded escalation.
fn escalations(tx: &Transaction<'_>) -> Result<HashSet<Escalation>> {
    let mut statement =
        tx.prepare("SELECT space_id, number, created_at FROM hub_escalated_threads")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
