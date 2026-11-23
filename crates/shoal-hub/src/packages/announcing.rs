//! A machine's side of its sleep. As the machine goes to sleep, it first
//! tells the hub so in one small notice, naming each space whose package
//! the hub keeps lacks changes and since when, then sends a new package of
//! each space that changed, all at once, each over a stream of its own,
//! until they are done or the platform's allowance runs out. Then, once it
//! also handed over the CI jobs the sleep cut off, it closes its link,
//! which it opens again only once the machine fully wakes, so a dark wake
//! neither tells the hub it is back nor races a takeover. An
//! intermittent machine names the spaces whose packages lag; an always-on
//! one keeps no package current, so it names every space as lagging by an
//! unknown time, and sends those with work.
use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        availability::Availability,
        link::{Call, Reply},
        sleep::Lagging,
    },
    model::Space,
    state::AgentState,
};
use tokio::time::Instant;

use super::sending::{self, packageable};
use crate::{Hub, daemon::Daemon, log, membership, session::Calls, time::UnixTime};

impl Hub {
    /// Tell the hub this machine goes to sleep and send it the packages of
    /// the spaces that changed, by `deadline`, keeping the link closed
    /// from [`Self::slept`] until [`Self::woke`]; on the hub's own machine
    /// and on one in no hub, do nothing more.
    pub async fn announce_sleep(&self, daemon: &dyn Daemon, deadline: Instant) {
        self.member.sleeps(true);
        let joined = matches!(self.store.read(membership::load).await, Ok(Some(_)));
        if !self.is_hub(daemon)
            && joined
            && let Some(calls) = self.member.calls()
        {
            let announced = self.send_sleep(daemon, &calls);
            if tokio::time::timeout_at(deadline, announced).await.is_err() {
                log!(
                    "hub: the packages of this machine's spaces did not all reach the hub before it slept"
                );
            }
        }
    }

    /// Close the link once the machine told the hub all it does before
    /// it sleeps, which stays closed until [`Self::woke`].
    pub fn slept(&self) {
        self.member.reread();
    }

    /// Open the link again once the machine fully woke, which tells the
    /// hub it is back.
    pub fn woke(&self) {
        self.member.sleeps(false);
        self.member.reread();
    }

    /// Send the hub over `calls` the notice that this machine sleeps, then
    /// the packages of its spaces that changed.
    async fn send_sleep(&self, daemon: &dyn Daemon, calls: &Calls) {
        let intermittent = self.member.has_availability(Availability::Intermittent);
        let spaces = match self.send_sleep_notice(daemon, calls, intermittent).await {
            Ok(spaces) => spaces,
            Err(error) => {
                return log!("hub: could not tell the hub this machine sleeps: {error:#}");
            }
        };
        (self.send_current_packages(daemon, calls, spaces, !intermittent)).await;
    }

    /// Send the hub over `calls` the notice that this machine sleeps,
    /// returning the spaces whose packages to send: on an `intermittent`
    /// machine, which keeps the hub's packages current, every space a
    /// package can hold, and on any other, which keeps none, only those
    /// with work.
    async fn send_sleep_notice(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        intermittent: bool,
    ) -> Result<Vec<Space>> {
        let spaces = daemon.listed_spaces(None).await?;
        let spaces: Vec<Space> = spaces.into_iter().filter(packageable).collect();
        let unsent = match intermittent {
            true => Some(self.store.read(sending::unsent).await?),
            false => None,
        };
        let lagging = lags(spaces.iter().map(|space| &space.id), unsent.as_ref());
        let sending = match intermittent {
            true => spaces,
            false => with_work(daemon, spaces).await?,
        };
        let notice = Call::Sleeping {
            lagging,
            sent_at: UnixTime::now(),
        };
        match calls.call(notice).await? {
            Reply::Delivered => Ok(sending),
            reply => bail!("the hub answered with an unexpected {reply:?}"),
        }
    }
}

/// How far the packages of the spaces `ids` lag: as `unsent` records for
/// a machine that keeps them current, each space that has changes since
/// then, and for one that keeps none, every space by an unknown lag, so
/// the hub trusts no package it kept before.
fn lags<'a>(
    ids: impl Iterator<Item = &'a SpaceId>,
    unsent: Option<&HashMap<SpaceId, UnixTime>>,
) -> Vec<Lagging> {
    ids.filter_map(|id| {
        let since = match unsent {
            Some(unsent) => Some(*unsent.get(id)?),
            None => None,
        };
        Some(Lagging {
            space: id.clone(),
            since,
        })
    })
    .collect()
}

/// Those of `spaces` with work: an agent working, or one queued.
async fn with_work(daemon: &dyn Daemon, spaces: Vec<Space>) -> Result<Vec<Space>> {
    let queued: HashSet<SpaceId> = (daemon.visible_queue().await?.into_iter())
        .filter_map(|entry| entry.space_id)
        .collect();
    let working = |space: &Space| {
        let state = space.agent_state.as_ref().map(|status| status.state);
        state == Some(AgentState::Working) || queued.contains(&space.id)
    };
    Ok(spaces.into_iter().filter(working).collect())
}

#[cfg(test)]
mod tests;
