//! What checking a launch's new chat before a start's pickers refuses:
//! the chat settled as its start settles it, with an issue a picker has
//! yet to choose left open, the workspace whose worktree it runs in being
//! ready there, and the parent it goes below.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::super::shape::Shape;
use crate::{
    daemon::{manager::Manager, operations::starts::Home, scope::Caller},
    model::Workspace,
};

impl Manager {
    /// Refuse what starting the new chat `launch` names in `shape` for
    /// `caller` would refuse, settled as [`Self::chat_placement`] settles
    /// it, with an issue a picker has yet to choose left open; returns the
    /// workspace whose worktree it runs in, if any.
    pub(in crate::daemon::launches) async fn check_chat_placement(
        &self,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        let (home, _) = self.settle_chat(launch, shape, true, caller).await?;
        let host = match &home {
            Home::Worktree(workspace) => Some(self.chat_workspace(workspace).await?),
            _ => None,
        };
        let repository = self.home_repository(&home).await?;
        (self.check_launch_parent(launch, repository.as_ref(), caller)).await?;
        Ok(host)
    }
}

#[cfg(test)]
mod tests;
