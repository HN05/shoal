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
        comment::CommentEdit,
        item::{Current, Item, ItemState},
        link::ItemKind,
        reaction::{ReactionChange, Subject},
    },
    review::submit::{NewReview, ReviewVerdict},
    transport::api::Request,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Boxed, as the largest change by far.
    Edit(Box<Edit>),
    Comment {
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    /// Replace the body of a comment the acting login wrote.
    EditComment(CommentEdit),
    Close,
    Reopen,
    Merge {
        method: MergeMethod,
        delete_branch: bool,
    },
    /// Have the forge merge the PR once its required checks and approvals
    /// pass, or now when they already do.
    AutoMerge {
        method: MergeMethod,
        delete_branch: bool,
    },
    CancelAutoMerge,
    /// Delete the head branch of a merged or closed PR from the repository.
    DeleteBranch,
    /// Bring a PR's branch up to date with its base on the forge.
    Update {
        method: UpdateMethod,
    },
    Thread(ThreadChange),
    Review(NewReview),
    React(ReactionChange),
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
    pub add_assignees: Vec<String>,
    pub remove_assignees: Vec<String>,
    pub milestone: Option<MilestoneChange>,
    pub draft: Option<bool>,
    pub attachments: Vec<Attachment>,
}

/// A new milestone for an item, by its title, or none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneChange {
    Set(String),
    Clear,
}

impl Edit {
    /// Whether the edit changes the item's assignees.
    pub(crate) fn assigns(&self) -> bool {
        !self.add_assignees.is_empty() || !self.remove_assignees.is_empty()
    }

    /// The assignees of `current` after the edit, in order.
    pub(crate) fn assignees_after(&self, current: &[String]) -> Vec<String> {
        changed_set(current, &self.add_assignees, &self.remove_assignees)
    }

    /// The title of the milestone the edit sets, if it sets one.
    pub(crate) fn milestone_title(&self) -> Option<&str> {
        match &self.milestone {
            Some(MilestoneChange::Set(title)) => Some(title),
            _ => None,
        }
    }

    fn ensure_milestone_applied(&self, item: &Item) -> Result<()> {
        let applied = match &self.milestone {
            None => return Ok(()),
            Some(MilestoneChange::Set(title)) => item.milestone.as_ref() == Some(title),
            Some(MilestoneChange::Clear) => item.milestone.is_none(),
        };
        if applied {
            return Ok(());
        }
        let change = match self.milestone_title() {
            Some(title) => format!("set the milestone {title}"),
            None => "clear the milestone".to_owned(),
        };
        let has = match &item.milestone {
            Some(title) => format!("the milestone {title}"),
            None => "no milestone".to_owned(),
        };
        anyhow::bail!(
            "the forge did not {change} on #{}, which has {has}",
            item.number
        )
    }
}

