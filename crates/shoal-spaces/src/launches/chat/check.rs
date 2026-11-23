//! What checking a launch's new chat before a start's pickers refuses:
//! the chat settled as its start settles it, with an issue a picker has
//! yet to choose left open, the workspace whose worktree it runs in being
//! ready there, and the parent it goes below.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::super::shape::Shape;
use crate::{
    model::Workspace,
    {scope::Caller, starts::Home},
};

impl Spaces {
    /// Refuse what starting the new chat `launch` names in `shape` for
    /// `caller` would refuse, settled as [`Self::chat_placement`] settles
    /// it, with an issue a picker has yet to choose left open; returns the
    /// workspace whose worktree it runs in, if any.
    pub(in crate::launches) async fn check_chat_placement(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        shape: &Shape,
        caller: Option<&Caller>,
    ) -> Result<Option<Workspace>> {
        let (home, _) = self
            .settle_chat(daemon, launch, shape, true, caller)
            .await?;
        let host = match &home {
            Home::Worktree(workspace) => Some(self.chat_workspace(daemon, workspace).await?),
            _ => None,
        };
        let repository = self.home_repository(daemon, &home).await?;
        (self.check_launch_parent(daemon, launch, repository.as_ref(), caller)).await?;
        Ok(host)
    }
}
