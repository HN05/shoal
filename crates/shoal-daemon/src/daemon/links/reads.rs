//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::Result;
use shoal_links::place::Access;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::ForgeRepo,
    protocol::{ItemContext, RepoTarget},
};

mod items;
mod repository;

impl Manager {
    /// The forge of the repository `context` reads in.
    pub(in crate::daemon::links) async fn context_forge(
        &self,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.links.place(self, context, Access::Read).await?;
        place.forge(self).await
    }

    /// The forge of the `target` repository, which `caller` reads.
    pub(in crate::daemon::links) async fn read_forge(
        &self,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<ForgeRepo> {
        let target = self
            .links
            .forge_target(self, target, None, Access::Read, caller);
        target.await?.into_forge()
    }
}

#[cfg(test)]
mod tests;
