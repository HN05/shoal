//! Reactions on an issue, a PR or one of their comments: read for item
//! views, and added or removed as the acting account's own.
use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt as _, TryStreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    items::{action::Acted, link::ItemKind},
    review::view::Details,
    transport::api::Request,
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

/// Adding or removing the acting account's reaction on an item, or on one
/// of its comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionChange {
    /// The ID of a comment or inline comment, as `shoal view` shows it.
    pub comment: Option<u64>,
    /// GitHub's name for the reaction, or the forge's own.
    pub content: String,
    pub remove: bool,
}

impl ReactionChange {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.content.is_empty() && !self.content.contains(char::is_whitespace),
            "name the reaction, such as +1 or heart"
        );
        Ok(())
    }
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

impl Listed {
    /// The reaction's numeric ID, which some forges remove it by.
    pub(crate) fn number(&self) -> Result<u64> {
        self.id
            .as_u64()
            .context("the forge did not report the reaction's ID")
    }
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
                let login = login.as_deref();
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

    /// Apply `change` to item `number` as `account`, leaving a reaction that
    /// is already as asked unchanged, and return the item, which a reaction
    /// does not change. The item and comment are checked before anything is
    /// sent.
    pub(crate) async fn react(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        change: &ReactionChange,
    ) -> Result<Acted> {
        let item = self.read(account, kind, number).await?.item;
        let subject = match change.comment {
            Some(id) => self.comment_subject(account, kind, number, id).await?,
            None => Subject::Item,
        };
        let request = self
            .plan_reaction(account, kind, number, subject, change)
            .await?;
        if let Some(request) = &request {
            self.send(account, request).await?;
        }
        Ok(Acted {
            item,
            authored: Vec::new(),
        })
    }

    /// Comment `comment` of item `number` as a reaction's subject, found
    /// among the item's comments and inline comments, which forges number
    /// apart or let a comment endpoint reach another item's.
    async fn comment_subject(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comment: u64,
    ) -> Result<Subject> {
        let mut details = self
            .kind
            .view_details(self, account, kind, number, true)
            .await?;
        let mut errors = Vec::new();
        details.reviews = self
            .kind
            .view_discussion(self, account, kind, number, &mut details, &mut errors)
            .await?;
        let found = subjects(&details).into_iter().find(|subject| {
            matches!(subject, Subject::Comment(id) | Subject::ReviewComment(id) if *id == comment)
        });
        match (found, errors.as_slice()) {
            (Some(subject), _) => Ok(subject),
            (None, []) => bail!("#{number} has no comment {comment}"),
            (None, errors) => bail!(
                "#{number} has no comment {comment} that Shoal could read: {}",
                errors.join("; ")
            ),
        }
    }

    /// The request that makes `change` to the reactions on `subject`, after
    /// reading them; none when the account's reaction is already as asked.
    async fn plan_reaction(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        subject: Subject,
        change: &ReactionChange,
    ) -> Result<Option<Request>> {
        let content = self.kind.reaction_content(&change.content);
        let login = (self.kind.own_login(self, account).await?)
            .context("reacting needs a login on the forge")?;
        let listed = self
            .listed_reactions(account, kind, number, subject)
            .await?;
        let own = listed.iter().find(|reaction| {
            reaction.content == content && reaction.login.eq_ignore_ascii_case(&login)
        });
        let endpoint = self.kind.reactions_endpoint(self, kind, number, subject)?;
        Ok(match (own, change.remove) {
            (None, false) => Some(self.kind.add_reaction(endpoint, &content)?),
            (Some(own), true) => Some(self.kind.remove_reaction(endpoint, own)?),
            (Some(_), false) | (None, true) => None,
        })
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
/// marking those `login`, if any, is among.
fn tally(listed: &[Listed], login: Option<&str>) -> Vec<Reaction> {
    let mut reactions: Vec<Reaction> = Vec::new();
    for reaction in listed {
        let own = login.is_some_and(|login| reaction.login.eq_ignore_ascii_case(login));
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
