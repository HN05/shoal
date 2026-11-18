//! Changes to an issue or PR: what each action asks for, and the checks of
//! what the forge left once it applied one.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};
use shoal_states::states;

use crate::items::attach::Attachment;
use crate::items::comment::CommentEdit;
use crate::items::reaction::ReactionChange;
use crate::review::submit::NewReview;
use crate::review::submit::ReviewVerdict;
use crate::{
    capability::Capability,
    items::{item::Item, link::ItemKind},
    numbers::CommentId,
};

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
    /// `head` is the commit the merge is for, such as one an approval bound,
    /// and else the head the PR has when read before merging; the forge
    /// refuses to merge a head that moved from it. Without a method the PR
    /// merges by the repository's default.
    Merge {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<MergeMethod>,
        delete_branch: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<CommitId>,
    },
    /// Have the forge merge the PR once its required checks and approvals
    /// pass, or now when they already do, at `head` as a merge takes it.
    AutoMerge {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method: Option<MergeMethod>,
        delete_branch: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head: Option<CommitId>,
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
    pub base: Option<BranchName>,
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
    pub fn assigns(&self) -> bool {
        !self.add_assignees.is_empty() || !self.remove_assignees.is_empty()
    }

    /// The assignees of `current` after the edit, in order.
    pub fn assignees_after(&self, current: &[String]) -> Vec<String> {
        changed_set(current, &self.add_assignees, &self.remove_assignees)
    }

    /// The title of the milestone the edit sets, if it sets one.
    pub fn milestone_title(&self) -> Option<&str> {
        match &self.milestone {
            Some(MilestoneChange::Set(title)) => Some(title),
            _ => None,
        }
    }

    pub fn ensure_milestone_applied(&self, item: &Item) -> Result<()> {
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
pub fn changed_set(current: &[String], added: &[String], removed: &[String]) -> Vec<String> {
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
    pub comment: CommentId,
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

    /// The action as an error names it, such as "request changes".
    pub fn verb(&self) -> String {
        self.name().replace('_', " ")
    }

    /// The action's name, which the log records; a review's is its
    /// verdict's, so the log tells an approval from a comment.
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
            Self::Review(review) => match review.verdict {
                ReviewVerdict::Comment => "review",
                ReviewVerdict::Approve => "approve",
                ReviewVerdict::RequestChanges => "request_changes",
            },
            Self::React(change) if change.remove => "unreact",
            Self::React(_) => "react",
        }
    }

    /// Whether the action merges the PR, now or once it can.
    pub fn merges(&self) -> bool {
        matches!(self, Self::Merge { .. } | Self::AutoMerge { .. })
    }

    /// The head commit a merge names.
    pub fn merge_head(&self) -> Option<&CommitId> {
        match self {
            Self::Merge { head, .. } | Self::AutoMerge { head, .. } => head.as_ref(),
            _ => None,
        }
    }

    /// Bind a merge that names no head yet to `read`, the head the PR had
    /// when read; other actions stay as they are.
    pub fn bind_merge_head(&mut self, read: CommitId) {
        if let Self::Merge { head, .. } | Self::AutoMerge { head, .. } = self {
            head.get_or_insert(read);
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
            Self::Merge { method, .. } => method.map(Capability::merge).into_iter().collect(),
            Self::AutoMerge { method, .. } => std::iter::once(Capability::AutoMerge)
                .chain(method.map(Capability::merge))
                .collect(),
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
    pub fn ensure_applied(&self, item: &Item, fold_case: bool) -> Result<()> {
        let Self::Edit(edit) = self else {
            return Ok(());
        };
        let labels = [("add", &edit.add_labels), ("remove", &edit.remove_labels)];
        ensure_set_applied(
            item.number,
            "labels",
            &item.labels,
            labels,
            |name, label| same_label(name, label, fold_case),
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

    /// The labels the action adds that `item` lacks.
    pub fn unadded_labels(&self, item: &Item, fold_case: bool) -> Vec<&str> {
        let Self::Edit(edit) = self else {
            return Vec::new();
        };
        edit.add_labels
            .iter()
            .map(String::as_str)
            .filter(|added| {
                !item
                    .labels
                    .iter()
                    .any(|has| same_label(has, added, fold_case))
            })
            .collect()
    }
}

/// Whether label names `a` and `b` name the same label; `fold_case` is
/// whether names that differ only in case do.
pub fn same_label(a: &str, b: &str, fold_case: bool) -> bool {
    a == b || (fold_case && a.eq_ignore_ascii_case(b))
}

/// `names` as "the <noun> a, b", or "no <noun>".
fn listing(noun: &str, names: &[String]) -> String {
    match names {
        [] => format!("no {noun}"),
        names => format!("the {noun} {}", names.join(", ")),
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

#[cfg(test)]
mod tests;
