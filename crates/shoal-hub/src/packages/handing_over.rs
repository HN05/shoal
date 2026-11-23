//! A machine's side of its hand-over as it shuts down, or as its user walks
//! away: once its work stopped, it sends the hub the final package of each
//! space it hands over with the start its agent would make, all at once,
//! each over a stream of its own, until the deadline it has, and learns
//! where the hub sends each. Nothing waits for a space to continue there.
use std::collections::HashSet;

use anyhow::Result;
use futures_util::future::join_all;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{
        link::{Call, Reply},
        packages::{Handed, Handing},
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

impl Hub {
    /// Whether this machine hands its spaces over: one joined to a hub,
    /// but not the hub's own, which has nowhere to send them.
    pub async fn hands_over(&self, daemon: &dyn Daemon) -> bool {
        !self.is_hub(daemon) && matches!(self.store.read(membership::load).await, Ok(Some(_)))
    }

    /// Hand each of this machine's spaces `handing` takes but those `kept`
    /// here over through the hub by `deadline`, and where each went; none
    /// on a machine that [hands nothing over](Self::hands_over).
    pub async fn hand_over(
        &self,
        daemon: &dyn Daemon,
        deadline: Instant,
        (handing, kept): (&Handing, &HashSet<SpaceId>),
    ) -> Option<Vec<(Space, Handed)>> {
        if !self.hands_over(daemon).await {
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
        let calls = match self.hub_calls().await {
            Ok(calls) => calls,
            Err(error) => {
                let why = format!("{error:#}");
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
        let mut handed = (self.hand_over_all(daemon, &calls, others, (deadline, &none))).await;
        let left: HashSet<SpaceId> = (handed.iter())
            .filter(|(_, handed)| handed.left())
            .map(|(space, _)| space.id.clone())
            .collect();
        handed.extend((self.hand_over_all(daemon, &calls, chats, (deadline, &left))).await);
        Some(handed)
    }

    /// Hand each of `spaces` over through the hub over `calls` by
    /// `deadline`, all at once, a chat in a workspace's worktree only once
    /// this machine `left` that workspace to the hub, and where each went.
    async fn hand_over_all(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        spaces: Vec<Space>,
        (deadline, left): (Instant, &HashSet<SpaceId>),
    ) -> Vec<(Space, Handed)> {
        let handing = spaces.into_iter().map(|space| async move {
            let handed = match space.within() {
                Some(workspace) if !left.contains(workspace) => {
                    let why = "the workspace whose worktree it runs in stays here";
                    Handed::Stayed(why.into())
                }
                within => (self.hand_over_by(daemon, calls, (&space, within), deadline)).await,
            };
            (space, handed)
        });
        join_all(handing).await
    }

    /// Hand `space` over through the hub over `calls` by `deadline`, and
    /// where it went: once it read where the hub sends it, this machine
    /// leaves it there, and whether the hub took that word decides only
    /// whether the space surely went.
    async fn hand_over_by(
        &self,
        daemon: &dyn Daemon,
        calls: &Calls,
        (space, within): (&Space, Option<&SpaceId>),
        deadline: Instant,
    ) -> Handed {
        let handing = self.hand_over_space(daemon, calls, (space, within));
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
        let release = calls.call(Call::Release {
            space: space.id.clone(),
        });
        let released = tokio::time::timeout_at(deadline, release).await.ok();
        if !matches!(released, Some(Ok(Reply::Delivered))) {
            log!(
                "hub: the hub did not take space {} as released: {released:?}",
                space.name
            );
        }
        released_as(released, decision)
    }

    /// Tell the hub that this machine stays after all, as once its shutdown
    /// was cancelled or its user came back, so it retires the spaces it
    /// handed over that other machines took; nothing on a machine that
    /// [hands nothing over](Self::hands_over).
    pub async fn stay(&self, daemon: &dyn Daemon) {
        if !self.hands_over(daemon).await {
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
