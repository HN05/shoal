//! Changes to an issue or PR, planned as the REST requests each forge needs
//! and applied in order, stopping at the first failure.
use std::borrow::Cow;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::state::states;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        attach::{Attachment, Holder, Pending},
        item::{Current, Item, ItemState},
        link::ItemKind,
    },
    transport::api::Request,
};

/// Marks a failure after an action's first request reached the forge, such
/// as a comment posted before attaching its files failed, so a caller does
/// not send it again.
#[derive(Debug, Default)]
pub struct Sent {
    /// The activity key of the comment or reply the action posted.
    pub authored: Option<String>,
}

impl std::fmt::Display for Sent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the change reached the forge, but what followed it failed")
    }
}

/// `error`, marked as [`Sent`] when the action's first request was sent.
pub fn after_send(error: anyhow::Error, sent: bool) -> anyhow::Error {
    if sent {
        error.context(Sent::default())
    } else {
        error
    }
}

/// `error`, marked as [`Sent`] with what the action posted once its first
/// request was sent.
fn after_post(error: anyhow::Error, sent: bool, authored: &Option<String>) -> anyhow::Error {
    if !sent {
        return error;
    }
    error.context(Sent {
        authored: authored.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Edit(Edit),
    Comment {
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    Close,
    Reopen,
    Merge {
        method: MergeMethod,
        delete_branch: bool,
    },
    Thread(ThreadChange),
}

/// Fields left empty stay as they are; base, reviewers and draft state are
/// PR-only. Attachments go into the new description, or after the current
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub add_reviewers: Vec<String>,
    pub remove_reviewers: Vec<String>,
    pub draft: Option<bool>,
    pub attachments: Vec<Attachment>,
}

/// A reply in the review thread of a PR's inline comment, a change to the
/// thread's resolution, or both, replying first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadChange {
    /// Any comment of the thread.
    pub comment: u64,
    pub reply: Option<String>,
    pub resolve: Option<bool>,
}

states!(MergeMethod: ValueEnum {
    Merge => "merge",
    Rebase => "rebase",
    Squash => "squash",
});

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Edit(_) => "edit",
            Self::Comment { .. } => "comment",
            Self::Close => "close",
            Self::Reopen => "reopen",
            Self::Merge { .. } => "merge",
            Self::Thread(change) => match change.resolve {
                Some(true) => "resolve",
                Some(false) => "unresolve",
                None => "reply",
            },
        }
    }

    /// The files the action attaches.
    pub fn attachments(&self) -> &[Attachment] {
        match self {
            Self::Edit(edit) => &edit.attachments,
            Self::Comment { attachments, .. } => attachments,
            _ => &[],
        }
    }

    /// The capabilities the forge needs for the action on an item of `kind`,
    /// besides attachments, which uploads check.
    pub fn needs(&self, kind: ItemKind) -> Vec<Capability> {
        match self {
            Self::Edit(edit) => {
                let labels = (!edit.add_labels.is_empty() || !edit.remove_labels.is_empty())
                    .then_some(Capability::Labels);
                let draft = edit.draft.map(|_| Capability::DraftPrs);
                labels.into_iter().chain(draft).collect()
            }
            Self::Reopen if kind == ItemKind::Pr => vec![Capability::ReopenPrs],
            Self::Merge { method, .. } => vec![Capability::merge(*method)],
            Self::Thread(change) if change.resolve.is_some() => vec![Capability::ResolveThreads],
            _ => Vec::new(),
        }
    }

    /// Refuse what no forge can do with an item of `kind`.
    pub fn validate(&self, kind: ItemKind) -> Result<()> {
        let pr_only = |what: &str| -> Result<()> {
            ensure!(kind == ItemKind::Pr, "issues have no {what}");
            Ok(())
        };
        match self {
            Self::Edit(edit) => {
                ensure!(
                    *edit != Edit::default(),
                    "nothing to change; pass a new title, description, attachment, base, label, reviewer or draft state"
                );
                if edit.base.is_some() {
                    pr_only("base branch")?;
                }
                if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
                    pr_only("reviewers")?;
                }
                if edit.draft.is_some() {
                    pr_only("draft state")?;
                }
            }
            Self::Comment { body, attachments } => ensure!(
                !body.trim().is_empty() || !attachments.is_empty(),
                "the comment is empty"
            ),
            Self::Merge { .. } => pr_only("merge")?,
            Self::Thread(change) => {
                pr_only("review threads")?;
                ensure!(
                    change.reply.is_some() || change.resolve.is_some(),
                    "nothing to change; pass a reply or a resolution"
                );
                if let Some(reply) = &change.reply {
                    ensure!(!reply.trim().is_empty(), "the reply is empty");
                }
            }
            Self::Close | Self::Reopen => {}
        }
        Ok(())
    }

    /// Refuse an edit whose labels `item`, as the forge reports it afterwards,
    /// does not show: a forge can leave out an unknown label, or one of two
    /// exclusive ones, without an error. `fold_case` is whether names that
    /// differ only in case name the same label.
    fn ensure_applied(&self, item: &Item, fold_case: bool) -> Result<()> {
        let Self::Edit(edit) = self else {
            return Ok(());
        };
        let has = |label: &String| {
            item.labels
                .iter()
                .any(|name| name == label || (fold_case && name.eq_ignore_ascii_case(label)))
        };
        // The labels of `labels` that `item` lacks when `added`, or has otherwise.
        let unapplied = |labels: &[String], added: bool| -> String {
            let labels: Vec<_> = labels
                .iter()
                .filter(|label| has(label) != added)
                .map(String::as_str)
                .collect();
            labels.join(", ")
        };
        let problems: Vec<_> = [
            ("add", unapplied(&edit.add_labels, true)),
            ("remove", unapplied(&edit.remove_labels, false)),
        ]
        .into_iter()
        .filter(|(_, labels)| !labels.is_empty())
        .map(|(verb, labels)| format!("did not {verb} {labels}"))
        .collect();
        if problems.is_empty() {
            return Ok(());
        }
        let labels = match item.labels.as_slice() {
            [] => "no labels".to_owned(),
            labels => format!("the labels {}", labels.join(", ")),
        };
        anyhow::bail!(
            "the forge {} on #{}, which has {labels}",
            problems.join(" and "),
            item.number
        )
    }
}

