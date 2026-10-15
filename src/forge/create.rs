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
    gitlab,
    item::Item,
    link::ItemKind,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// What `shoal pr open` was told about a new PR; unset fields take defaults
/// from the workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullOptions {
    pub title: Option<String>,
    pub body: Option<String>,
    pub base: Option<String>,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
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
}

impl ForgeRepo {
    /// The open PR whose head is `branch` in this repository.
    pub(crate) async fn open_pull_for(
        &self,
        account: &Account,
        branch: &str,
    ) -> Result<Option<u64>> {
        anyhow::ensure!(
            self.kind != ForgeKind::Bitbucket,
            "opening Bitbucket pull requests is not supported yet"
        );
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
            ForgeKind::GitLab | ForgeKind::Bitbucket => {
                let branch = encode(branch, false);
                let endpoint = format!("merge_requests?state=opened&source_branch={branch}");
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
            ForgeKind::GitLab | ForgeKind::Bitbucket => {
                pull["source_branch"] == branch
                    && pull["source_project_id"] == pull["target_project_id"]
            }
        }
    }

    /// Open the PR and return it as the forge reports it.
    pub(crate) async fn create_pull(&self, account: &Account, pull: &NewPull) -> Result<Item> {
        let created = self
            .send(account, &self.create_pull_request(pull))
            .await
            .context("could not open the PR")?;
        let number = self.created_number(&created)?;
        if pull.labels.is_empty() && pull.reviewers.is_empty() {
            return Ok(self.read(account, ItemKind::Pr, number).await?.item);
        }
        let follow_up = Action::Edit(Edit {
            add_labels: pull.labels.clone(),
            add_reviewers: pull.reviewers.clone(),
            ..Edit::default()
        });
        self.act(account, ItemKind::Pr, number, &follow_up)
            .await
            .with_context(|| {
                format!("PR #{number} was opened, but labeling it or requesting reviews failed")
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
            ForgeKind::GitLab | ForgeKind::Bitbucket => (
                "merge_requests",
                json!({"title": title, "description": pull.body,
                    "source_branch": pull.head, "target_branch": pull.base}),
            ),
        };
        Request::new(HttpMethod::Post, self.repo_endpoint(resource), Some(body))
    }

    /// Open the issue and return it as the forge reports it.
    pub(crate) async fn create_issue(&self, account: &Account, issue: &NewIssue) -> Result<Item> {
        anyhow::ensure!(
            self.kind != ForgeKind::Bitbucket,
            "opening Bitbucket issues is not supported yet"
        );
        let created = self
            .send(account, &self.issue_request(issue))
            .await
            .context("could not open the issue")?;
        let number = self.created_number(&created)?;
        // Forgejo takes only label IDs on creation; the label set takes names.
        if self.kind == ForgeKind::Forgejo && !issue.labels.is_empty() {
            let labels = Action::Edit(Edit {
                add_labels: issue.labels.clone(),
                ..Edit::default()
            });
            return self
                .act(account, ItemKind::Issue, number, &labels)
                .await
                .with_context(|| format!("issue #{number} was opened, but labeling it failed"));
        }
        Ok(self.read(account, ItemKind::Issue, number).await?.item)
    }

    fn issue_request(&self, issue: &NewIssue) -> Request {
        let body = match self.kind {
            ForgeKind::GitHub => {
                json!({ "title": issue.title, "body": issue.body, "labels": issue.labels })
            }
            ForgeKind::Forgejo => json!({ "title": issue.title, "body": issue.body }),
            ForgeKind::GitLab | ForgeKind::Bitbucket => {
                json!({ "title": issue.title, "description": issue.body,
                "labels": issue.labels.join(",") })
            }
        };
        Request::new(HttpMethod::Post, self.repo_endpoint("issues"), Some(body))
    }

    /// The number of the item a creation or list response describes.
    fn created_number(&self, created: &Value) -> Result<u64> {
        created[self.kind.number_field()]
            .as_u64()
            .context("the forge did not report the new item's number")
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