/// The users `current` names without `removed`, then with each of `added`
/// it lacks, keeping the spelling `current` has: logins and account IDs name
/// the same user whatever their case.
pub(crate) fn changed_set(current: &[String], added: &[String], removed: &[String]) -> Vec<String> {
    let names = |set: &[String], name: &str| set.iter().any(|has| has.eq_ignore_ascii_case(name));
    let mut set: Vec<String> = current
        .iter()
        .filter(|name| !names(removed, name))
        .cloned()
        .collect();
    for name in added {
        if !names(&set, name) {
            set.push(name.clone());
        }
    }
    set
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

states!(UpdateMethod: ValueEnum {
    Merge => "merge",
    Rebase => "rebase",
});

impl Action {
    pub fn edit(edit: Edit) -> Self {
        Self::Edit(Box::new(edit))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Edit(_) => "edit",
            Self::Comment { .. } => "comment",
            Self::EditComment(_) => "edit_comment",
            Self::Close => "close",
            Self::Reopen => "reopen",
            Self::Merge { .. } => "merge",
            Self::AutoMerge { .. } => "auto_merge",
            Self::CancelAutoMerge => "cancel_auto_merge",
            Self::DeleteBranch => "delete_branch",
            Self::Update { .. } => "update",
            Self::Thread(change) => match change.resolve {
                Some(true) => "resolve",
                Some(false) => "unresolve",
                None => "reply",
            },
            Self::Review(_) => "review",
            Self::React(change) if change.remove => "unreact",
            Self::React(_) => "react",
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
                let assignees = edit.assigns().then_some(Capability::assignees(kind));
                let milestone = edit
                    .milestone
                    .as_ref()
                    .map(|_| Capability::milestones(kind));
                let draft = edit.draft.map(|_| Capability::DraftPrs);
                labels
                    .into_iter()
                    .chain(assignees)
                    .chain(milestone)
                    .chain(draft)
                    .collect()
            }
            Self::Reopen if kind == ItemKind::Pr => vec![Capability::ReopenPrs],
            Self::Merge { method, .. } => vec![Capability::merge(*method)],
            Self::AutoMerge { method, .. } => {
                vec![Capability::AutoMerge, Capability::merge(*method)]
            }
            Self::CancelAutoMerge => vec![Capability::AutoMerge],
            Self::Update { method } => vec![Capability::update(*method)],
            Self::Thread(change) if change.resolve.is_some() => vec![Capability::ResolveThreads],
            Self::Review(review) => {
                let lines = (!review.comments.is_empty()).then_some(Capability::LineComments);
                let verdict = match review.verdict {
                    ReviewVerdict::Comment => None,
                    ReviewVerdict::Approve => Some(Capability::ApprovePrs),
                    ReviewVerdict::RequestChanges => Some(Capability::RequestChanges),
                };
                lines.into_iter().chain(verdict).collect()
            }
            Self::React(_) => vec![Capability::Reactions],
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
                    **edit != Edit::default(),
                    "nothing to change; pass a new title, description, attachment, base, label, reviewer, assignee, milestone or draft state"
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
            Self::EditComment(edit) => edit.validate()?,
            Self::Merge { .. } | Self::AutoMerge { .. } | Self::CancelAutoMerge => {
                pr_only("merge")?;
            }
            Self::DeleteBranch => pr_only("branch to delete")?,
            Self::Update { .. } => pr_only("branch to update")?,
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
            Self::Review(review) => {
                pr_only("reviews")?;
                review.validate()?;
            }
            Self::React(change) => change.validate()?,
            Self::Close | Self::Reopen => {}
        }
        Ok(())
    }

    /// Refuse an edit whose labels, assignees or milestone `item`, as the
    /// forge reports it afterwards, does not show: a forge can leave out an
    /// unknown label, one of two exclusive ones, or an assignee it cannot
    /// assign, without an error. `fold_case` is whether names that differ
    /// only in case name the same label.
    fn ensure_applied(&self, item: &Item, fold_case: bool) -> Result<()> {
        let Self::Edit(edit) = self else {
            return Ok(());
        };
        let labels = [("add", &edit.add_labels), ("remove", &edit.remove_labels)];
        ensure_set_applied(
            item.number,
            "labels",
            &item.labels,
            labels,
            |name, label| name == label || (fold_case && name.eq_ignore_ascii_case(label)),
        )?;
        // Logins name the same user whatever their case.
        let assignees = [
            ("assign", &edit.add_assignees),
            ("unassign", &edit.remove_assignees),
        ];
        ensure_set_applied(
            item.number,
            "assignees",
            &item.assignees,
            assignees,
            |name, login| name.eq_ignore_ascii_case(login),
        )?;
        edit.ensure_milestone_applied(item)
    }
}

/// Refuse the changes to `set`, item `number`'s `noun` as the forge reports
/// them afterwards, that it does not show: the names added first that it
/// lacks and the names removed second that it has, each change named by its
/// verb. `same` is whether a name of `set` names the one changed.
fn ensure_set_applied(
    number: u64,
    noun: &str,
    set: &[String],
    changes: [(&str, &Vec<String>); 2],
    same: impl Fn(&str, &str) -> bool,
) -> Result<()> {
    let [(add, added), (remove, removed)] = changes;
    let problems: Vec<_> = [(add, added, true), (remove, removed, false)]
        .into_iter()
        .filter_map(|(verb, names, added)| {
            let names: Vec<_> = names
                .iter()
                .map(String::as_str)
                .filter(|name| set.iter().any(|has| same(has, name)) != added)
                .collect();
            (!names.is_empty()).then(|| format!("did not {verb} {}", names.join(", ")))
        })
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "the forge {} on #{number}, which has {}",
        problems.join(" and "),
        listing(noun, set)
    )
}

/// `names` as "the <noun> a, b", or "no <noun>".
fn listing(noun: &str, names: &[String]) -> String {
    match names {
        [] => format!("no {noun}"),
        names => format!("the {noun} {}", names.join(", ")),
    }
}

