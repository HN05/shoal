//! Changes to an issue or PR, planned as the REST requests each forge needs
//! and applied in order, stopping at the first failure.
use std::borrow::Cow;

use anyhow::{Context, Result, ensure};
use shoal_refs::CommitId;

use crate::{
    backends::{Conventions, Items, Merging},
    capability::CapabilityRequests,
    conventions::labels::LabelRequests,
    items::{
        attach::{AttachRequests, AttachSteps, Holder, Pending},
        comment::CommentRequests,
        item::ItemRequests,
        milestone::MilestoneRequests,
        reaction::{ReactionRequests, Subject},
        review::SubmitSteps,
    },
    review::thread::{ThreadRequests, ThreadSteps},
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
    updates::{Edited, ItemDetails},
};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        action::{Action, Edit, MergeMethod, same_label},
        item::{Current, Item, ItemState},
        link::ItemKind,
    },
    numbers::{CommentId, PullNumber},
};

/// Marks a failure after an action's first request reached the forge, such
/// as a comment posted before attaching its files failed, so a caller does
/// not send it again.
#[derive(Debug, Default)]
pub struct Sent {
    /// The activity keys of the comments, replies or reviews the action
    /// posted.
    pub authored: Vec<String>,
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
pub(crate) fn after_post(error: anyhow::Error, sent: bool, authored: &[String]) -> anyhow::Error {
    if !sent {
        return error;
    }
    error.context(Sent {
        authored: authored.to_vec(),
    })
}

/// What an action's requests left on the forge.
#[derive(Default)]
pub(crate) struct Receipt {
    /// The response to the first request, such as the comment it posted;
    /// none until a request reached the forge.
    first: Option<serde_json::Value>,
    /// The activity keys of the comment or reply the first request posted.
    authored: Vec<String>,
}

impl Receipt {
    /// `error`, marked as [`Sent`] with what the action posted once a
    /// request reached the forge.
    fn failed(&self, error: anyhow::Error) -> anyhow::Error {
        after_post(error, self.first.is_some(), &self.authored)
    }
}

/// What an action left on the forge.
pub struct Acted {
    /// The item as the forge reports it afterwards.
    pub item: Item,
    /// The activity keys of the comments, replies or reviews the action
    /// posted.
    pub authored: Vec<String>,
    /// What the action changed of the item's title, labels and other
    /// details.
    pub edited: Edited,
}

/// Acting on issues and PRs.
#[shoal_forge_derive::extension(pub ActionRequests, pub(crate) ActionSteps)]
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
        match action {
            Action::Review(review) => {
                return self
                    .submit_review(account, PullNumber::new(number), review)
                    .await;
            }
            // Boxed so finding a comment among the item's discussion does not
            // grow the future of every action.
            Action::React(change) => {
                return Box::pin(self.react(account, kind, number, change)).await;
            }
            Action::EditComment(edit) => {
                return Box::pin(self.edit_own_comment(account, kind, number, edit, per_item))
                    .await;
            }
            _ => {}
        }
        let current = self.read(account, kind, number).await?;
        let before = ItemDetails::of(&current);
        let (action, pending) = self
            .upload_attachments(account, &current, action, per_item)
            .await?;
        let requests = self.requests(account, current, &action).await?;
        let receipt = self.execute(account, &action, requests).await?;
        let after = self
            .settle(account, kind, number, &action, pending, &receipt)
            .await
            .map_err(|error| receipt.failed(error))?;
        let edited = Edited {
            before,
            after: ItemDetails::of(&after),
        };
        Ok(Acted {
            item: after.item,
            authored: receipt.authored,
            edited,
        })
    }

    /// Send `requests`, which carry out `action`, in order, stopping at the
    /// first failure, which is marked as [`Sent`] once a request reached the
    /// forge.
    async fn execute(
        &self,
        account: &Account,
        action: &Action,
        requests: Vec<Request>,
    ) -> Result<Receipt> {
        let mut receipt = Receipt::default();
        for request in requests {
            let response = self
                .send(account, &request)
                .await
                .map_err(|error| receipt.failed(error))?;
            // A comment, or a thread's reply, is the action's first request.
            if receipt.first.is_none() {
                receipt.authored.extend(self.posted(action, &response));
                receipt.first = Some(response);
            }
        }
        Ok(receipt)
    }

    /// Attach the files `pending` holds to the comment `receipt` posted on
    /// item `number`, then read the item and refuse it when it does not show
    /// `action`.
    async fn settle(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        action: &Action,
        pending: Option<Pending>,
        receipt: &Receipt,
    ) -> Result<Current> {
        if let (Some(pending), Some(comment)) = (pending, &receipt.first) {
            self.attach_to_comment(account, kind, number, comment, pending)
                .await
                .context("the comment was posted, but attaching its files failed")?;
        }
        let current = self.read(account, kind, number).await?;
        self.ensure_applied(account, action, &current.item).await?;
        Ok(current)
    }

    /// Refuse `item` when it does not show `action`, naming the labels the
    /// action failed to add that the repository does not have.
    async fn ensure_applied(&self, account: &Account, action: &Action, item: &Item) -> Result<()> {
        let fold_case = self.kind().label_names_fold_case();
        let Err(error) = action.ensure_applied(item, fold_case) else {
            return Ok(());
        };
        let unadded = action.unadded_labels(item, fold_case);
        let unknown = self.unknown_labels(account, unadded, fold_case).await;
        if unknown.is_empty() {
            return Err(error);
        }
        Err(error.context(format!(
            "the repository has no label {}; shoal repo labels lists its labels",
            unknown.join(", ")
        )))
    }

    /// The `names` no label of the repository has; none when its labels
    /// cannot be read, since the refusal stands without them.
    async fn unknown_labels<'a>(
        &self,
        account: &Account,
        names: Vec<&'a str>,
        fold_case: bool,
    ) -> Vec<&'a str> {
        if names.is_empty() {
            return names;
        }
        let Ok(labels) = self.labels(account).await else {
            return Vec::new();
        };
        names
            .into_iter()
            .filter(|name| {
                !labels
                    .iter()
                    .any(|label| same_label(&label.name, name, fold_case))
            })
            .collect()
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
                    ..Edit::clone(edit)
                };
                Ok((Cow::Owned(Action::edit(edit)), None))
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

    /// Upload the files of a comment posted on item `number` to it and
    /// embed them in its body.
    async fn attach_to_comment(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comment: &serde_json::Value,
        pending: Pending,
    ) -> Result<()> {
        let id = comment_id(comment)?;
        let holder = Holder::Comment(id);
        let body = self
            .attach(account, holder, &pending.body, &pending.attachments)
            .await?;
        let edit = self
            .kind()
            .edit_comment(self, kind, number, Subject::Comment(id), &body)?
            .repeatable();
        self.send(account, &edit).await?;
        Ok(())
    }

    /// The activity key of what `action` posted, from its response: the
    /// entry the forge's activity records it under. Without an ID the post is
    /// reported back like anyone else's, rather than failing a post that
    /// succeeded.
    fn posted(&self, action: &Action, response: &serde_json::Value) -> Option<String> {
        let reply = matches!(action, Action::Thread(change) if change.reply.is_some());
        let key = match action {
            Action::Comment { .. } => self.kind().comment_key(),
            _ if reply => self.kind().reply_key(),
            _ => return None,
        };
        crate::updates::posted_key(key, response)
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
                let number = PullNumber::new(current.item.number);
                let thread = self.thread(account, number, change.comment).await?;
                self.plan_thread(number, &thread, change)
            }
            Action::Edit(edit) => {
                if let Some(title) = edit.milestone_title() {
                    current.milestone_id = Some(self.milestone_id(account, title).await?);
                }
                self.kind()
                    .prepare_edit(self, account, edit, &mut current)
                    .await?;
                self.plan(&current, action)
            }
            Action::Merge { method: None, .. } | Action::AutoMerge { method: None, .. } => {
                self.kind()
                    .prepare_merge(self, account, &mut current)
                    .await?;
                self.plan(&current, action)
            }
            _ => self.plan(&current, action),
        }
    }

    pub async fn read(&self, account: &Account, kind: ItemKind, number: u64) -> Result<Current> {
        let value = self.read_value(account, kind, number).await?;
        self.current(kind, number, &value)
    }

    /// The item as the forge reports it, for the fields `Current` leaves out.
    pub(crate) async fn read_value(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<serde_json::Value> {
        self.send(account, &Request::get(self.item_endpoint(kind, number)))
            .await
    }

    /// Whether `number` names an issue or a PR. Only forges that number both
    /// in one sequence can tell; elsewhere the caller names the kind.
    pub async fn item_kind(&self, account: &Account, number: u64) -> Result<ItemKind> {
        ensure!(
            self.kind().numbers_items_together(),
            "this forge numbers issues and PRs separately; put issue or pr before #{number}"
        );
        let value = self.read_value(account, ItemKind::Issue, number).await?;
        crate::items::item::issue_read_kind(number, &value)
    }

    /// The requests that carry out `action` on `current`, in order.
    pub(crate) fn plan(&self, current: &Current, action: &Action) -> Result<Vec<Request>> {
        let item = &current.item;
        Ok(match action {
            Action::Edit(edit) => self.kind().edit(self, current, edit)?,
            Action::Comment { body, .. } => vec![self.kind().comment(self, item, body)],
            Action::Close => {
                ensure_open(item)?;
                vec![self.kind().set_state(self, item, ItemState::Closed)?]
            }
            Action::Reopen => {
                ensure!(
                    item.state == ItemState::Closed,
                    "#{} is not closed, or was merged",
                    item.number
                );
                vec![self.kind().set_state(self, item, ItemState::Open)?]
            }
            Action::Merge {
                method,
                message,
                delete_branch,
                head,
            } => {
                ensure_open(item)?;
                ensure_merge_head(current, head.as_ref())?;
                let message = squash_message(current, *method, message.as_deref())?;
                (self.kind()).merge(self, current, *method, message, *delete_branch)?
            }
            Action::AutoMerge {
                method,
                message,
                delete_branch,
                head,
            } => {
                ensure_open(item)?;
                ensure_merge_head(current, head.as_ref())?;
                let message = squash_message(current, *method, message.as_deref())?;
                (self.kind()).auto_merge(self, current, *method, message, *delete_branch)?
            }
            Action::CancelAutoMerge => {
                ensure_open(item)?;
                self.kind().cancel_auto_merge(self, current)?
            }
            Action::Update { method } => {
                ensure_open(item)?;
                self.kind().update_branch(self, current, *method)?
            }
            Action::Thread(_) => anyhow::bail!("a review thread change is planned from its thread"),
            Action::Review(_) => anyhow::bail!("a review is planned from the PR's diff"),
            Action::React(_) => {
                anyhow::bail!("a reaction is planned from the reactions it changes")
            }
            Action::EditComment(_) => anyhow::bail!("a comment's edit is planned from the comment"),
        })
    }
}

