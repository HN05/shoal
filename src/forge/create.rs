//! Open issues and PRs through the forge's REST API.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    action::{Action, Edit},
    api::{HttpMethod, Request},
    item::Item,
    link::ItemKind,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIssue {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

impl ForgeRepo {
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
}
