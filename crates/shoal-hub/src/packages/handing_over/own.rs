//! The hub's own machine handing its spaces over once its user paused it.
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow};
use shoal_core::fsutil::off_runtime;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::{hub::placement::Decision, model::Space};
use tokio::sync::mpsc;

use super::super::{Continuation, Sent, handovers::Released, sending};
use crate::{Hub, daemon::Daemon, log, time::UnixTime};

/// A space the hub's own machine left to the hub: that machine, the
/// space, and where it goes.
type OwnRelease = (MemberId, SpaceId, Released);

/// The spaces the hub's own machine left to the hub, which its background
/// continues where the hub placed them.
pub(crate) struct OwnReleases {
    sender: mpsc::UnboundedSender<OwnRelease>,
    receiver: Mutex<Option<mpsc::UnboundedReceiver<OwnRelease>>>,
}

impl Default for OwnReleases {
    fn default() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    }
}

impl Hub {
    /// As the paused hub's own machine `own`: keep the final package of
    /// `space` as the hub, and where the hub sends it among the other
    /// connected machines, or for a chat in the workspace `within`'s
    /// worktree where it sent that workspace; none where no machine can
    /// take it.
    pub(super) async fn hand_over_own(
        &self,
        daemon: &dyn Daemon,
        own: &MemberId,
        (space, within): (&Space, Option<&SpaceId>),
    ) -> Result<Option<Decision>> {
        self.ensure_package_room(daemon).await?;
        self.handovers.leaves(own);
        let start = self.keep_own_package(daemon, own, (space, within)).await?;
        let decision = match within {
            Some(workspace) => Some(self.handovers.placed_workspace(workspace, own)?),
            None => self.place_elsewhere(daemon, own, start).await?,
        };
        if let Some(decision) = &decision {
            (self.handovers).place(&space.id, (own, within), decision.clone());
        }
        Ok(decision)
    }

    /// Keep the package of `space`, which the hub's own machine `own` hands
    /// over following the workspace `within`, as the hub keeps a package a
    /// machine sends, with the state it was built from; the start its agent
    /// would make.
    async fn keep_own_package(
        &self,
        daemon: &dyn Daemon,
        own: &MemberId,
        (space, within): (&Space, Option<&SpaceId>),
    ) -> Result<shoal_schedule::placement::snapshot::Request> {
        let incoming = self.incoming_package_directory().await?;
        let file = incoming.path().join("package.shoal");
        let built_at = UnixTime::now();
        // Taken before the package, so a copy that changes after it differs.
        let state = daemon.package_state(space).await?;
        let packaged = daemon.write_package(space, &file).await?;
        let start = daemon.package_start(space, &packaged).await?;
        let size = file_size(&file).await?;
        let sent = Sent {
            member: own.clone(),
            space: space.id.clone(),
            built_at,
            arrived_at: built_at,
            continuation: Continuation {
                start: Some(start.clone()),
                within: within.cloned(),
            },
        };
        let incoming_file = tempfile::TempPath::try_from_path(&file)?;
        self.place_package(sent, size, incoming_file).await?;
        let id = space.id.clone();
        (self.store)
            .write(move |tx| sending::record_sent(tx, &id, &state))
            .await?;
        Ok(start)
    }

    /// As the paused hub's own machine `own`: leave the space `id` where
    /// the hub placed it, for the hub's background to continue it there.
    pub(super) fn release_own(&self, own: &MemberId, id: &SpaceId) -> Result<()> {
        let released = self.handovers.release(id, own)?;
        (self.own_releases.sender)
            .send((own.clone(), id.clone(), released))
            .map_err(|_| anyhow!("the hub no longer continues the spaces handed over"))
    }

    /// As the hub: continue each space its own machine leaves it where it
    /// placed the space, until the daemon stops.
    pub(crate) async fn keep_continuing_own(self: &Arc<Self>, daemon: &Arc<dyn Daemon>) {
        let Some(mut receiver) = self.own_releases.receiver.lock().unwrap().take() else {
            return;
        };
        while let Some((own, id, released)) = receiver.recv().await {
            tokio::spawn((self.clone()).continue_released(daemon.clone(), own, id, released));
        }
    }

    /// As the hub, whose own machine was paused and is no longer: stay, and
    /// retire the copies of the spaces taken over from it now.
    pub(super) async fn stay_own(&self, daemon: &dyn Daemon) {
        let own = match self.own_member().await {
            Ok(own) => own,
            Err(error) => return log!("hub: could not read its own machine to stay: {error:#}"),
        };
        self.handovers.stays(&own);
        self.retire_own(daemon, &own).await;
    }
}

async fn file_size(path: &std::path::Path) -> Result<u64> {
    let path = path.to_owned();
    off_runtime::run(move || {
        let metadata = std::fs::metadata(&path);
        Ok(metadata
            .with_context(|| format!("read {}", path.display()))?
            .len())
    })
    .await
}

#[cfg(test)]
mod tests;
