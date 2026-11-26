//! Taking a space's PR gate, and verifying the worktree a request about
//! its items relies on.
use anyhow::Result;
use shoal_daemon_base::gates::PrGuard;
use shoal_protocol::space_names::SpaceSelector;

use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::{Space, Workspace};

impl Spaces {
    /// Reads the workspace again once its PR gate is held.
    pub async fn lock_pr_gate(&self, selector: &SpaceSelector) -> Result<(Workspace, PrGuard)> {
        let id = self.workspace(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((self.workspace_by_id(&id).await?, guard))
    }

    /// Reads the space again once its PR gate is held.
    pub async fn lock_space_gate(&self, selector: &SpaceSelector) -> Result<(Space, PrGuard)> {
        let id = self.space(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((self.space(&SpaceSelector::id(&id)).await?, guard))
    }

    /// A chat owns no worktree to verify, nor does a workspace whose worktree
    /// does not exist yet: it reads its forge in the checkout.
    pub async fn verify_space(&self, daemon: &dyn Daemon, space: &Space) -> Result<()> {
        match space.to_workspace() {
            Some(workspace) if !workspace.checkout.unborn() => {
                self.verify_worktree(daemon, &workspace).await
            }
            _ => Ok(()),
        }
    }
}
