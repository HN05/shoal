//! A machine's side of packages: sending the hub a new package of each
//! space that changed while the machine is intermittent.
use std::{collections::HashMap, convert::Infallible, time::Duration};

use tokio::time::Instant;

use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{availability::Availability, link::Call},
    model::Space,
    state::{AgentState, WorkspaceState},
};

use crate::{Hub, daemon::Daemon, log, session::Calls, time::UnixTime};

/// How often a machine looks at every space, for what changed without a
/// turn of its agent ending.
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// The least time between two packages of one space.
const SEND_EVERY: Duration = Duration::from_secs(5 * 60);

/// What a machine recorded of the packages of one of its spaces.
#[derive(Debug, Default, PartialEq)]
struct Recorded {
    /// The state the last package that arrived holds.
    sent: Option<String>,
    /// When it last tried to send one.
    tried_at: Option<UnixTime>,
    /// Since when the space has changes the hub's package lacks.
    unsent_since: Option<UnixTime>,
}

/// What a look at a space found it needs.
#[derive(Debug, PartialEq)]
enum Look {
    /// Nothing: the hub's package holds what it has.
    Current,
    /// A new package, which holds `state`.
    Send(String),
    /// A new package once `wait` passed since it last tried to send one.
    Wait(Duration),
}

impl Hub {
    /// Keep the hub's packages of this machine's spaces current while the
    /// hub tells it it is intermittent, for as long as the link `calls`
    /// stays open: at once, then as turns end and every [`CHECK_EVERY`].
    pub(crate) async fn keep_packaging(&self, daemon: &dyn Daemon, calls: &Calls) -> Infallible {
        let mut agents = daemon.watch_agents();
        let mut every = tokio::time::interval(CHECK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut turns = HashMap::new();
        let mut due = None;
        loop {
            let held = async {
                match due {
                    Some(due) => tokio::time::sleep_until(due).await,
                    None => std::future::pending().await,
                }
            };
            let every_space = tokio::select! {
                _ = every.tick() => true,
                () = held => true,
                changed = agents.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                    false
                }
            };
            if !self.member.has_availability(Availability::Intermittent) {
                // Nothing is held back while no package is sent.
                due = None;
                continue;
            }
            let found = match self
                .spaces_to_package(daemon, every_space, &mut turns)
                .await
            {
                Ok(spaces) => self.send_changed_packages(daemon, calls, spaces).await,
                Err(error) => {
                    log!("hub: could not look at the spaces to send: {error:#}");
                    None
                }
            };
            let found = found.map(|wait| Instant::now() + wait);
            due = next_due(due, found, every_space);
        }
    }

    /// This machine's spaces a package can hold, every one when
    /// `every_space`, else those whose agent's turn ended since `turns`,
    /// which this records, last looked.
    async fn spaces_to_package(
        &self,
        daemon: &dyn Daemon,
        every_space: bool,
        turns: &mut HashMap<SpaceId, UnixTime>,
    ) -> Result<Vec<Space>> {
        let spaces = daemon.listed_spaces(None).await?;
        Ok((spaces.into_iter())
            .filter(packageable)
            .filter(|space| {
                let ended = (space.agent_state.as_ref())
                    .filter(|status| status.state == AgentState::Idle)
                    .map(|status| status.since);
                let new = ended.is_some_and(|ended| turns.get(&space.id) != Some(&ended));
                if let Some(ended) = ended {
                    turns.insert(space.id.clone(), ended);
                }
                every_space || new
            })
            .collect())
    }

    /// Look at each of `spaces` and send the hub a new package of each that
    /// changed, all at once over `calls`. Returns how long until the first
    /// package held back, since its space sent one lately, can go.
    async fn send_changed_packages(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        spaces: Vec<Space>,
    ) -> Option<Duration> {
        let now = UnixTime::now();
        let mut sending = Vec::new();
        let mut due: Option<Duration> = None;
        for space in spaces {
            match self.look_at_package(daemon, &space, now).await {
                Ok(Look::Current) => {}
                Ok(Look::Send(state)) => sending.push((space, state)),
                Ok(Look::Wait(wait)) => due = Some(due.map_or(wait, |due| due.min(wait))),
                Err(error) => log!(
                    "hub: could not tell whether {} changed since its package: {error:#}",
                    space.name
                ),
            }
        }
        let sends =
            (sending.iter()).map(|(space, state)| self.send_package(daemon, calls, space, state));
        futures_util::future::join_all(sends).await;
        due
    }

