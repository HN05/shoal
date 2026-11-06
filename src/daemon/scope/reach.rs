//! The spaces a scope reaches: the caller's own space and those below it,
//! or, for a chat without a repository, which manages the spaces of every
//! repository, every space.
use anyhow::Result;

use super::Caller;
use crate::daemon::{store, workspace::Manager};

impl Manager {
    /// The space whose subtree `caller`'s scope reaches; none when it
    /// reaches every space, as the user's and a chat's without a repository
    /// do.
    pub(in crate::daemon) async fn reach_root(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Option<String>> {
        let Some(caller) = caller else {
            return Ok(None);
        };
        let id = caller.space_id.clone();
        let everywhere = (self.store)
            .run(move |db| {
                store::exists(
                    db,
                    "SELECT 1 FROM chats WHERE id=?1 AND repository_id IS NULL",
                    [id],
                )
            })
            .await?;
        Ok((!everywhere).then(|| caller.space_id.clone()))
    }

    /// Whether `caller`'s scope reaches the space `id`.
    pub(in crate::daemon) async fn reaches(&self, caller: &Caller, id: &str) -> Result<bool> {
        match self.reach_root(Some(caller)).await? {
            Some(root) => self.within_subtree(&root, id).await,
            None => Ok(true),
        }
    }

    /// The IDs of the spaces `caller`'s scope reaches; none when it
    /// reaches every space.
    pub(in crate::daemon) async fn reached_spaces(
        &self,
        caller: Option<&Caller>,
    ) -> Result<Option<Vec<String>>> {
        match self.reach_root(caller).await? {
            Some(root) => Ok(Some(self.subtree(&root).await?)),
            None => Ok(None),
        }
    }
}
