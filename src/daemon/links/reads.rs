//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::{Result, bail};

use super::{actions::NOTHING_LINKED, space::LinkSpace};
use crate::{
    daemon::workspace::Manager,
    forge::{ForgeRepo, account::Account, items::link::ItemKind},
    model::Repository,
    protocol::ItemContext,
};

mod items;
mod repository;

/// Where a read runs: a space, whose linked items it may default to, or a
/// registered checkout.
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read by the request that made it"
)]
pub(in crate::daemon::links) enum ReadPlace {
    Space(LinkSpace),
    Checkout(Repository),
}

impl Manager {
    /// The account a read goes out as: the agent account for a `scoped`
    /// caller and the user's own logins otherwise.
    pub(in crate::daemon::links) fn read_account(&self, scoped: bool) -> Account {
        if scoped {
            self.agent_account()
        } else {
            self.user_account()
        }
    }

    /// Where `context` reads, with a workspace's worktree verified. A
    /// stopped chat still reads its links in its repository's checkout.
    pub(in crate::daemon::links) async fn read_place(
        &self,
        context: &ItemContext,
    ) -> Result<ReadPlace> {
        match context {
            ItemContext::Workspace(selector) | ItemContext::Chat(selector) => {
                let space = self.link_space(selector).await?;
                self.verify_space(&space).await?;
                Ok(ReadPlace::Space(space))
            }
            ItemContext::Checkout(selector) => {
                Ok(ReadPlace::Checkout(self.repository(selector).await?))
            }
        }
    }

    /// The forge of the repository `place` reads in.
    pub(in crate::daemon::links) async fn read_place_forge(
        &self,
        place: &ReadPlace,
    ) -> Result<ForgeRepo> {
        match place {
            ReadPlace::Space(space) => self.space_forge(space).await,
            ReadPlace::Checkout(checkout) => self.checkout_forge(checkout).await,
        }
    }

    /// The forge of the repository `context` reads in.
    pub(in crate::daemon::links) async fn context_forge(
        &self,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.read_place(context).await?;
        self.read_place_forge(&place).await
    }

    /// The forge of the repository of `context`, or of the one
    /// `repository` names.
    pub(in crate::daemon::links) async fn repository_forge(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
    ) -> Result<ForgeRepo> {
        let own = self.context_forge(context).await?;
        self.named_forge(own, repository).await
    }

    /// The item `item` names where `place` reads, or the space's linked
    /// one of `kind`.
    pub(in crate::daemon::links) async fn place_item(
        &self,
        item: Option<String>,
        place: &ReadPlace,
        kind: ItemKind,
    ) -> Result<String> {
        match (item, place) {
            (Some(item), _) => Ok(item),
            (None, ReadPlace::Space(space)) => self.linked_item(space.id(), kind).await,
            (None, ReadPlace::Checkout(_)) => bail!(NOTHING_LINKED),
        }
    }
}

#[cfg(test)]
mod tests;
