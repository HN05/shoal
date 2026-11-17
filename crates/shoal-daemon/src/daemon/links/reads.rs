//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::Result;

use super::place::Access;
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    forge::ForgeRepo,
    protocol::{ItemContext, RepoTarget},
};
use shoal_forge::account::Account;

mod items;
mod repository;

impl Manager {
    /// The account `caller`'s read goes out as: the agent account for a
    /// scoped caller and the user's own logins for the user.
    pub(in crate::daemon::links) fn read_account(&self, caller: Option<&Caller>) -> Account {
        match caller {
            Some(_) => self.agent_account(),
            None => self.user_account(),
        }
    }

    /// The forge of the repository `context` reads in.
    pub(in crate::daemon::links) async fn context_forge(
        &self,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.place(context, Access::Read).await?;
        self.place_forge(&place).await
    }

    /// The forge of the `target` repository, which `caller` reads.
    pub(in crate::daemon::links) async fn read_forge(
        &self,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<ForgeRepo> {
        let target = self.forge_target(target, None, Access::Read, caller);
        Ok(target.await?.forge)
    }
}

#[cfg(test)]
mod tests;