/// Refuse to merge the PR `current` once its head moved from `head`, the
/// commit the merge is for; the forge then gets the head it read, which is
/// that commit.
fn ensure_merge_head(current: &Current, head: Option<&CommitId>) -> Result<()> {
    let Some(head) = head else {
        return Ok(());
    };
    let read = current.read_head()?;
    // Bitbucket reports a PR's head as an abbreviated hash; a bound head
    // shorter than the one read would name more than one commit.
    ensure!(
        head.as_str().starts_with(read),
        "the head of #{} moved from {head} to {read} since the merge was asked for; run shoal merge again",
        current.item.number
    );
    Ok(())
}

/// `message`, refused for a merge of the PR `current` by `method` that does
/// not squash, or whose method the forge does not tell.
fn squash_message<'a>(
    current: &Current,
    method: Option<MergeMethod>,
    message: Option<&'a str>,
) -> Result<Option<&'a str>> {
    if message.is_some() {
        ensure!(
            current.merge_style(method).ok() == Some(MergeMethod::Squash.as_str()),
            "a merge message is a squash merge's commit message; pass -X squash"
        );
    }
    Ok(message)
}

fn ensure_open(item: &Item) -> Result<()> {
    ensure!(
        item.state == ItemState::Open,
        "#{} is not open",
        item.number
    );
    Ok(())
}

/// The ID of the comment a post's response describes.
fn comment_id(response: &serde_json::Value) -> Result<CommentId> {
    response["id"]
        .as_u64()
        .map(CommentId::new)
        .context("the forge did not report the comment's ID")
}

#[cfg(test)]
mod tests;
