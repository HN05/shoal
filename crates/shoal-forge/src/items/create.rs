//! Open issues and PRs through the forge's REST API, and find a branch's
//! open PR.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        action::{Action, Edit},
        attach::{Attachment, Pending},
        item::{Item, ItemState, PrSummary},
        link::ItemKind,
    },
    transport::api::Request,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// What `shoal pr open` was told about a new PR; unset fields take defaults
/// from the workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullOptions {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    /// Overrides `[pr] draft` when set.
    pub draft: Option<bool>,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub attachments: Vec<Attachment>,
}

/// A PR to open from `head` into `base`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPull {
    pub title: String,
    pub body: String,
    pub head: String,
    /// `owner/repository` of the fork `head` is in; unset for this repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_repository: Option<String>,
    pub base: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub attachments: Vec<Attachment>,
}

impl NewIssue {
    /// The capabilities the forge needs to open the issue, besides
    /// attachments, which uploads check.
    fn needs(&self) -> Option<Capability> {
        (!self.labels.is_empty()).then_some(Capability::Labels)
    }
}

impl NewPull {
    /// The capabilities the forge needs to open the PR, besides attachments.
    fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let draft = self.draft.then_some(Capability::DraftPrs);
        let fork = self
            .head_repository
            .is_some()
            .then_some(Capability::ForkPrs);
        labels.into_iter().chain(draft).chain(fork).collect()
    }
}

/// An item Shoal opened, as the forge reports it once finished, or as it
/// was posted when finishing it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub item: Item,
    /// Why finishing the item failed after the forge opened it, so it may
    /// lack labels, reviewers or files.
    pub unfinished: Option<String>,
}

/// A PR's head: a branch in the PR's repository or in a fork of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head<'a> {
    /// `owner/repository` of the repository the branch is in.
    pub repository: &'a str,
    pub branch: &'a str,
}

impl Head<'_> {
    /// The owner of the repository the branch is in.
    pub(crate) fn owner(&self) -> &str {
        self.repository.split('/').next().unwrap_or_default()
    }
}

impl ForgeRepo {
    /// Another repository on the same forge, such as a fork.
    pub fn sibling(&self, path: &str) -> ForgeRepo {
        ForgeRepo {
            host: self.host.clone(),
            path: path.to_owned(),
            kind: self.kind,
            web_scheme: self.web_scheme,
        }
    }

