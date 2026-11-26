//! Checking a launch's new chat before a start's pickers: settled as its
//! start settles it, with an issue a picker has yet to choose left open.
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
    /// Settles as [`Self::chat_placement`] does. Returns the workspace whose
    /// worktree the chat runs in, if any.
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
