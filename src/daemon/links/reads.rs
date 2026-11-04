//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::{Result, bail};

use super::{actions::NOTHING_LINKED, space::LinkSpace};
use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo, account::Account, conventions::labels::Label, items::link::ItemKind,
        review::reviewers::Reviewer,
    },
    model::Repository,
    protocol::ItemContext,
};

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

    /// The labels of the repository of `context`, or of the one
    /// `repository` names.
    pub async fn labels(
        &self,
        context: &ItemContext,
        repository: Option<&str>,
        scoped: bool,
    ) -> Result<Vec<Label>> {
        let own = self.context_forge(context).await?;
        let forge = self.named_forge(own, repository).await?;
        forge.labels(&self.read_account(scoped)).await
    }

    /// Who the PR `item` names in the repository of `context`, or the
    /// space's linked PR, can ask for a review.
    pub async fn reviewers(
        &self,
        context: &ItemContext,
        item: Option<String>,
        scoped: bool,
    ) -> Result<Vec<Reviewer>> {
        let place = self.read_place(context).await?;
        let input = match (item, &place) {
            (Some(item), _) => item,
            (None, ReadPlace::Space(space)) => self.linked_item(space.id(), ItemKind::Pr).await?,
            (None, ReadPlace::Checkout(_)) => bail!(NOTHING_LINKED),
        };
        let forge = self.read_place_forge(&place).await?;
        let (number, _) = forge.pull(&input)?;
        forge.reviewers(&self.read_account(scoped), number).await
    }
}

#[cfg(test)]
mod tests;
