//! Who a PR can ask for a review.
use anyhow::Result;

use crate::{
    items::{item::ItemRequests, review::Reviews},
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    items::{link::ItemKind, review::reviewers::Reviewer},
    numbers::PullNumber,
};

/// A reviewer name that names a team, split into its organization and team;
/// a username names no team.
pub(crate) fn team_name(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
}

/// Listing who can review a PR.
#[shoal_forge_derive::extension(pub ReviewerRequests)]
impl ForgeRepo {
    /// The users and teams PR `number` can ask for a review, in the forge's
    /// order, leaving out its author.
    pub async fn reviewers(&self, account: &Account, number: PullNumber) -> Result<Vec<Reviewer>> {
        // Boxed so each forge's user and team reads stay off the caller's stack.
        let kind = self.kind();
        let (pr, candidates) = tokio::try_join!(
            self.read(account, ItemKind::Pr, number.get()),
            Box::pin(kind.reviewers(self, account)),
        )?;
        Ok(candidates
            .into_iter()
            .filter(|reviewer| reviewer.team || pr.author.as_ref() != Some(&reviewer.name))
            .collect())
    }
}

#[cfg(test)]
mod tests;