    /// What `space` needs at `now`, recording since when it has changes
    /// the hub's package lacks.
    async fn look_at_package(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        now: UnixTime,
    ) -> Result<Look> {
        let state = daemon.package_state(space).await?;
        self.record_look(&space.id, state, now).await
    }

    /// What the space `id`, which a package taken now would hold as
    /// `state`, needs at `now`, recording since when it has changes the
    /// hub's package lacks.
    async fn record_look(&self, id: &SpaceId, state: String, now: UnixTime) -> Result<Look> {
        let id = id.clone();
        let look = move |tx: &Transaction<'_>| {
            let recorded = load(tx, &id)?;
            let look = look(&recorded, state, now);
            let unsent_since = match look {
                Look::Current => None,
                _ => recorded.unsent_since.or(Some(now)),
            };
            if unsent_since != recorded.unsent_since {
                record_unsent_since(tx, &id, unsent_since)?;
            }
            Ok(look)
        };
        self.store.write(look).await
    }

    /// Send the hub a package of `space`, which holds `state`, over a
    /// stream of `calls`, recording that the hub has it once it kept it.
    async fn send_package(&self, daemon: &dyn Daemon, calls: &Calls, space: &Space, state: &str) {
        let id = space.id.clone();
        let tried = self.store.write(move |tx| record_tried(tx, &id));
        if let Err(error) = tried.await {
            return log!(
                "hub: could not record a package of {}: {error:#}",
                space.name
            );
        }
        if let Err(error) = self.deliver_package(daemon, calls, space).await {
            return log!(
                "hub: could not send the package of {}: {error:#}",
                space.name
            );
        }
        let (id, state) = (space.id.clone(), state.to_owned());
        let sent = self.store.write(move |tx| record_sent(tx, &id, &state));
        if let Err(error) = sent.await {
            log!(
                "hub: could not record the package of {}: {error:#}",
                space.name
            );
        }
    }

    /// Build a package of `space` and write it up a stream of `calls`,
    /// with the start its agent would make elsewhere, returning once the
    /// hub kept it.
    async fn deliver_package(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        space: &Space,
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        let built_at = UnixTime::now();
        let packaged = daemon.write_package(space, &file).await?;
        let start = match daemon.package_start(space, &packaged).await {
            Ok(start) => Some(start),
            Err(error) => {
                log!(
                    "hub: no machine can continue {} from its package: {error:#}",
                    space.name
                );
                None
            }
        };
        let keep = |stream| Call::Keep {
            space: space.id.clone(),
            built_at,
            stream,
            start: start.clone(),
            within: space.within().cloned(),
        };
        let (mut kept, mut package) = calls.duplex(keep).await?;
        package.write_file(&file).await?;
        package.end(None).await;
        // The hub ends its side once it kept the package, with why not.
        while kept.next().await?.is_some() {}
        Ok(())
    }
}

impl Hub {
    /// Send the hub a package of each of `spaces` whose state changed since the
    /// last one arrived, or of every space when `every`. Sends them all at once
    /// over `calls` and holds none back for being recent, as before this
    /// machine sleeps.
    pub(super) async fn send_current_packages(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        spaces: Vec<Space>,
        every: bool,
    ) {
        let sends = spaces.iter().map(|space| async move {
            let state = match daemon.package_state(space).await {
                Ok(state) => state,
                Err(error) => {
                    return log!(
                        "hub: could not tell whether {} changed since its package: {error:#}",
                        space.name
                    );
                }
            };
            let id = space.id.clone();
            let sent = self.store.read(move |tx| sent_state(tx, &id)).await;
            if every || sent.ok().flatten().as_ref() != Some(&state) {
                self.send_package(daemon, calls, space, &state).await;
            }
        });
        futures_util::future::join_all(sends).await;
    }

