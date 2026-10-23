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
        let head = Head {
            repository: "group/sub/repo",
            branch: "topic",
        };
        let listed =
            json!({"source_branch": "topic", "source_project_id": 1, "target_project_id": 1});
        assert!(gitlab.kind.has_head(&gitlab, &listed, &head));
        let fork =
            json!({"source_branch": "topic", "source_project_id": 2, "target_project_id": 1});
        assert!(!gitlab.kind.has_head(&gitlab, &fork, &head));
        let forked = Head {
            repository: "someone/repo",
            ..head
        };
        assert!(!gitlab.kind.has_head(&gitlab, &listed, &forked));
        assert!(gitlab.require(Capability::ForkPrs).is_err());
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
        let head = |repository, branch| Head { repository, branch };
        assert!(
            bitbucket
                .kind
                .has_head(&bitbucket, &listed, &head("workspace/repo", "topic"))
        );
        assert!(
            !bitbucket
                .kind
                .has_head(&bitbucket, &listed, &head("workspace/repo", "other"))
        );
        assert!(
            !bitbucket
                .kind
                .has_head(&bitbucket, &listed, &head("someone/repo", "topic"))
        );
        let forked = NewPull {
            head_repository: Some("someone/repo".into()),
            ..pull
        };
        assert_eq!(
            bitbucket.create_pull_request(&forked).body.unwrap()["source"],
            json!({"branch": {"name": "topic"}, "repository": {"full_name": "someone/repo"}})
        );
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

    #[test]
    fn github_and_forgejo_name_a_fork_head_by_its_owner() {
        let pull = NewPull {
            title: "Fix".into(),
            head: "topic".into(),
            head_repository: Some("someone/repo".into()),
            base: "main".into(),
            ..NewPull::default()
        };
        for remote in [
            "git@github.com:team/repo.git",
            "https://forge.example/team/repo",
        ] {
            let forge = ForgeRepo::parse(remote).unwrap();
            assert_eq!(
                forge.create_pull_request(&pull).body.unwrap()["head"],
                "someone:topic"
            );
            assert!(forge.require(Capability::ForkPrs).is_ok());
            let listed = json!({"head": {"ref": "topic", "repo": {"full_name": "Someone/Repo"}}});
            let head = |repository| Head {
                repository,
                branch: "topic",
            };
            assert!(forge.kind.has_head(&forge, &listed, &head("someone/repo")));
            assert!(!forge.kind.has_head(&forge, &listed, &head("team/repo")));
        }
    }

    #[test]
    fn each_forge_reads_a_branch_commit_from_its_own_route() {
        for (remote, endpoint, pointer) in [
            (
                "git@github.com:team/repo.git",
                "repos/team/repo/branches/fix/a",
                "/commit/sha",
            ),
            (
                "https://forge.example/team/repo",
                "repos/team/repo/branches/fix%2Fa",
                "/commit/id",
            ),
            (
                "git@gitlab.com:team/repo.git",
                "projects/team%2Frepo/repository/branches/fix%2Fa",
                "/commit/id",
            ),
            (
                "git@bitbucket.org:team/repo.git",
                "repositories/team/repo/refs/branches/fix%2Fa",
                "/target/hash",
            ),
        ] {
            let forge = ForgeRepo::parse(remote).unwrap();
            let (request, read) = forge.kind.branch_read(&forge, "fix/a");
            assert_eq!((request.endpoint.as_str(), read), (endpoint, pointer));
        }
        let fork = ForgeRepo::parse("https://github.com/team/repo")
            .unwrap()
            .sibling("someone/repo");
        assert_eq!(
            fork,
            ForgeRepo::parse("https://github.com/someone/repo").unwrap()
        );
    }
}
