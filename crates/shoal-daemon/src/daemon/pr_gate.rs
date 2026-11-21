//! Taking a space's PR gate by selector.
use anyhow::Result;

use super::{gates::PrGuard, manager::Manager};
use crate::model::{Space, Workspace};
use shoal_protocol::space_names::SpaceSelector;

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

    /// The space `selector` names, read again once its PR gate is held.
    pub(crate) async fn lock_space_gate(
        &self,
        selector: &SpaceSelector,
    ) -> Result<(Space, PrGuard)> {
        let id = self.space(selector).await?.id;
        let guard = self.gates.lock_prs([&id]).await;
        Ok((self.space(&SpaceSelector::id(&id)).await?, guard))
    }
}
