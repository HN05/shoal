//! Reactions on an issue, a PR or one of their comments, read for item
//! views.
use anyhow::Result;
use futures_util::{StreamExt as _, TryStreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo, account::Account, backends::Backend, items::link::ItemKind, review::view::Details,
};

/// Reaction lists read at once for a view.
const CONCURRENT_READS: usize = 8;

/// The reactions on an item or comment with one content, as a view lists
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// GitHub's name for it, such as `+1` or `heart`; GitLab's name for an
    /// emoji GitHub has no reaction for.
    pub content: String,
    pub count: u64,
    /// Whether the login the view reads as is among them.
    pub reacted: bool,
}

/// What a reaction is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subject {
    /// The issue or PR itself.
    Item,
    /// A discussion comment.
    Comment(u64),
    /// An inline comment on a PR's diff.
    ReviewComment(u64),
}

/// A reaction as the forge lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    /// The forge's ID for it, where it has one.
    pub id: Value,
    pub content: String,
    pub login: String,
}

impl ForgeRepo {
    /// Fill the reactions of the item `details` describes and of its
    /// comments, as the login of `account` sees them.
    pub(crate) async fn view_reactions(
        &self,
        account: &Account,
        kind: ItemKind,
        details: &mut Details,
    ) -> Result<()> {
        let login = self.kind.own_login(self, account).await?;
        let number = details.number;
        let subjects = subjects(details);
        let reactions: Vec<Vec<Reaction>> = futures_util::stream::iter(&subjects)
            .map(|&subject| {
                let login = login.as_str();
                async move {
                    let listed = self
                        .listed_reactions(account, kind, number, subject)
                        .await?;
                    Ok::<_, anyhow::Error>(tally(&listed, login))
                }
            })
            .buffered(CONCURRENT_READS)
            .try_collect()
            .await?;
        place(details, reactions);
        Ok(())
    }

    /// The reactions on `subject` of item `number`, as the forge lists them.
    pub(crate) async fn listed_reactions(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<Vec<Listed>> {
        let endpoint = self.kind.reactions_endpoint(self, kind, number, subject)?;
        self.pages(account, &endpoint)
            .await?
            .iter()
            .map(|reaction| self.kind.listed_reaction(reaction))
            .collect()
    }
}

/// The item, its comments and its reviews' inline comments, in that order.
fn subjects(details: &Details) -> Vec<Subject> {
    let comments = details.comments.iter().flatten();
    let review_comments = details
        .reviews
        .iter()
        .flatten()
        .flat_map(|review| &review.comments);
    std::iter::once(Subject::Item)
        .chain(comments.map(|comment| Subject::Comment(comment.id)))
        .chain(review_comments.map(|comment| Subject::ReviewComment(comment.id)))
        .collect()
}

/// Give each of [`subjects`] its reactions, in the same order.
fn place(details: &mut Details, reactions: Vec<Vec<Reaction>>) {
    let mut reactions = reactions.into_iter();
    details.reactions = reactions.next().unwrap_or_default();
    let comments = details.comments.iter_mut().flatten();
    for (comment, reactions) in comments.zip(&mut reactions) {
        comment.reactions = reactions;
    }
    let review_comments = details
        .reviews
        .iter_mut()
        .flatten()
        .flat_map(|review| &mut review.comments);
    for (comment, reactions) in review_comments.zip(&mut reactions) {
        comment.reactions = reactions;
    }
}

/// `listed` counted by content, in the order each content was first used,
/// marking those `login` is among.
fn tally(listed: &[Listed], login: &str) -> Vec<Reaction> {
    let mut reactions: Vec<Reaction> = Vec::new();
    for reaction in listed {
        let own = reaction.login.eq_ignore_ascii_case(login);
        match reactions
            .iter_mut()
            .find(|tallied| tallied.content == reaction.content)
        {
            Some(tallied) => {
                tallied.count += 1;
                tallied.reacted |= own;
            }
            None => reactions.push(Reaction {
                content: reaction.content.clone(),
                count: 1,
                reacted: own,
            }),
        }
    }
    reactions
}

#[cfg(test)]
mod tests;
