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
                self.forgejo_pages(account, &self.repo_endpoint("pulls?state=open"))
                    .await?
            }
        };
        pulls
            .iter()
            .find(|pull| {
                pull["head"]["ref"] == branch
                    && pull["head"]["repo"]["full_name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case(&self.path))
            })
            .map(created_number)
            .transpose()
    }

    /// Open the PR and return it as the forge reports it.
    pub(crate) async fn create_pull(&self, account: &Account, pull: &NewPull) -> Result<Item> {
        let created = self
            .send(account, &self.create_pull_request(pull))
            .await
            .context("could not open the PR")?;
        let number = created_number(&created)?;
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
        let mut body = json!({
            "title": pull.title,
            "body": pull.body,
            "head": pull.head,
            "base": pull.base,
        });
        match self.kind {
            ForgeKind::GitHub => body["draft"] = pull.draft.into(),
            ForgeKind::Forgejo if pull.draft => {
                body["title"] = draft_title(&pull.title, true).into()
            }
            ForgeKind::Forgejo => {}
        }
        Request::new(HttpMethod::Post, self.repo_endpoint("pulls"), Some(body))
    }

    /// Open the issue and return it as the forge reports it.
    pub(crate) async fn create_issue(&self, account: &Account, issue: &NewIssue) -> Result<Item> {
        let created = self
            .send(account, &self.issue_request(issue))
            .await
            .context("could not open the issue")?;
        let number = created_number(&created)?;
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
        let mut body = json!({ "title": issue.title, "body": issue.body });
        if self.kind == ForgeKind::GitHub {
            body["labels"] = json!(issue.labels);
        }
        Request::new(HttpMethod::Post, self.repo_endpoint("issues"), Some(body))
    }
}

/// The number of the item a creation response describes.
pub(crate) fn created_number(created: &Value) -> Result<u64> {
    created["number"]
        .as_u64()
        .context("the forge did not report the new item's number")
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
        assert!(created_number(&json!({"id": 1})).is_err());
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
