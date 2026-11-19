//! The spaces a scope reaches: the caller's own space and those below it,
//! on every joined machine, or, for a chat without a repository, which
//! manages the spaces of every repository, every space. Another machine
//! vouches for its own callers' reach.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;

use super::Caller;
use crate::{
    daemon::{manager::Manager, store},
    model::Space,
};

impl Manager {
    /// The space whose subtree `caller`'s scope reaches; none when it
    /// reaches every space, as the user's and a chat's without a repository
    /// do.
    pub(in crate::daemon) async fn reach_root(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Option<SpaceId>> {
        let Some(caller) = caller else {
            return Ok(None);
        };
        if let Some(vouched) = self.vouched_space(&caller.space_id) {
            return Ok((!vouched.everywhere).then(|| caller.space_id.clone()));
        }
        let id = caller.space_id.clone();
        let everywhere = (self.store)
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM spaces WHERE id=?1 AND repository_id IS NULL",
                    [id],
                )
            })
            .await?;
        Ok((!everywhere).then(|| caller.space_id.clone()))
    }

    /// Whether `caller`'s scope reaches the space `id`, through the spaces
    /// above it on other machines too.
    pub(in crate::daemon) async fn reaches(&self, caller: &Caller, id: &SpaceId) -> Result<bool> {
        match self.reach_root(Some(caller)).await? {
            Some(root) if self.within_subtree(&root, id).await? => Ok(true),
            Some(root) => Ok(self.ancestors_elsewhere(id).await.contains(&root)),
            None => Ok(true),
        }
    }

    /// Refuse `caller` the space `selector` names unless its scope reaches
    /// it, before it asks approval for an action there; none names no space.
    pub(in crate::daemon) async fn ensure_selected_reached(
        &self,
        caller: &Caller,
        selector: Option<&mut shoal_protocol::space_names::SpaceSelector>,
    ) -> Result<()> {
        match selector {
            Some(selector) => {
                self.ensure_reaches(caller, &self.space(selector).await?)
                    .await
            }
            None => Ok(()),
        }
    }

    /// Refuse `caller` the `space` its scope does not reach.
    pub(in crate::daemon) async fn ensure_reaches(
        &self,
        caller: &Caller,
        space: &Space,
    ) -> Result<()> {
        ensure!(
            self.reaches(caller, &space.id).await?,
            "{}",
            outside_scope(space)
        );
        Ok(())
    }

    /// The IDs of this machine's spaces `caller`'s scope reaches, through
    /// spaces on other machines too; none when it reaches every space.
    pub(in crate::daemon) async fn reached_spaces(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Option<Vec<SpaceId>>> {
        let Some(root) = self.reach_root(caller).await? else {
            return Ok(None);
        };
        let mut reached = self.subtree(&root).await?;
        // This machine's spaces below it through spaces elsewhere.
        for below in self.reached_through_elsewhere(&root).await {
            reached.extend(self.subtree(&below).await?);
        }
        Ok(Some(reached))
    }
}

/// Why a scoped caller is refused `space`, which its scope does not reach.
pub(in crate::daemon) fn outside_scope(space: &Space) -> String {
    format!("space {} is outside this space's scope", space.label())
}
