//! Taking a workspace's PR gate by selector, and the gates a space's own
//! change records on.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use super::{Manager, PrGuard};
use crate::client::space_names::SpaceSelector;
use crate::model::Workspace;

impl Manager {
    /// The workspace `selector` names, read again once its PR gate is held.
    pub(crate) async fn lock_pr_gate(
        &self,
        selector: &SpaceSelector,
    ) -> Result<(Workspace, PrGuard)> {
        let id = self.workspace(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((self.workspace_by_id(&id).await?, guard))
    }

    /// Hold the PR gates of the space `id`, of every space below it and of
    /// `others`. A change the space makes through Shoal is recorded on its
    /// cursor and on those of the spaces below it whose activity it receives
    /// when the change is recorded, so holding them all from before the
    /// change keeps a poll from recording it first, whichever agents start
    /// or stop meanwhile.
    pub(crate) async fn lock_change_gates(
        &self,
        id: &SpaceId,
        others: &[&SpaceId],
    ) -> Result<PrGuard> {
        let subtree = self.subtree(id).await?;
        let ids = subtree.iter().chain(others.iter().copied());
        Ok(self.gates.lock_prs(ids).await)
    }
}
