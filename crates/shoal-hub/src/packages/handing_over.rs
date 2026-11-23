//! A machine's side of its hand-over as it shuts down, or as its user walks
//! away or pauses it: once its work stopped, it sends the hub the final
//! package of each space it hands over with the start its agent would
//! make, all at once, each over a stream of its own, until the deadline it
//! has, and learns where the hub sends each. The hub's own machine, once
//! paused, keeps each package as the hub and places it among the others
//! itself. Nothing waits for a space to continue there.
use std::{collections::HashSet, sync::Arc};

use anyhow::Result;
use futures_util::future::join_all;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{
    hub::{
        link::{Call, Reply},
        packages::{Handed, Handing, Leaving},
        placement::Decision,
    },
    model::Space,
};
use tokio::time::Instant;

use super::sending::{self, packageable};
use crate::{
    Hub,
    daemon::Daemon,
    log, membership,
    session::{Calls, Unanswered},
    time::UnixTime,
};

mod own;
pub(crate) use own::OwnReleases;

/// Where this machine sends the spaces it hands over.
enum Route {
    /// Over its link, to the hub it joined.
    Link(Arc<Calls>),
    /// To the hub this daemon is, as the hub's own machine `MemberId`.
    Own(MemberId),
}

/// Why a machine in no hub keeps the spaces a pause would hand over.
const IN_NO_HUB: &str = "this machine is in no hub";

impl Hub {
    /// Whether this machine hands its spaces over as it is `leaving`: one
    /// joined to a hub, and the hub's own, which keeps running to place
    /// them, only once its user paused it.
    pub async fn hands_over(&self, daemon: &dyn Daemon, leaving: Leaving) -> bool {
        if self.is_hub(daemon) {
            return leaving == Leaving::Paused;
        }
        matches!(self.store.read(membership::load).await, Ok(Some(_)))
    }

    /// Hand each of this machine's spaces `handing` takes but those `kept`
    /// here over through the hub by `deadline` as it is `leaving`, and
    /// where each went; none on a machine that [hands nothing
    /// over](Self::hands_over) then, but for a pause the spaces it takes,
    /// which stay in a machine in no hub.
    pub async fn hand_over(
        &self,
        daemon: &dyn Daemon,
        deadline: Instant,
        (handing, kept): (&Handing, &HashSet<SpaceId>),
        leaving: Leaving,
    ) -> Option<Vec<(Space, Handed)>> {
        let hands_over = self.hands_over(daemon, leaving).await;
        if !hands_over && leaving == Leaving::Away {
            return None;
        }
        let spaces = match daemon.listed_spaces(None).await {
            Ok(spaces) => spaces,
            Err(error) => {
                log!("hub: could not list the spaces to hand over: {error:#}");
                return Some(Vec::new());
            }
        };
        let spaces = (spaces.into_iter()).filter(|space| {
            packageable(space) && handing.takes(space) && !kept.contains(&space.id)
        });
        let route = match hands_over {
            true => self
                .route(daemon)
                .await
                .map_err(|error| format!("{error:#}")),
            false => Err(IN_NO_HUB.to_owned()),
        };
        let route = match route {
            Ok(route) => route,
            Err(why) => {
                return Some(
                    spaces
                        .map(|space| (space, Handed::Stayed(why.clone())))
                        .collect(),
                );
            }
        };
        // A chat in a workspace's worktree follows that workspace.
        let (chats, others): (Vec<Space>, Vec<Space>) =
            spaces.partition(|space| space.within().is_some());
        let none = HashSet::new();
        let mut handed = (self.hand_over_all(daemon, &route, others, (deadline, &none))).await;
        let left: HashSet<SpaceId> = (handed.iter())
            .filter(|(_, handed)| handed.left())
            .map(|(space, _)| space.id.clone())
            .collect();
        handed.extend((self.hand_over_all(daemon, &route, chats, (deadline, &left))).await);
        Some(handed)
    }

    /// Where this machine sends the spaces it hands over: the hub its daemon
    /// is, or else the one it joined, failing while its link is down.
    async fn route(&self, daemon: &dyn Daemon) -> Result<Route> {
        match self.is_hub(daemon) {
            true => Ok(Route::Own(self.own_member().await?)),
            false => Ok(Route::Link(self.hub_calls().await?)),
        }
    }

    /// Hand each of `spaces` over through the hub on `route` by
    /// `deadline`, all at once, a chat in a workspace's worktree only once
    /// this machine `left` that workspace to the hub, and where each went.
    async fn hand_over_all(
        &self,
        daemon: &dyn Daemon,
        route: &Route,
        spaces: Vec<Space>,
        (deadline, left): (Instant, &HashSet<SpaceId>),
    ) -> Vec<(Space, Handed)> {
        let handing = spaces.into_iter().map(|space| async move {
            let handed = match space.within() {
                Some(workspace) if !left.contains(workspace) => {
                    let why = "the workspace whose worktree it runs in stays here";
                    Handed::Stayed(why.into())
                }
                within => (self.hand_over_by(daemon, route, (&space, within), deadline)).await,
            };
            (space, handed)
        });
        join_all(handing).await
    }

