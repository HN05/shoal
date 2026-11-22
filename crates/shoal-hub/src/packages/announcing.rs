//! A machine's side of its sleep. As the machine goes to sleep, it first
//! tells the hub so in one small notice, naming each space whose package
//! the hub keeps lacks changes and since when, then sends a new package of
//! each space that changed, all at once, each over a stream of its own,
//! until they are done or the platform's allowance runs out. Then it closes
//! its link, which it opens again only once the machine fully wakes, so a
//! dark wake neither tells the hub it is back nor races a takeover. An
//! intermittent machine names the spaces whose packages lag; an always-on
//! one keeps no package current, so it names and sends every space with
//! work.
use std::collections::HashSet;

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
use crate::{Hub, daemon::Daemon, log, membership, session::Calls};

impl Hub {
    /// Tell the hub this machine goes to sleep and send it the packages of
    /// the spaces that changed, by `deadline`, then close the link until
    /// [`Self::woke`]; on the hub's own machine and on one in no hub, only
    /// keep the link closed.
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
        // Closes the link, which stays closed while the machine sleeps.
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
    /// returning the spaces whose packages to send: every space a package
    /// can hold on an `intermittent` machine, which keeps the hub's
    /// packages current and names those that lag, and otherwise every one
    /// with work, which it names.
    async fn send_sleep_notice(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        intermittent: bool,
    ) -> Result<Vec<Space>> {
        let spaces = daemon.listed_spaces(None).await?;
        let spaces: Vec<Space> = spaces.into_iter().filter(packageable).collect();
        let (lagging, sending) = match intermittent {
            true => {
                let unsent = self.store.read(sending::unsent).await?;
                let lagging = (spaces.iter())
                    .filter_map(|space| {
                        let since = unsent.get(&space.id)?;
                        Some(Lagging {
                            space: space.id.clone(),
                            since: Some(*since),
                        })
                    })
                    .collect();
                (lagging, spaces)
            }
            false => {
                let queued: HashSet<SpaceId> = (daemon.visible_queue().await?.into_iter())
                    .filter_map(|entry| entry.space_id)
                    .collect();
                let working = |space: &Space| {
                    let state = space.agent_state.as_ref().map(|status| status.state);
                    state == Some(AgentState::Working) || queued.contains(&space.id)
                };
                let sending: Vec<Space> = spaces.into_iter().filter(working).collect();
                let lagging = (sending.iter())
                    .map(|space| Lagging {
                        space: space.id.clone(),
                        since: None,
                    })
                    .collect();
                (lagging, sending)
            }
        };
        match calls.call(Call::Sleeping { lagging }).await? {
            Reply::Delivered => Ok(sending),
            reply => bail!("the hub answered with an unexpected {reply:?}"),
        }
    }
}
