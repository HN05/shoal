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
        item::Item,
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
        labels.into_iter().chain(draft).collect()
    }
}

impl ForgeRepo {
    /// The open PR whose head is `branch` in this repository.
    pub async fn open_pull_for(&self, account: &Account, branch: &str) -> Result<Option<u64>> {
        let pulls = self.kind.open_pulls_from(self, account, branch).await?;
        pulls
            .iter()
            .find(|pull| self.kind.has_head(self, pull, branch))
            .map(|pull| self.created_number(pull))
            .transpose()
    }

    /// Open the PR and return it as the forge reports it.
    pub async fn create_pull(&self, account: &Account, pull: &NewPull) -> Result<Item> {
        let (number, follow_up) = self.post_pull(account, pull).await?;
        self.finish_item(account, ItemKind::Pr, number, &follow_up)
            .await
    }

    /// Post the PR and return its number with the edit that finishes it: its
    /// labels, reviewers and files the forge keeps with the item.
    pub async fn post_pull(&self, account: &Account, pull: &NewPull) -> Result<(u64, Edit)> {
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
        let number = self.created_number(&created)?;
        let follow_up = Edit {
            add_labels: pull.labels,
            add_reviewers: pull.reviewers,
            ..pending_edit(pending)
        };
        Ok((number, follow_up))
    }

    fn create_pull_request(&self, pull: &NewPull) -> Request {
        self.kind.new_pull(self, pull)
    }

    /// Open the issue and return it as the forge reports it.
    pub async fn create_issue(&self, account: &Account, issue: &NewIssue) -> Result<Item> {
        let (number, follow_up) = self.post_issue(account, issue).await?;
        self.finish_item(account, ItemKind::Issue, number, &follow_up)
            .await
    }

    /// Post the issue and return its number with the edit that finishes it:
    /// Forgejo's labels, which it takes only by ID on creation, and files the
    /// forge keeps with the item.
    pub async fn post_issue(&self, account: &Account, issue: &NewIssue) -> Result<(u64, Edit)> {
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
        let number = self.created_number(&created)?;
        let add_labels = if self.kind.labels_issue_after_opening() {
            issue.labels
        } else {
            Vec::new()
        };
        let follow_up = Edit {
            add_labels,
            ..pending_edit(pending)
        };
        Ok((number, follow_up))
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
        if *follow_up == Edit::default() {
            return Ok(self.read(account, kind, number).await?.item);
        }
        self.act(account, kind, number, &Action::Edit(follow_up.clone()))
            .await
            .with_context(|| match kind {
                ItemKind::Issue => format!(
                    "issue #{number} was opened, but labeling it or attaching its files failed"
                ),
                ItemKind::Pr => format!(
                    "PR #{number} was opened, but labeling it, requesting reviews or attaching its files failed"
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
fn pending_edit(pending: Option<Pending>) -> Edit {
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
mod tests {
    use super::*;
    use crate::transport::api::HttpMethod;
    use serde_json::json;

    #[test]
    fn issues_carry_labels_on_github_and_label_afterwards_on_forgejo() {
        let issue = NewIssue {
            title: "Flaky".into(),
            body: "Seen twice".into(),
            labels: vec!["bug".into()],
            attachments: Vec::new(),
        };
        let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        assert_eq!(
            github.issue_request(&issue),
            Request::new(
                HttpMethod::Post,
                "repos/team/repo/issues".into(),
                Some(json!({"title": "Flaky", "body": "Seen twice", "labels": ["bug"]})),
            )
        );
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            forgejo.issue_request(&issue).body,
            Some(json!({"title": "Flaky", "body": "Seen twice"}))
        );
        assert!(github.created_number(&json!({"id": 1})).is_err());
    }

    #[test]
    fn gitlab_opens_merge_requests_with_draft_titles_and_labeled_issues() {
        let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
        let pull = NewPull {
            title: "Fix".into(),
            body: "Closes #3".into(),
            head: "topic".into(),
            base: "main".into(),
            draft: true,
            ..NewPull::default()
        };
        assert_eq!(
            gitlab.create_pull_request(&pull),
            Request::new(
                HttpMethod::Post,
                "projects/group%2Fsub%2Frepo/merge_requests".into(),
                Some(json!({"title": "Draft: Fix", "description": "Closes #3",
                    "source_branch": "topic", "target_branch": "main"})),
            )
        );
        let issue = NewIssue {
            title: "Flaky".into(),
            body: "Seen twice".into(),
            labels: vec!["bug".into(), "ci".into()],
            attachments: Vec::new(),
        };
        assert_eq!(
            gitlab.issue_request(&issue).body,
            Some(json!({"title": "Flaky", "description": "Seen twice", "labels": "bug,ci"}))
        );
        assert_eq!(
            gitlab.created_number(&json!({"iid": 4, "id": 90})).unwrap(),
            4
        );
        let listed =
            json!({"source_branch": "topic", "source_project_id": 1, "target_project_id": 1});
        assert!(gitlab.kind.has_head(&gitlab, &listed, "topic"));
        let fork =
            json!({"source_branch": "topic", "source_project_id": 2, "target_project_id": 1});
        assert!(!gitlab.kind.has_head(&gitlab, &fork, "topic"));
    }

    #[test]
    fn bitbucket_opens_pull_requests_and_issues_without_labels() {
        let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
        let pull = NewPull {
            title: "Fix".into(),
            body: "Closes #3".into(),
            head: "topic".into(),
            base: "main".into(),
            draft: true,
            ..NewPull::default()
        };
        assert_eq!(
            bitbucket.create_pull_request(&pull),
            Request::new(
                HttpMethod::Post,
                "repositories/workspace/repo/pullrequests".into(),
                Some(
                    json!({"title": "Fix", "description": "Closes #3", "draft": true,
                    "source": {"branch": {"name": "topic"}},
                    "destination": {"branch": {"name": "main"}}})
                ),
            )
        );
        let issue = NewIssue {
            title: "Flaky".into(),
            body: "Seen twice".into(),
            labels: Vec::new(),
            attachments: Vec::new(),
        };
        assert_eq!(
            bitbucket.issue_request(&issue).body,
            Some(json!({"title": "Flaky", "content": {"raw": "Seen twice"}}))
        );
        assert!(bitbucket.require_all(issue.needs()).is_ok());
        let labeled = NewIssue {
            labels: vec!["bug".into()],
            ..issue
        };
        assert!(bitbucket.require_all(labeled.needs()).is_err());
        assert_eq!(bitbucket.created_number(&json!({"id": 9})).unwrap(), 9);
        let listed = json!({"source": {"branch": {"name": "topic"},
            "repository": {"full_name": "Workspace/Repo"}}});
        assert!(bitbucket.kind.has_head(&bitbucket, &listed, "topic"));
        assert!(!bitbucket.kind.has_head(&bitbucket, &listed, "other"));
    }

    #[test]
    fn drafts_are_a_flag_on_github_and_a_title_on_forgejo() {
        let pull = NewPull {
            title: "Fix".into(),
            body: "Closes #3".into(),
            head: "topic".into(),
            base: "main".into(),
            draft: true,
            ..NewPull::default()
        };
        let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        assert_eq!(
            github.create_pull_request(&pull),
            Request::new(
                HttpMethod::Post,
                "repos/team/repo/pulls".into(),
                Some(json!({"title": "Fix", "body": "Closes #3", "head": "topic",
                    "base": "main", "draft": true})),
            )
        );
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            forgejo.create_pull_request(&pull).body,
            Some(
                json!({"title": "WIP: Fix", "body": "Closes #3", "head": "topic", "base": "main"})
            )
        );
    }
}
