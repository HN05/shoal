//! The repository's labels as the forge lists them, so agents can follow
//! the scheme the repository describes in its label descriptions.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ForgeKind, ForgeRepo, account::Account};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub description: String,
    /// Whether an item takes at most one label of its scope: Forgejo's
    /// exclusive labels and GitLab's scoped `key::value` labels.
    pub exclusive: bool,
}

impl ForgeRepo {
    /// Every label of the repository, in the forge's order; Bitbucket has none.
    pub async fn labels(&self, account: &Account) -> Result<Vec<Label>> {
        if self.kind == ForgeKind::Bitbucket {
            return Ok(Vec::new());
        }
        let listed = self.pages(account, &self.repo_endpoint("labels")).await?;
        Ok(listed
            .iter()
            .filter_map(|label| self.label(label))
            .collect())
    }

    fn label(&self, label: &Value) -> Option<Label> {
        let name = label["name"].as_str()?.to_owned();
        let exclusive = match self.kind {
            ForgeKind::Forgejo => label["exclusive"].as_bool().unwrap_or(false),
            ForgeKind::GitLab => name.contains("::"),
            ForgeKind::GitHub | ForgeKind::Bitbucket => false,
        };
        Some(Label {
            description: label["description"].as_str().unwrap_or_default().to_owned(),
            name,
            exclusive,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_read_descriptions_and_exclusivity_per_forge() {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            forgejo.label(&json!({"name": "type/bug", "description": "Broken", "exclusive": true})),
            Some(Label {
                name: "type/bug".into(),
                description: "Broken".into(),
                exclusive: true,
            })
        );
        let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        let label = github
            .label(&json!({"name": "bug", "description": null}))
            .unwrap();
        assert_eq!((label.description.as_str(), label.exclusive), ("", false));
        let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
        assert!(
            gitlab
                .label(&json!({"name": "priority::high"}))
                .unwrap()
                .exclusive
        );
        assert!(!gitlab.label(&json!({"name": "bug"})).unwrap().exclusive);
        assert_eq!(forgejo.label(&json!({"id": 3})), None);
    }
}
