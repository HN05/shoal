//! The places a space's agent left: those its saved agents ran in, which
//! serve nothing once the space's agent runs elsewhere, as after a restore
//! that could not run where its agent stopped. Shoal closes the ones it
//! created then, so `shoal status` and `shoal attach` show only the place
//! the agent runs in.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;

use super::super::{Manager, places};
use crate::{
    daemon::log,
    execution::recovery::{Saved, SavedExt as _},
    sessions::{Origin, OriginExt as _},
};

impl Manager {
    /// The places the saved agents of `space` ran in, other than the one
    /// its agent `execution` starts in, which that agent leaves. Read them
    /// before the start is acknowledged: the wrapper then consumes what it
    /// restored from. Saved agents that cannot be read leave none.
    pub(crate) async fn left_places(
        &self,
        space: &SpaceId,
        execution: &ExecutionId,
    ) -> Vec<Origin> {
        let runs_in = self.execution_place(execution).await;
        let saved = match Saved::load(&self.paths, space) {
            Ok(saved) => saved,
            Err(error) => {
                log!("saved agents of space {space} unreadable: {error:#}");
                return Vec::new();
            }
        };
        let ran_in = (saved.agents.into_iter().chain(saved.sessions))
            .filter_map(|session| session.origin)
            .filter(|origin| origin.place.is_some());
        let mut left: Vec<Origin> = Vec::new();
        for origin in ran_in {
            let known = runs_in
                .iter()
                .chain(&left)
                .any(|known| same_place(known, &origin));
            if !known {
                left.push(origin);
            }
        }
        left
    }

    /// Close `places`, which the agent of `space` left, where the space's
    /// record or its host's label says Shoal created them for it; failures
    /// are visible and the place stays.
    pub(crate) async fn close_left_places(&self, space: &SpaceId, places: Vec<Origin>) {
        for place in places {
            if let Err(error) = self.close_left_place(space, &place).await {
                self.session_cleanup_failed(None, &error).await;
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
