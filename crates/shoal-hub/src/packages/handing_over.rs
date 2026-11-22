//! A machine's side of its hand-over as it shuts down: once its work
//! stopped, it sends the hub the final package of each of its spaces with
//! the start its agent would make, all at once, each over a stream of its
//! own, until the deadline the shutdown leaves it, and learns where the hub
//! sends each. Nothing waits for a space to continue there.
use std::collections::HashSet;

use anyhow::Result;
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{link::Call, packages::Handed, placement::Decision},
    model::Space,
};
use tokio::time::Instant;

use super::sending::packageable;
use crate::{Hub, daemon::Daemon, log, membership, session::Calls, time::UnixTime};

impl Hub {
    /// Hand each of this machine's spaces but those `kept` here over
    /// through the hub by `deadline`, and where each went; none on the
    /// hub's own machine and on one in no hub, which keep their spaces.
    pub async fn hand_over(
        &self,
        daemon: &dyn Daemon,
        deadline: Instant,
        kept: &HashSet<SpaceId>,
    ) -> Option<Vec<(Space, Handed)>> {
        if self.is_hub(daemon) || !matches!(self.store.read(membership::load).await, Ok(Some(_))) {
            return None;
        }
        let spaces = match daemon.listed_spaces(None).await {
            Ok(spaces) => spaces,
            Err(error) => {
                log!("hub: could not list the spaces to hand over: {error:#}");
                return Some(Vec::new());
            }
        };
        let spaces: Vec<Space> = (spaces.into_iter())
            .filter(|space| packageable(space) && !kept.contains(&space.id))
            .collect();
        let calls = self.hub_calls().await;
        let handing = spaces.into_iter().map(|space| {
            let calls = calls.as_ref();
            async move {
                let handed = match calls {
                    Ok(calls) => self.hand_over_by(daemon, calls, &space, deadline).await,
                    Err(error) => Handed::Stayed(format!("{error:#}")),
                };
                (space, handed)
            }
        });
        Some(join_all(handing).await)
    }

    /// Hand `space` over through the hub over `calls` by `deadline`, and
    /// where it went.
    async fn hand_over_by(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        space: &Space,
        deadline: Instant,
    ) -> Handed {
        let handing = self.hand_over_space(daemon, calls, space);
        match tokio::time::timeout_at(deadline, handing).await {
            Ok(Ok(Some(decision))) => Handed::Went(decision),
            Ok(Ok(None)) => Handed::Stayed("no other machine of the hub can take it".into()),
            Ok(Err(error)) => {
                log!("hub: could not hand space {} over: {error:#}", space.name);
                Handed::Stayed(format!("{error:#}"))
            }
            Err(_) => {
                Handed::Stayed("its package did not reach the hub before the shutdown".into())
            }
        }
    }

    /// Tell the hub that this machine stays after all, as once its shutdown
    /// was cancelled, so it retires the spaces it handed over that other
    /// machines took; nothing on the hub's own machine and on one in no hub.
    pub async fn stay(&self, daemon: &dyn Daemon) {
        if self.is_hub(daemon) || !matches!(self.store.read(membership::load).await, Ok(Some(_))) {
            return;
        }
        // While the link is down, the hub has it retire them once it opens.
        if let Err(error) = self.through_hub(Call::Stay).await {
            log!("hub: could not tell the hub this machine stays: {error:#}");
        }
    }

    /// Send the hub the final package of `space` over a stream of
    /// `calls`, and where the hub sends the space, none where no machine
    /// can take it.
    async fn hand_over_space(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        space: &Space,
    ) -> Result<Option<Decision>> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        let built_at = UnixTime::now();
        let packaged = daemon.write_package(space, &file).await?;
        let start = daemon.handover_start(space, &packaged).await?;
        let hand_over = |stream| Call::HandOver {
            space: space.id.clone(),
            built_at,
            stream,
            start,
        };
        let (mut answer, mut package) = calls.duplex(hand_over).await?;
        package.write_file(&file).await?;
        package.end(None).await;
        let mut written = Vec::new();
        while let Some(bytes) = answer.next().await? {
            written.extend(bytes);
        }
        Ok(serde_json::from_slice(&written)?)
    }
}