/// What an action left on the forge.
pub struct Acted {
    /// The item as the forge reports it afterwards.
    pub item: Item,
    /// The activity keys of the comments, replies or reviews the action
    /// posted.
    pub authored: Vec<String>,
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
        match action {
            Action::Review(review) => return self.submit_review(account, number, review).await,
            // Boxed so finding a comment among the item's discussion does not
            // grow the future of every action.
            Action::React(change) => {
                return Box::pin(self.react(account, kind, number, change)).await;
            }
            Action::EditComment(edit) => {
                return Box::pin(self.edit_own_comment(account, kind, number, edit)).await;
            }
            _ => {}
        }
        let per_item = self.check_uploads(account, action.attachments()).await?;
        let current = self.read(account, kind, number).await?;
        let (action, pending) = self
            .upload_attachments(account, &current, action, per_item)
            .await?;
        let mut authored = Vec::new();
        let mut first = None;
        for request in self.requests(account, current, &action).await? {
            let response = self
                .send(account, &request)
                .await
                .map_err(|error| after_post(error, first.is_some(), &authored))?;
            // A comment, or a thread's reply, is the action's first request.
            if first.is_none() {
                authored.extend(self.posted(&action, &response));
                first = Some(response);
            }
        }
        if let (Some(pending), Some(comment)) = (pending, &first) {
            self.attach_to_comment(account, kind, number, comment, pending)
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
            .kind
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
            Action::Comment { .. } => self.kind.comment_key(),
            _ if reply => self.kind.reply_key(),
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
                let number = current.item.number;
                let thread = self.thread(account, number, change.comment).await?;
                self.plan_thread(number, &thread, change)
            }
            Action::Edit(edit) => {
                if let Some(title) = edit.milestone_title() {
                    current.milestone_id = Some(self.milestone_id(account, title).await?);
                }
                self.kind
                    .prepare_edit(self, account, edit, &mut current)
                    .await?;
                self.plan(&current, action)
            }
            Action::DeleteBranch => {
                let requests = self.plan(&current, action)?;
                self.ensure_branch_unmoved(account, &current).await?;
                Ok(requests)
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
                ensure_open(item)?;
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
                ensure_open(item)?;
                self.kind.merge(self, current, *method, *delete_branch)?
            }
            Action::AutoMerge {
                method,
                delete_branch,
            } => {
                ensure_open(item)?;
                self.kind
                    .auto_merge(self, current, *method, *delete_branch)?
            }
            Action::CancelAutoMerge => {
                ensure_open(item)?;
                self.kind.cancel_auto_merge(self, current)?
            }
            Action::DeleteBranch => vec![self.branch_deletion(current)?],
            Action::Update { method } => {
                ensure_open(item)?;
                self.kind.update_branch(self, current, *method)?
            }
            Action::Thread(_) => anyhow::bail!("a review thread change is planned from its thread"),
            Action::Review(_) => anyhow::bail!("a review is planned from the PR's diff"),
            Action::React(_) => {
                anyhow::bail!("a reaction is planned from the reactions it changes")
            }
            Action::EditComment(_) => anyhow::bail!("a comment's edit is planned from the comment"),
        })
    }

    /// The request that deletes the head branch of `current`, refused while
    /// the PR is open or when the branch is a fork's.
    fn branch_deletion(&self, current: &Current) -> Result<Request> {
        let item = &current.item;
        let pr = item
            .pr
            .as_ref()
            .context("only PRs have a branch to delete")?;
        ensure!(
            item.state != ItemState::Open,
            "#{} is open; merge or close it before deleting its branch",
            item.number
        );
        ensure!(
            current.head_in(self),
            "the branch of #{} is in a fork, not in {}",
            item.number,
            self.path
        );
        Ok(self.kind.branch_delete(self, &pr.head))
    }

    /// Refuse to delete a PR's branch that no longer points to the PR's
    /// head, such as one pushed to again after it merged, whose other
    /// commits are in no PR.
    async fn ensure_branch_unmoved(&self, account: &Account, current: &Current) -> Result<()> {
        let item = &current.item;
        let pr = item
            .pr
            .as_ref()
            .context("only PRs have a branch to delete")?;
        let head = current
            .head_sha
            .as_deref()
            .context("the forge did not report the PR's head commit")?;
        let tip = self.branch_commit(account, &pr.head).await?;
        // Bitbucket reports a PR's head as an abbreviated hash.
        ensure!(
            tip.starts_with(head),
            "branch {} moved to {tip} after #{} was {}; it holds commits the PR does not have",
            pr.head,
            item.number,
            item.state.as_str()
        );
        Ok(())
    }
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
fn comment_id(response: &serde_json::Value) -> Result<u64> {
    response["id"]
        .as_u64()
        .context("the forge did not report the comment's ID")
}

#[cfg(test)]
mod tests;