    /// The commit `branch` points to, as the forge reports it: what a PR
    /// with it as its head would contain.
    pub async fn branch_commit(&self, account: &Account, branch: &str) -> Result<String> {
        let (request, pointer) = self.kind.branch_read(self, branch);
        let read = self
            .send(account, &request)
            .await
            .with_context(|| format!("could not read branch {branch} of {}", self.path))?;
        read.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("the forge did not report the commit of branch {branch}"))
    }

    /// The open PR whose head is `branch` in `head_repository`, else in this
    /// repository.
    pub async fn open_pull_for(
        &self,
        account: &Account,
        branch: &str,
        head_repository: Option<&str>,
    ) -> Result<Option<u64>> {
        let head = Head {
            repository: head_repository.unwrap_or(&self.path),
            branch,
        };
        let pulls = self.kind.open_pulls_from(self, account, &head).await?;
        pulls
            .iter()
            .find(|pull| self.kind.has_head(self, pull, &head))
            .map(|pull| self.created_number(pull))
            .transpose()
    }

    /// Open the PR; only failing to post it fails.
    pub async fn create_pull(&self, account: &Account, pull: &NewPull) -> Result<Created> {
        let (posted, follow_up) = self.post_pull(account, pull).await?;
        Ok(self.finish_posted(account, posted, &follow_up).await)
    }

    /// Post the PR and return it as posted with the edit that finishes it:
    /// its labels, reviewers and files the forge keeps with the item.
    pub async fn post_pull(&self, account: &Account, pull: &NewPull) -> Result<(Item, Edit)> {
        self.require_all(pull.needs())?;
        let per_item = self.check_uploads(account, &pull.attachments).await?;
        let (body, pending) = self
            .new_body(account, &pull.body, &pull.attachments, per_item)
            .await?;
        let pull = NewPull {
            body,
            ..pull.clone()
        };
        let created = self
            .send(account, &self.create_pull_request(&pull))
            .await
            .context("could not open the PR")?;
        let summary = PrSummary {
            head: pull.head,
            base: pull.base,
            draft: pull.draft,
        };
        let posted = Item {
            pr: Some(summary),
            ..self.posted_item(ItemKind::Pr, &created, pull.title, Vec::new())?
        };
        let follow_up = Edit {
            add_labels: pull.labels,
            add_reviewers: pull.reviewers,
            ..pending_edit(pending)
        };
        Ok((posted, follow_up))
    }

    fn create_pull_request(&self, pull: &NewPull) -> Request {
        self.kind.new_pull(self, pull)
    }

    /// Open the issue; only failing to post it fails.
    pub async fn create_issue(&self, account: &Account, issue: &NewIssue) -> Result<Created> {
        let (posted, follow_up) = self.post_issue(account, issue).await?;
        Ok(self.finish_posted(account, posted, &follow_up).await)
    }

    /// Finish the item that was just posted, keeping it as posted when that
    /// fails: it exists, so the caller records it instead of failing, and a
    /// retry cannot open another.
    pub(crate) async fn finish_posted(
        &self,
        account: &Account,
        posted: Item,
        follow_up: &Edit,
    ) -> Created {
        match (self.finish_item(account, posted.kind, posted.number, follow_up)).await {
            Ok(item) => Created {
                item,
                unfinished: None,
            },
            Err(error) => Created {
                item: posted,
                unfinished: Some(format!("{error:#}")),
            },
        }
    }

    /// Post the issue and return it as posted with the edit that finishes it:
    /// Forgejo's labels, which it takes only by ID on creation, and files the
    /// forge keeps with the item.
    pub async fn post_issue(&self, account: &Account, issue: &NewIssue) -> Result<(Item, Edit)> {
        self.require_all(issue.needs())?;
        let per_item = self.check_uploads(account, &issue.attachments).await?;
        let (body, pending) = self
            .new_body(account, &issue.body, &issue.attachments, per_item)
            .await?;
        let issue = NewIssue {
            body,
            ..issue.clone()
        };
        let created = self
            .send(account, &self.issue_request(&issue))
            .await
            .context("could not open the issue")?;
        let (posted_labels, add_labels) = if self.kind.labels_issue_after_opening() {
            (Vec::new(), issue.labels)
        } else {
            (issue.labels, Vec::new())
        };
        let posted = self.posted_item(ItemKind::Issue, &created, issue.title, posted_labels)?;
        let follow_up = Edit {
            add_labels,
            ..pending_edit(pending)
        };
        Ok((posted, follow_up))
    }

    /// The open item a creation response describes, with what was posted.
    fn posted_item(
        &self,
        kind: ItemKind,
        created: &Value,
        title: String,
        labels: Vec<String>,
    ) -> Result<Item> {
        let number = self.created_number(created)?;
        Ok(Item {
            kind,
            number,
            url: self.item_url(kind, number)?,
            title,
            state: ItemState::Open,
            labels,
            pr: None,
        })
    }

    /// Apply the edit [`Self::post_issue`] or [`Self::post_pull`] left to
    /// the item `number`, and return it as the forge then reports it.
    pub async fn finish_item(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
        follow_up: &Edit,
    ) -> Result<Item> {
        let opened = format!("{} #{number} was opened", kind.noun());
        if *follow_up == Edit::default() {
            let read = self.read(account, kind, number).await;
            return read
                .map(|read| read.item)
                .with_context(|| format!("{opened}, but reading it failed"));
        }
        self.act(account, kind, number, &Action::Edit(follow_up.clone()))
            .await
            .with_context(|| match kind {
                ItemKind::Issue => {
                    format!("{opened}, but labeling it or attaching its files failed")
                }
                ItemKind::Pr => format!(
                    "{opened}, but labeling it, requesting reviews or attaching its files failed"
                ),
            })
    }

    fn issue_request(&self, issue: &NewIssue) -> Request {
        self.kind.new_issue(self, issue)
    }

    /// The number of the item a creation or list response describes.
    fn created_number(&self, created: &Value) -> Result<u64> {
        created[self.kind.number_field()]
            .as_u64()
            .context("the forge did not report the new item's number")
    }
}

/// The edit that attaches the files of a new item once it exists.
pub(crate) fn pending_edit(pending: Option<Pending>) -> Edit {
    match pending {
        Some(pending) => Edit {
            body: Some(pending.body),
            attachments: pending.attachments,
            ..Edit::default()
        },
        None => Edit::default(),
    }
}

#[cfg(test)]
mod tests;