/// What an action left on the forge.
pub struct Acted {
    /// The item as the forge reports it afterwards.
    pub item: Item,
    /// The activity key of the comment or review reply the action posted.
    pub authored: Option<String>,
}

impl ForgeRepo {
    /// Apply `action` as `account` and return the item as the forge reports it
    /// afterwards.
    pub(crate) async fn act(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        action: &Action,
    ) -> Result<Item> {
        Ok(self.apply(account, kind, number, action).await?.item)
    }

    /// [`Self::act`], also naming the comment or reply the action posted.
    pub async fn apply(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        action: &Action,
    ) -> Result<Acted> {
        action.validate(kind)?;
        // Refused before anything is read or posted.
        self.require_all(action.needs(kind))?;
        let per_item = self.check_uploads(account, action.attachments()).await?;
        let current = self.read(account, kind, number).await?;
        let (action, pending) = self
            .upload_attachments(account, &current, action, per_item)
            .await?;
        let mut authored = None;
        let mut first = None;
        for request in self.requests(account, current, &action).await? {
            let response = self
                .send(account, &request)
                .await
                .map_err(|error| after_post(error, first.is_some(), &authored))?;
            // A comment, or a thread's reply, is the action's first request.
            if first.is_none() {
                authored = self.posted(&action, &response);
                first = Some(response);
            }
        }
        if let (Some(pending), Some(comment)) = (pending, &first) {
            self.attach_to_comment(account, comment, pending)
                .await
                .context("the comment was posted, but attaching its files failed")
                .map_err(|error| after_post(error, first.is_some(), &authored))?;
        }
        let item = self
            .read(account, kind, number)
            .await
            .map_err(|error| after_post(error, first.is_some(), &authored))?
            .item;
        action
            .ensure_applied(&item, self.kind.label_names_fold_case())
            .map_err(|error| after_post(error, first.is_some(), &authored))?;
        Ok(Acted { item, authored })
    }