    /// Hand `space` over through the hub on `route` by `deadline`, and
    /// where it went: once it read where the hub sends it, this machine
    /// leaves it there, and whether the hub took that word decides only
    /// whether the space surely went.
    async fn hand_over_by(
        &self,
        daemon: &dyn Daemon,
        route: &Route,
        (space, within): (&Space, Option<&SpaceId>),
        deadline: Instant,
    ) -> Handed {
        let handing = self.send_final_package(daemon, route, (space, within));
        let decision = match tokio::time::timeout_at(deadline, handing).await {
            Ok(Ok(Some(decision))) => decision,
            Ok(Ok(None)) => {
                return Handed::Stayed("no other machine of the hub can take it".into());
            }
            Ok(Err(error)) => {
                log!("hub: could not hand space {} over: {error:#}", space.name);
                return Handed::Stayed(format!("{error:#}"));
            }
            Err(_) => {
                let why = "its package did not reach the hub in time";
                return Handed::Stayed(why.into());
            }
        };
        let released = self.release(route, &space.id, deadline).await;
        if !matches!(released, Some(Ok(Reply::Delivered))) {
            log!(
                "hub: the hub did not take space {} as released: {released:?}",
                space.name
            );
        }
        released_as(released, decision)
    }

    /// Send the hub on `route` the final package of `space`, a chat in the
    /// worktree of the workspace `within` when it names one, and where the
    /// hub sends the space, none where no machine can take it.
    async fn send_final_package(
        &self,
        daemon: &dyn Daemon,
        route: &Route,
        (space, within): (&Space, Option<&SpaceId>),
    ) -> Result<Option<Decision>> {
        match route {
            Route::Link(calls) => self.hand_over_space(daemon, calls, (space, within)).await,
            Route::Own(own) => self.hand_over_own(daemon, own, (space, within)).await,
        }
    }

    /// Tell the hub on `route` that this machine leaves the space `id`
    /// where the hub sends it, and the hub's answer, none when it did not
    /// come by `deadline`.
    async fn release(
        &self,
        route: &Route,
        id: &SpaceId,
        deadline: Instant,
    ) -> Option<Result<Reply>> {
        match route {
            Route::Link(calls) => {
                let release = calls.call(Call::Release { space: id.clone() });
                tokio::time::timeout_at(deadline, release).await.ok()
            }
            Route::Own(own) => Some(self.release_own(own, id).map(|()| Reply::Delivered)),
        }
    }

    /// Tell the hub that this machine stays after all, as once its shutdown
    /// was cancelled or its user came back or unpaused it, so it retires
    /// the spaces it handed over that other machines took; as the hub,
    /// retire them here; nothing on a machine in no hub.
    pub async fn stay(&self, daemon: &dyn Daemon) {
        if self.is_hub(daemon) {
            return self.stay_own(daemon).await;
        }
        if !self.hands_over(daemon, Leaving::Away).await {
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
        (space, within): (&Space, Option<&SpaceId>),
    ) -> Result<Option<Decision>> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        let built_at = UnixTime::now();
        // Taken before the package, so a copy that changes after it differs.
        let state = daemon.package_state(space).await?;
        let packaged = daemon.write_package(space, &file).await?;
        let start = daemon.package_start(space, &packaged).await?;
        let hand_over = |stream| Call::HandOver {
            space: space.id.clone(),
            built_at,
            stream,
            start,
            within: within.cloned(),
        };
        let (mut answer, mut package) = calls.duplex(hand_over).await?;
        package.write_file(&file).await?;
        package.end(None).await;
        let mut written = Vec::new();
        while let Some(bytes) = answer.next().await? {
            written.extend(bytes);
        }
        // The hub keeps it whole, which a takeover goes by.
        let id = space.id.clone();
        (self.store)
            .write(move |tx| sending::record_sent(tx, &id, &state))
            .await?;
        Ok(serde_json::from_slice(&written)?)
    }
}

/// Where a space this machine released to the hub, which sends it as
/// `decision`, went, as the hub's answer to the release says, none when it
/// did not come in time: the hub continues it once it took the release,
/// and may have when its answer never came; it surely did not when it
/// refused the release or the release never left this machine.
fn released_as(released: Option<Result<Reply>>, decision: Decision) -> Handed {
    match released {
        Some(Ok(Reply::Delivered)) => Handed::Went(decision),
        None => Handed::Unconfirmed(decision),
        Some(Err(error)) if error.is::<Unanswered>() => Handed::Unconfirmed(decision),
        Some(Err(error)) => Handed::Stayed(format!("the hub refused it: {error:#}")),
        Some(Ok(reply)) => Handed::Stayed(format!("the hub answered with {reply:?}")),
    }
}

#[cfg(test)]
mod tests;
