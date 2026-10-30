//! The comments of an issue or PR: finding one among its discussion and
//! inline comments, and replacing the body of one the acting login wrote.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    items::{action::Acted, link::ItemKind, reaction::Subject},
    review::view::Details,
};

/// A new body for a comment or inline comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentEdit {
    /// The ID of a comment or inline comment, as `shoal view` shows it.
    pub comment: u64,
    pub body: String,
}

impl CommentEdit {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(!self.body.trim().is_empty(), "the comment is empty");
        Ok(())
    }
}

/// A comment of an item, and who wrote it.
pub(crate) struct Found {
    pub subject: Subject,
    pub author: String,
}

impl ForgeRepo {
    /// Comment `comment` of item `number`, found among the item's comments
    /// and inline comments, which forges number apart or let a comment
    /// endpoint reach another item's.
    pub(crate) async fn find_comment(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comment: u64,
    ) -> Result<Found> {
        let mut details = self
            .kind
            .view_details(self, account, kind, number, true)
            .await?;
        let mut errors = Vec::new();
        details.reviews = self
            .kind
            .view_discussion(self, account, kind, number, &mut details, &mut errors)
            .await?;
        let found = comments(&details).find(|(subject, _)| {
            matches!(subject, Subject::Comment(id) | Subject::ReviewComment(id) if *id == comment)
        });
        match (found, errors.as_slice()) {
            (Some((subject, author)), _) => Ok(Found {
                subject,
                author: author.to_owned(),
            }),
            (None, []) => bail!("#{number} has no comment {comment}"),
            (None, errors) => bail!(
                "#{number} has no comment {comment} that Shoal could read: {}",
                errors.join("; ")
            ),
        }
    }

    /// Replace the body of a comment of item `number` that the login of
    /// `account` wrote, and return the item, which the edit does not
    /// change, with the comment's activity key. The item, the comment and
    /// its author are checked before anything is sent.
    pub(crate) async fn edit_own_comment(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        edit: &CommentEdit,
    ) -> Result<Acted> {
        let item = self.read(account, kind, number).await?.item;
        let found = self
            .find_comment(account, kind, number, edit.comment)
            .await?;
        self.ensure_own(account, &found, edit.comment).await?;
        let request = self
            .kind
            .edit_comment(self, kind, number, found.subject, &edit.body)?
            .repeatable();
        let response = self.send(account, &request).await?;
        let key = match found.subject {
            Subject::ReviewComment(_) => self.kind.reply_key(),
            _ => self.kind.comment_key(),
        };
        Ok(Acted {
            item,
            authored: crate::updates::posted_key(key, &response)
                .into_iter()
                .collect(),
        })
    }

    /// Refuse a comment that the login of `account` did not write, which
    /// an account with the forge's admin rights could otherwise change.
    async fn ensure_own(&self, account: &Account, found: &Found, comment: u64) -> Result<()> {
        let login = (self.kind.own_login(self, account).await?)
            .context("editing a comment needs a login on the forge")?;
        ensure!(
            found.author.eq_ignore_ascii_case(&login),
            "comment {comment} was written by {}, not by {login}, the login Shoal acts as",
            found.author
        );
        Ok(())
    }
}

/// The comments and inline comments of `details`, in that order, with
/// their authors.
pub(crate) fn comments(details: &Details) -> impl Iterator<Item = (Subject, &str)> {
    let comments = details
        .comments
        .iter()
        .flatten()
        .map(|comment| (Subject::Comment(comment.id), comment.author.as_str()));
    let review_comments = details
        .reviews
        .iter()
        .flatten()
        .flat_map(|review| &review.comments)
        .map(|comment| (Subject::ReviewComment(comment.id), comment.author.as_str()));
    comments.chain(review_comments)
}

#[cfg(test)]
mod tests;