    /// `action` with its attachments uploaded and embedded in its body, and
    /// the files of a comment that must exist before the forge takes them;
    /// `per_item` is whether the forge keeps them with an item or comment.
    async fn upload_attachments<'a>(
        &self,
        account: &Account,
        current: &Current,
        action: &'a Action,
        per_item: bool,
    ) -> Result<(Cow<'a, Action>, Option<Pending>)> {
        match action {
            Action::Edit(edit) if !edit.attachments.is_empty() => {
                let holder = if per_item {
                    Holder::Item(current.item.number)
                } else {
                    Holder::Repository
                };
                let body = edit.body.as_deref().unwrap_or(&current.body);
                let body = self
                    .attach(account, holder, body, &edit.attachments)
                    .await?;
                let edit = Edit {
                    body: Some(body),
                    attachments: Vec::new(),
                    ..edit.clone()
                };
                Ok((Cow::Owned(Action::Edit(edit)), None))
            }
            Action::Comment { body, attachments } if !attachments.is_empty() => {
                let (body, pending) = self.new_body(account, body, attachments, per_item).await?;
                let comment = Action::Comment {
                    body,
                    attachments: Vec::new(),
                };
                Ok((Cow::Owned(comment), pending))
            }
            _ => Ok((Cow::Borrowed(action), None)),
        }
    }

    /// Upload a posted comment's files to it and embed them in its body.
    async fn attach_to_comment(
        &self,
        account: &Account,
        comment: &serde_json::Value,
        pending: Pending,
    ) -> Result<()> {
        let id = comment_id(comment)?;
        let holder = Holder::Comment(id);
        let body = self
            .attach(account, holder, &pending.body, &pending.attachments)
            .await?;
        let edit = self.kind.edit_comment(self, id, &body)?.repeatable();
        self.send(account, &edit).await?;
        Ok(())
    }

    /// The activity key of what `action` posted, from its response: the
    /// entry the forge's activity records it under. Without an ID the post is
    /// reported back like anyone else's, rather than failing a post that
    /// succeeded.
    fn posted(&self, action: &Action, response: &serde_json::Value) -> Option<String> {
        let reply = matches!(action, Action::Thread(change) if change.reply.is_some());
        let (source, field) = match action {
            Action::Comment { .. } => self.kind.comment_key(),
            _ if reply => self.kind.reply_key(),
            _ => return None,
        };
        response
            .get(field)
            .filter(|id| id.is_string() || id.is_number())
            .map(|id| crate::updates::activity_key(source, id))
    }

    /// The requests that carry out `action`, after reading what it needs
    /// beyond the item.
    async fn requests(
        &self,
        account: &Account,
        mut current: Current,
        action: &Action,
    ) -> Result<Vec<Request>> {
        match action {
            Action::Thread(change) => {
                let number = current.item.number;
                let thread = self.thread(account, number, change.comment).await?;
                self.plan_thread(number, &thread, change)
            }
            Action::Edit(edit) => {
                self.kind
                    .prepare_edit(self, account, edit, &mut current)
                    .await?;
                self.plan(&current, action)
            }
            _ => self.plan(&current, action),
        }
    }

    pub async fn read(&self, account: &Account, kind: ItemKind, number: u64) -> Result<Current> {
        let value = self
            .send(account, &Request::get(self.item_endpoint(kind, number)))
            .await?;
        self.current(kind, number, &value)
    }

    /// Whether `number` names an issue or a PR. Only forges that number both
    /// in one sequence can tell; elsewhere the caller names the kind.
    pub async fn item_kind(&self, account: &Account, number: u64) -> Result<ItemKind> {
        ensure!(
            self.kind.numbers_items_together(),
            "this forge numbers issues and PRs separately; put issue or pr before #{number}"
        );
        let value = self
            .send(
                account,
                &Request::get(self.item_endpoint(ItemKind::Issue, number)),
            )
            .await?;
        crate::items::item::issue_read_kind(number, &value)
    }

    /// The requests that carry out `action` on `current`, in order.
    pub(crate) fn plan(&self, current: &Current, action: &Action) -> Result<Vec<Request>> {
        let item = &current.item;
        Ok(match action {
            Action::Edit(edit) => self.kind.edit(self, current, edit)?,
            Action::Comment { body, .. } => vec![self.kind.comment(self, item, body)],
            Action::Close => {
                ensure!(
                    item.state == ItemState::Open,
                    "#{} is not open",
                    item.number
                );
                vec![self.kind.set_state(self, item, ItemState::Closed)?]
            }
            Action::Reopen => {
                ensure!(
                    item.state == ItemState::Closed,
                    "#{} is not closed, or was merged",
                    item.number
                );
                vec![self.kind.set_state(self, item, ItemState::Open)?]
            }
            Action::Merge {
                method,
                delete_branch,
            } => {
                ensure!(
                    item.state == ItemState::Open,
                    "#{} is not open",
                    item.number
                );
                self.kind.merge(self, current, *method, *delete_branch)?
            }
            Action::Thread(_) => anyhow::bail!("a review thread change is planned from its thread"),
        })
    }
}

/// The ID of the comment a post's response describes.
fn comment_id(response: &serde_json::Value) -> Result<u64> {
    response["id"]
        .as_u64()
        .context("the forge did not report the comment's ID")
}

#[cfg(test)]
mod tests;
