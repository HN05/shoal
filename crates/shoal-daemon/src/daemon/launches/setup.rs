//! A repository's setup, as `shoal repo setup` launches it: the one launch
//! whose agent starts in the setup mode, which administers the repository.
use anyhow::{Context as _, Result};
use shoal_protocol::launches::Launch;

use super::ManagerReads;
use crate::{
    config::permissions::PermissionKey,
    daemon::{permissions::Action, scope::Caller, workspace::Manager},
    launches,
};

impl Manager {
    /// Let the scoped `caller` start the setup `launch` asks for in its
    /// repository as its `admin` permission allows; any other launch, and
    /// the user's, needs nothing here.
    pub(super) async fn authorize_repository_setup(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let (true, Some(caller)) = (launch.repository_setup, caller) else {
            return Ok(());
        };
        let reads = ManagerReads::new(self, Some(caller));
        let repo = (launches::shape::launch_repository(&reads, launch).await?)
            .context("a repository's setup names its repository")?;
        let action = Action::new(
            PermissionKey::Admin,
            format!("shoal repo setup {}", repo.id),
        );
        (self.authorize_action(Some(caller), action.in_repository(repo.id))).await
    }
}

#[cfg(test)]
mod tests;
