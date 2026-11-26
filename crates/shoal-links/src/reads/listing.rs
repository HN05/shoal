//! Listing the issues and PRs of a repository.
use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_forge::items::list::ListRequests;
use shoal_forge_types::items::{item::Item, link::ItemKind, list::Search};
use shoal_protocol::protocol::RepoTarget;

use crate::{Links, daemon::Daemon};

impl Links {
    /// Issues or PRs that `search` matches in the `target` repository, read
    /// as the agent account for a scoped `caller` and as the user's own
    /// logins for the user.
    pub async fn list_items(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        kind: ItemKind,
        search: &Search,
        caller: Option<&Caller>,
    ) -> Result<Vec<Item>> {
        let forge = self.read_forge(daemon, target, caller).await?;
        forge
            .list(&self.forges.read_account(caller), kind, search)
            .await
    }
}