    /// Since when each space has changes the hub's package of it lacks,
    /// while the hub tells this machine it is intermittent, which it keeps
    /// them for; none on any other machine.
    pub(crate) async fn unsent_changes(&self) -> Result<HashMap<SpaceId, UnixTime>> {
        if !self.member.has_availability(Availability::Intermittent) {
            return Ok(HashMap::new());
        }
        self.store.read(unsent).await
    }
}

/// When to look at every space again for the packages held back as recent.
/// A pass over every space sets it to what it `found`. A pass over only the
/// spaces whose turns ended keeps the earlier of that and the time `due`
/// before.
fn next_due(due: Option<Instant>, found: Option<Instant>, every_space: bool) -> Option<Instant> {
    match (every_space, due, found) {
        (true, _, found) => found,
        (false, Some(due), Some(found)) => Some(due.min(found)),
        (false, due, found) => due.or(found),
    }
}

/// Whether a package can hold `space`: a chat, or a workspace whose
/// worktree is ready.
pub(super) fn packageable(space: &Space) -> bool {
    space
        .worktree()
        .is_none_or(|worktree| worktree.state == WorkspaceState::Ready)
}

/// What a space that `recorded` holds of needs at `now` while its state is
/// `state`.
fn look(recorded: &Recorded, state: String, now: UnixTime) -> Look {
    if recorded.sent.as_ref() == Some(&state) {
        return Look::Current;
    }
    match recorded.tried_at {
        Some(tried) if now.duration_since(tried) < SEND_EVERY => {
            Look::Wait(SEND_EVERY - now.duration_since(tried))
        }
        _ => Look::Send(state),
    }
}

/// The state the last package of the space `id` that reached the hub
/// holds, none before one did.
pub(super) fn sent_state(tx: &Transaction<'_>, id: &SpaceId) -> Result<Option<String>> {
    Ok(load(tx, id)?.sent)
}

fn load(tx: &Transaction<'_>, id: &SpaceId) -> Result<Recorded> {
    let recorded = tx
        .query_row(
            "SELECT sent, tried_at, unsent_since FROM hub_package_states WHERE space_id=?1",
            [id],
            |row| {
                Ok(Recorded {
                    sent: row.get(0)?,
                    tried_at: row.get(1)?,
                    unsent_since: row.get(2)?,
                })
            },
        )
        .optional()?;
    Ok(recorded.unwrap_or_default())
}

/// Since when each space has changes the hub's package lacks, of those
/// that have.
pub(super) fn unsent(tx: &Transaction<'_>) -> Result<HashMap<SpaceId, UnixTime>> {
    let mut statement = tx.prepare(
        "SELECT space_id, unsent_since FROM hub_package_states WHERE unsent_since IS NOT NULL",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Record that the space `id` has had changes the hub's package lacks
/// since `since`, or none.
fn record_unsent_since(tx: &Transaction<'_>, id: &SpaceId, since: Option<UnixTime>) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_package_states(space_id, unsent_since) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET unsent_since=excluded.unsent_since",
        params![id, since],
    )?;
    Ok(())
}

/// Record that this machine tries to send a package of the space `id` now.
fn record_tried(tx: &Transaction<'_>, id: &SpaceId) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_package_states(space_id, tried_at) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET tried_at=excluded.tried_at",
        params![id, UnixTime::now()],
    )?;
    Ok(())
}

/// Record that the hub keeps a package of the space `id` that holds
/// `state`, which it has no changes past.
pub(super) fn record_sent(tx: &Transaction<'_>, id: &SpaceId, state: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO hub_package_states(space_id, sent) VALUES (?1,?2)
         ON CONFLICT(space_id) DO UPDATE SET sent=excluded.sent, unsent_since=NULL",
        params![id, state],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
