//! Reads for the forges Shoal reaches through their REST APIs, every forge
//! but GitHub, whose CLI reports structured data itself.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    ForgeRepo,
    account::Account,
    api::Request,
    item::{Current, ItemState},
    link::ItemKind,
    list,
    pr::state::{Verdict, forgejo_reviews},
};

/// A CI check in the states every REST forge is mapped to; `Pending` is the
/// only incomplete one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestCheck {
    pub name: String,
    pub state: &'static str,
}

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
            .forgejo_pages(
                account,
                &self.repo_endpoint(&format!("pulls/{number}/commits")),
            )
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

    /// The CI checks on the PR's head commit.
    pub(super) async fn rest_checks(
        &self,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull.head_sha.as_deref().context("PR has no head commit")?;
        let endpoint = self.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        let combined = self.send(account, &Request::get(endpoint)).await?;
        forgejo_checks(&combined)
    }

    /// Each submitted review's reviewer and verdict.
    pub(super) async fn rest_verdicts(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let endpoint = self.repo_endpoint(&format!("pulls/{number}/reviews"));
        forgejo_reviews(&self.forgejo_pages(account, &endpoint).await?)
    }

    /// What changes when a comment is added, edited or deleted.
    pub(super) async fn rest_discussion(&self, account: &Account, number: u64) -> Result<String> {
        let endpoint = self.repo_endpoint(&format!("issues/{number}/comments"));
        let comments = self.send(account, &Request::get(endpoint)).await?;
        let comments = comments.as_array().context("comments are not a list")?;
        Ok(fingerprint(comments, &["id", "body", "updated_at"]))
    }

    /// What changes when a review is submitted, edited, dismissed or answered.
    pub(super) async fn rest_review_activity(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<String> {
        let endpoint = self.repo_endpoint(&format!("pulls/{number}/reviews"));
        let reviews = self.forgejo_pages(account, &endpoint).await?;
        Ok(fingerprint(
            &reviews,
            &[
                "id",
                "body",
                "state",
                "comments_count",
                "stale",
                "dismissed",
                "updated_at",
            ],
        ))
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

/// The fields of each item that matter for activity; empty for no items, so
/// an item without comments reads the same as before it had any.
fn fingerprint(items: &[Value], fields: &[&str]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let items: Vec<Value> = items
        .iter()
        .map(|item| {
            fields
                .iter()
                .map(|field| ((*field).to_owned(), item[*field].clone()))
                .collect()
        })
        .collect();
    Value::from(items).to_string()
}

/// Forgejo's combined status; a commit without statuses has `"statuses": null`.
fn forgejo_checks(combined: &Value) -> Result<Vec<RestCheck>> {
    let statuses = match &combined["statuses"] {
        Value::Null => &[][..],
        statuses => statuses.as_array().context("invalid Forgejo CI statuses")?,
    };
    ensure!(
        combined["total_count"].as_u64() == Some(statuses.len() as u64),
        "Forgejo returned a partial page of CI statuses"
    );
    statuses
        .iter()
        .map(|status| {
            let name = status["context"]
                .as_str()
                .context("Forgejo CI status has no context")?;
            let state = match status["status"].as_str() {
                Some("pending") => "Pending",
                Some("success") => "Success",
                Some("failure") => "Failure",
                Some("warning") => "Warning",
                Some("skipped") => "Skipped",
                Some("error") => "Error",
                _ => anyhow::bail!("unknown Forgejo CI state"),
            };
            Ok(RestCheck {
                name: name.into(),
                state,
            })
        })
        .collect()
}

fn list_item(item: &Value) -> Result<list::Item> {
    let number = item["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .context("listed item has no number")?;
    let title = item["title"].as_str().context("listed item has no title")?;
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
            head_sha: None,
            conflict: None,
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
        for commit in [
            json!({"sha": "abc"}),
            json!({}),
            json!({"sha": "g".repeat(40)}),
        ] {
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

    #[test]
    fn forgejo_checks_need_every_status_and_known_states() {
        let combined = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"},
            {"context": "review", "status": "pending"}
        ]});
        assert_eq!(
            forgejo_checks(&combined).unwrap(),
            [
                RestCheck {
                    name: "ci / rust".into(),
                    state: "Success"
                },
                RestCheck {
                    name: "review".into(),
                    state: "Pending"
                },
            ]
        );
        assert!(
            forgejo_checks(&json!({"total_count": 0, "statuses": null}))
                .unwrap()
                .is_empty()
        );
        let partial = json!({"total_count": 3, "statuses": combined["statuses"]});
        assert!(forgejo_checks(&partial).is_err());
        let unknown = json!({"total_count": 1, "statuses": [{"context": "x", "status": "later"}]});
        assert!(forgejo_checks(&unknown).is_err());
    }

    #[test]
    fn fingerprints_ignore_other_fields_and_read_empty_lists_as_nothing() {
        let comment = json!({"id": 1, "body": "hi", "reactions": 1});
        let reacted = json!({"id": 1, "body": "hi", "reactions": 2});
        assert_eq!(
            fingerprint(std::slice::from_ref(&comment), &["id", "body"]),
            fingerprint(&[reacted], &["id", "body"])
        );
        let edited = json!({"id": 1, "body": "edited"});
        assert_ne!(
            fingerprint(&[comment], &["id", "body"]),
            fingerprint(&[edited], &["id", "body"])
        );
        assert_eq!(fingerprint(&[], &["id"]), "");
    }
}
