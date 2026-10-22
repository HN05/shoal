//! Open issues and PRs through the forge's REST API, and find a branch's
//! open PR.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    action::{Action, Edit, draft_title},
    api::{HttpMethod, Request, encode},
    attach::{Attachment, Pending},
    gitlab,
    item::Item,
    link::ItemKind,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NewPull {
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub attachments: Vec<Attachment>,
}

impl ForgeRepo {
    /// The open PR whose head is `branch` in this repository.
    pub(crate) async fn open_pull_for(
        &self,
        account: &Account,
        branch: &str,
    ) -> Result<Option<u64>> {
        let pulls = match self.kind {
            ForgeKind::GitHub => {
                let owner = self.path.split('/').next().unwrap_or_default();
                let head = encode(&format!("{owner}:{branch}"), false);
                let endpoint = self.repo_endpoint(&format!("pulls?state=open&head={head}"));
                self.send(account, &Request::get(endpoint))
                    .await?
                    .as_array()
                    .cloned()
                    .context("the PR list is not a list")?
            }
            // Forgejo cannot filter its list by head branch.
            ForgeKind::Forgejo => {
                self.pages(account, &self.repo_endpoint("pulls?state=open"))
                    .await?
            }
            ForgeKind::GitLab => {
                let branch = encode(branch, false);
                let endpoint = format!("merge_requests?state=opened&source_branch={branch}");
                self.pages(account, &self.repo_endpoint(&endpoint)).await?
            }
            ForgeKind::Bitbucket => {
                let literal = branch.replace('\\', "\\\\").replace('"', "\\\"");
                let query = encode(&format!("source.branch.name=\"{literal}\""), false);
                let endpoint = format!("pullrequests?state=OPEN&q={query}");
                self.pages(account, &self.repo_endpoint(&endpoint)).await?
            }
        };
        pulls
            .iter()
            .find(|pull| self.has_head(pull, branch))
            .map(|pull| self.created_number(pull))
            .transpose()
    }

    /// Whether the listed PR's head is `branch` in this repository.
    fn has_head(&self, pull: &Value, branch: &str) -> bool {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => {
                pull["head"]["ref"] == branch
                    && pull["head"]["repo"]["full_name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&self.path))
            }
            ForgeKind::GitLab => {
                pull["source_branch"] == branch
                    && pull["source_project_id"] == pull["target_project_id"]
            }
            ForgeKind::Bitbucket => {
                pull["source"]["branch"]["name"] == branch
                    && pull["source"]["repository"]["full_name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&self.path))
            }
        }
    }

    /// Open the PR and return it as the forge reports it.
    pub(crate) async fn create_pull(&self, account: &Account, pull: &NewPull) -> Result<Item> {
        self.ensure_labels(&pull.labels)?;
        let (body, pending) = self
            .new_body(account, &pull.body, &pull.attachments)
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
        if follow_up == Edit::default() {
            return Ok(self.read(account, ItemKind::Pr, number).await?.item);
        }
        self.act(account, ItemKind::Pr, number, &Action::Edit(follow_up))
            .await
            .with_context(|| {
                format!(
                    "PR #{number} was opened, but labeling it, requesting reviews or attaching its files failed"
                )
            })
    }

    fn create_pull_request(&self, pull: &NewPull) -> Request {
        let title = match self.kind {
            ForgeKind::Forgejo if pull.draft => draft_title(&pull.title, true),
            ForgeKind::GitLab if pull.draft => gitlab::draft_title(&pull.title, true),
            _ => pull.title.clone(),
        };
        let (resource, body) = match self.kind {
            ForgeKind::GitHub => (
                "pulls",
                json!({"title": title, "body": pull.body, "head": pull.head, "base": pull.base,
                    "draft": pull.draft}),
            ),
            ForgeKind::Forgejo => (
                "pulls",
                json!({"title": title, "body": pull.body, "head": pull.head, "base": pull.base}),
            ),
            ForgeKind::GitLab => (
                "merge_requests",
                json!({"title": title, "description": pull.body,
                    "source_branch": pull.head, "target_branch": pull.base}),
            ),
            ForgeKind::Bitbucket => (
                "pullrequests",
                json!({"title": title, "description": pull.body, "draft": pull.draft,
                    "source": {"branch": {"name": pull.head}},
                    "destination": {"branch": {"name": pull.base}}}),
            ),
        };
        Request::new(HttpMethod::Post, self.repo_endpoint(resource), Some(body))
    }

    /// Open the issue and return it as the forge reports it.
    pub(crate) async fn create_issue(&self, account: &Account, issue: &NewIssue) -> Result<Item> {
        self.ensure_labels(&issue.labels)?;
        let (body, pending) = self
            .new_body(account, &issue.body, &issue.attachments)
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
        // Forgejo takes only label IDs on creation; the label set takes names.
        let add_labels = match self.kind {
            ForgeKind::Forgejo => issue.labels,
            _ => Vec::new(),
        };
        let follow_up = Edit {
            add_labels,
            ..pending_edit(pending)
        };
        if follow_up == Edit::default() {
            return Ok(self.read(account, ItemKind::Issue, number).await?.item);
        }
        self.act(account, ItemKind::Issue, number, &Action::Edit(follow_up))
            .await
            .with_context(|| {
                format!("issue #{number} was opened, but labeling it or attaching its files failed")
            })
    }

    fn issue_request(&self, issue: &NewIssue) -> Request {
        let body = match self.kind {
            ForgeKind::GitHub => {
                json!({ "title": issue.title, "body": issue.body, "labels": issue.labels })
            }
            ForgeKind::Forgejo => json!({ "title": issue.title, "body": issue.body }),
            ForgeKind::GitLab => json!({ "title": issue.title, "description": issue.body,
                "labels": issue.labels.join(",") }),
            ForgeKind::Bitbucket => {
                json!({ "title": issue.title, "content": { "raw": issue.body } })
            }
        };
        Request::new(HttpMethod::Post, self.repo_endpoint("issues"), Some(body))
    }

    /// Refuse labels where the forge has none, before creating anything.
    fn ensure_labels(&self, labels: &[String]) -> Result<()> {
        anyhow::ensure!(
            labels.is_empty() || self.kind != ForgeKind::Bitbucket,
            "Bitbucket has no labels"
        );
        Ok(())
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
        assert!(gitlab.has_head(&listed, "topic"));
        let fork =
            json!({"source_branch": "topic", "source_project_id": 2, "target_project_id": 1});
        assert!(!gitlab.has_head(&fork, "topic"));
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
        assert!(bitbucket.ensure_labels(&["bug".into()]).is_err());
        assert!(bitbucket.ensure_labels(&[]).is_ok());
        assert_eq!(bitbucket.created_number(&json!({"id": 9})).unwrap(), 9);
        let listed = json!({"source": {"branch": {"name": "topic"},
            "repository": {"full_name": "Workspace/Repo"}}});
        assert!(bitbucket.has_head(&listed, "topic"));
        assert!(!bitbucket.has_head(&listed, "other"));
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
