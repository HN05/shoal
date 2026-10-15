//! Reads for the forges Shoal reaches through their REST APIs, every forge
//! but GitHub, whose CLI reports structured data itself.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    ForgeRepo,
    account::Account,
    item::{Current, ItemState},
    link::ItemKind,
    list,
};

impl ForgeRepo {
    /// The commits of the PR once it has merged, or `None` before. The PR must
    /// have `branch` as its head.
    pub(super) async fn rest_merged_commits(
        &self,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        let pull = self.read(account, ItemKind::Pr, number).await?;
        ensure!(
            pull.item.pr.as_ref().is_some_and(|pr| pr.head == branch),
            "PR does not match the workspace branch"
        );
        if pull.item.state != ItemState::Merged {
            return Ok(None);
        }
        let commits = self
            .forgejo_pages(account, &self.repo_endpoint(&format!("pulls/{number}/commits")))
            .await?;
        commit_shas(&commits).map(Some)
    }

    /// Title, head and base of a PR whose head branch is in this repository.
    pub(super) async fn rest_pull_details(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        let pull = self.read(account, ItemKind::Pr, number).await?;
        self.branches(pull)
    }

    pub(super) async fn rest_open_items(
        &self,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Vec<list::Item>> {
        let endpoint = match kind {
            ItemKind::Issue => "issues?state=open&type=issues",
            ItemKind::Pr => "pulls?state=open",
        };
        let items = self
            .forgejo_pages(account, &self.repo_endpoint(endpoint))
            .await?;
        items.iter().map(list_item).collect()
    }

    fn branches(&self, pull: Current) -> Result<(String, String, String)> {
        let number = pull.item.number;
        // A deleted fork leaves no head repository.
        ensure!(
            pull.head_repository
                .is_some_and(|repository| repository.eq_ignore_ascii_case(&self.path)),
            "PR #{number} comes from a fork; only branches in this repository can be opened"
        );
        let pr = pull.item.pr.context("not a PR")?;
        Ok((pull.item.title, pr.head, pr.base))
    }
}

/// Full commit IDs, refusing anything else so cleanup never matches a prefix.
fn commit_shas(commits: &[Value]) -> Result<Vec<String>> {
    commits
        .iter()
        .map(|commit| {
            commit["sha"]
                .as_str()
                .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .map(str::to_owned)
                .context("PR commit has no full commit ID")
        })
        .collect()
}

fn list_item(item: &Value) -> Result<list::Item> {
    let number = item["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .context("listed item has no number")?;
    let title = item["title"]
        .as_str()
        .context("listed item has no title")?;
    Ok(list::Item {
        number,
        title: title.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::item::{Item, PrSummary};
    use serde_json::json;

    fn pull(head_repository: Option<&str>) -> Current {
        Current {
            item: Item {
                kind: ItemKind::Pr,
                number: 7,
                url: String::new(),
                title: "Fix".into(),
                state: ItemState::Open,
                labels: Vec::new(),
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: false,
                }),
            },
            node_id: None,
            head_repository: head_repository.map(str::to_owned),
            body: String::new(),
        }
    }

    #[test]
    fn only_branches_of_this_repository_can_be_opened() {
        let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            forge.branches(pull(Some("Team/Repo"))).unwrap(),
            ("Fix".into(), "topic".into(), "main".into())
        );
        for fork in [Some("someone/repo"), None] {
            let error = forge.branches(pull(fork)).unwrap_err();
            assert!(error.to_string().contains("fork"), "{error}");
        }
    }

    #[test]
    fn merged_commits_need_full_ids_and_lists_need_numbers() {
        let sha = "a".repeat(40);
        assert_eq!(commit_shas(&[json!({"sha": sha})]).unwrap(), [sha]);
        for commit in [json!({"sha": "abc"}), json!({}), json!({"sha": "g".repeat(40)})] {
            assert!(commit_shas(&[commit]).is_err());
        }
        assert_eq!(
            list_item(&json!({"number": 3, "title": "Bug"})).unwrap(),
            list::Item {
                number: 3,
                title: "Bug".into()
            }
        );
        assert!(list_item(&json!({"number": 0, "title": "Bug"})).is_err());
        assert!(list_item(&json!({"title": "Bug"})).is_err());
    }
}
