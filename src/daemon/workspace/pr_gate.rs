//! Taking a workspace's PR gate by selector.
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
}
