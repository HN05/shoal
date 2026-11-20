//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_forge_types::ForgeRepo;
use shoal_protocol::protocol::{ItemContext, RepoTarget};

use crate::{Links, daemon::Daemon, place::Access};

mod items;
mod repository;

impl Links {
    /// The forge of the repository `context` reads in.
    pub async fn context_forge(
        &self,
        daemon: &dyn Daemon,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.place(daemon, context, Access::Read).await?;
        place.forge(daemon).await
    }

    /// The forge of the `target` repository, which `caller` reads.
    pub async fn read_forge(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        caller: Option<&Caller>,
    ) -> Result<ForgeRepo> {
        let target = self.forge_target(daemon, target, None, Access::Read, caller);
        target.await?.into_forge()
    }
}
