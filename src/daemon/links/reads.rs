//! What the daemon reads from a forge for a space or a registered checkout,
//! so a scoped caller reads as its agent account and anyone else with the
//! user's own logins.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{
        ForgeRepo, account::Account, conventions::labels::Label, items::link::ItemKind,
        review::reviewers::Reviewer,
    },
    protocol::ItemContext,
};

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

    /// The forge of the repository `context` runs in, with a workspace's
    /// worktree verified.
    pub(in crate::daemon::links) async fn context_forge(
        &self,
        context: &ItemContext,
    ) -> Result<ForgeRepo> {
        let place = self.place(context).await?;
        self.place_forge(&place).await
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
        let place = self.place(context).await?;
        let input = self.named_or_linked(item, &place, ItemKind::Pr).await?;
        let forge = self.place_forge(&place).await?;
        let (number, _) = forge.pull(&input)?;
        forge.reviewers(&self.read_account(scoped), number).await
    }
}
