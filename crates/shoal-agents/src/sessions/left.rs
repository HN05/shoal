//! The places a space's agent left, such as those of the saved agents it
//! restores once it runs elsewhere. Shoal closes the ones it created, and keeps
//! those where another saved agent waits for its restore.
use std::path::PathBuf;

use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use crate::Agents;
use crate::daemon::Daemon;
use crate::execution::recovery::Saved;
use crate::execution::recovery::SavedExt as _;
use crate::log;
use crate::places;
use shoal_core::sessions::Origin;
use shoal_core::sessions::OriginExt as _;

impl Agents {
    /// The places the agent `execution` of `space` leaves: those the saved
    /// agents at `restored_from`, which it restores, ran in, except where it
    /// starts and where another saved agent waits. Read them before the
    /// start is acknowledged: the wrapper then consumes the records. Saved
    /// agents that cannot be read leave none.
    pub async fn left_places(
        &self,
        space: &SpaceId,
        execution: &ExecutionId,
        restored_from: &[PathBuf],
    ) -> Vec<Origin> {
        if restored_from.is_empty() {
            return Vec::new();
        }
        let saved = match Saved::load(&self.paths, space) {
            Ok(saved) => saved,
            Err(error) => {
                log!("saved agents of space {space} unreadable: {error:#}");
                return Vec::new();
            }
        };
        let (restored, waiting): (Vec<_>, Vec<_>) = (saved.agents.into_iter())
            .chain(saved.sessions)
            .partition(|session| restored_from.contains(&session.path));
        let mut kept: Vec<Origin> = (waiting.into_iter())
            .filter_map(|session| session.origin)
            .collect();
        kept.extend(self.execution_place(execution).await);
        let ran_in = (restored.into_iter())
            .filter_map(|session| session.origin)
            .filter(|origin| origin.place.is_some());
        let mut left: Vec<Origin> = Vec::new();
        for origin in ran_in {
            let known = (kept.iter().chain(&left)).any(|known| same_place(known, &origin));
            if !known {
                left.push(origin);
            }
        }
        left
    }

    /// Close `places`, which the agent of `space` left, where the space's
    /// record or its host's label says Shoal created them for it; failures
    /// are visible and the place stays.
    pub async fn close_left_places(
        &self,
        daemon: &dyn Daemon,
        space: &SpaceId,
        places: Vec<Origin>,
    ) {
        for place in places {
            if let Err(error) = self.close_left_place(space, &place).await {
                self.session_cleanup_failed(daemon, None, &error).await;
            }
        }
    }

    /// Close `place`, which the agent of `space` left, unless an execution
    /// is recorded there now, read under the lock registrations take a
    /// place under.
    async fn close_left_place(&self, space: &SpaceId, place: &Origin) -> Result<()> {
        let _ownership = self.place_ownership.lock().await;
        let lookup = place.clone();
        if (self.store)
            .run(move |db| places::occupied(db, &lookup))
            .await?
        {
            return Ok(());
        }
        self.close_recorded(space, place).await.map(drop)
    }
}

/// Whether `a` and `b` name one place, by host, server and ID as the
/// daemon keys its records of places, whatever else each records of it.
fn same_place(a: &Origin, b: &Origin) -> bool {
    a.host == b.host && a.server() == b.server() && a.id() == b.id()
}

#[cfg(test)]
mod tests;
