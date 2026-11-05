//! Taking a workspace's PR gate by selector, and the gates a space's own
//! change records on.
use anyhow::Result;

use super::{Manager, PrGuard};
use crate::model::Workspace;

impl Manager {
    /// The workspace `selector` names, read again once its PR gate is held.
    pub(crate) async fn lock_pr_gate(&self, selector: &str) -> Result<(Workspace, PrGuard)> {
        let id = self.workspace(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((self.workspace(&id).await?, guard))
    }

    /// Hold the PR gates of the space `id`, of the spaces below it whose
    /// activity its agent receives and of `others`. A change the space makes
    /// through Shoal is recorded on the cursors of the first two, so holding
    /// them from before the change keeps a poll from recording it first.
    pub(crate) async fn lock_change_gates(&self, id: &str, others: &[&str]) -> Result<PrGuard> {
        let children = self.delegated_spaces(id).await?;
        let receiving = children.iter().map(|child| child.id.as_str());
        let ids = receiving.chain([id]).chain(others.iter().copied());
        Ok(self.gates.lock_prs(ids).await)
    }
}
