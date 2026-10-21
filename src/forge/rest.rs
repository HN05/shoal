//! Reads for the forges Shoal reaches through their REST APIs, every forge
//! but GitHub, whose CLI reports structured data itself.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use super::{
    ForgeKind, ForgeRepo,
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
        let pull = self.rest_branch_pull(account, number, Some(branch)).await?;
        if pull.item.state != ItemState::Merged {
            return Ok(None);
        }
        let endpoint = format!("{}/commits", self.item_endpoint(ItemKind::Pr, number));
        let commits = self
            .pages(account, &endpoint)
            .await
            .context(self.kind.login_hint())?;
        commit_shas(&commits, self.kind.commit_field()).map(Some)
    }

    /// The PR, which must have `branch` as its head when one is given. Only a
    /// failed request suggests logging in.
    pub(super) async fn rest_branch_pull(
        &self,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Current> {
        let pull = self
            .read(account, ItemKind::Pr, number)
            .await
            .context(self.kind.login_hint())?;
        if let Some(branch) = branch {
            ensure!(
                pull.item.pr.as_ref().is_some_and(|pr| pr.head == branch),
                "PR does not match the workspace branch"
            );
        }
        Ok(pull)
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
        let endpoint = match (self.kind, kind) {
            (ForgeKind::GitHub | ForgeKind::Forgejo, ItemKind::Issue) => {
                "issues?state=open&type=issues"
            }
            (ForgeKind::GitHub | ForgeKind::Forgejo, ItemKind::Pr) => "pulls?state=open",
            (ForgeKind::GitLab, ItemKind::Issue) => "issues?state=opened",
            (ForgeKind::GitLab, ItemKind::Pr) => "merge_requests?state=opened",
            // state="new" OR state="open" OR state="on hold"
            (ForgeKind::Bitbucket, ItemKind::Issue) => {
                "issues?q=state%3D%22new%22%20OR%20state%3D%22open%22%20OR%20state%3D%22on%20hold%22"
            }
            (ForgeKind::Bitbucket, ItemKind::Pr) => "pullrequests?state=OPEN",
        };
        let items = self.pages(account, &self.repo_endpoint(endpoint)).await?;
        items
            .iter()
            .map(|item| list_item(item, self.kind.number_field()))
            .collect()
    }

    /// The CI checks on the PR's head commit.
    pub(super) async fn rest_checks(
        &self,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull.head_sha.as_deref().context("PR has no head commit")?;
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => self.forgejo_checks(account, head).await,
            ForgeKind::GitLab => self.gitlab_checks(account, head).await,
            ForgeKind::Bitbucket => self.bitbucket_checks(account, pull.item.number).await,
        }
    }

    /// Each submitted review's reviewer and verdict.
    pub(super) async fn rest_verdicts(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => {
                let endpoint = self.repo_endpoint(&format!("pulls/{number}/reviews"));
                forgejo_reviews(&self.pages(account, &endpoint).await?)
            }
            ForgeKind::GitLab => self.gitlab_verdicts(account, number).await,
            ForgeKind::Bitbucket => self.bitbucket_verdicts(account, number).await,
        }
    }

    /// Each comment by ID, with what changes when it is added or edited.
    pub(super) async fn rest_discussion(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => {
                // Every page: a comment missing from the set reads as deleted.
                let endpoint = self.repo_endpoint(&format!("issues/{number}/comments"));
                let comments = self.pages(account, &endpoint).await?;
                fingerprints(&comments, &["body", "updated_at"])
            }
            ForgeKind::GitLab => {
                let notes = self.gitlab_notes(account, kind, number).await?;
                fingerprints(&notes, &["body", "updated_at", "resolved"])
            }
            ForgeKind::Bitbucket => {
                let endpoint = format!("{}/comments", self.item_endpoint(kind, number));
                let comments = self.pages(account, &endpoint).await?;
                fingerprints(
                    &comments,
                    &["content", "updated_on", "deleted", "resolution"],
                )
            }
        }
    }

    /// Each review by ID with what changes when it is submitted, edited,
    /// dismissed or answered.
    pub(super) async fn rest_review_activity(
        &self,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => {
                let endpoint = self.repo_endpoint(&format!("pulls/{number}/reviews"));
                let reviews = self.pages(account, &endpoint).await?;
                fingerprints(
                    &reviews,
                    &[
                        "body",
                        "state",
                        "comments_count",
                        "stale",
                        "dismissed",
                        "updated_at",
                    ],
                )
            }
            // Inline comments are part of the discussion; verdicts have no
            // ID, so they change as one entry.
            ForgeKind::GitLab | ForgeKind::Bitbucket => Ok(vec![(
                "verdicts".into(),
                self.rest_verdicts(account, number)
                    .await?
                    .iter()
                    .map(|(reviewer, verdict)| format!("{reviewer}:{verdict:?}\n"))
                    .collect(),
            )]),
        }
    }

    async fn forgejo_checks(&self, account: &Account, head: &str) -> Result<Vec<RestCheck>> {
        let endpoint = self.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        let combined = self.send(account, &Request::get(endpoint)).await?;
        forgejo_statuses(&combined)
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
fn commit_shas(commits: &[Value], field: &str) -> Result<Vec<String>> {
    commits
        .iter()
        .map(|commit| {
            commit[field]
                .as_str()
                .filter(|sha| sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .map(str::to_owned)
                .context("PR commit has no full commit ID")
        })
        .collect()
}

/// Each item's ID with the fields of it that matter for activity.
pub(super) fn fingerprints(items: &[Value], fields: &[&str]) -> Result<Vec<(String, String)>> {
    items
        .iter()
        .map(|item| {
            let id = item.get("id").context("comment or review has no ID")?;
            let fields: serde_json::Map<String, Value> = fields
                .iter()
                .map(|field| ((*field).to_owned(), item[*field].clone()))
                .collect();
            Ok((
                crate::forge::updates::id_text(id),
                Value::from(fields).to_string(),
            ))
        })
        .collect()
}

/// Forgejo's combined status; a commit without statuses has `"statuses": null`.
fn forgejo_statuses(combined: &Value) -> Result<Vec<RestCheck>> {
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

fn list_item(item: &Value, field: &str) -> Result<list::Item> {
    let number = item[field]
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
            base_sha: None,
            conflict: None,
            reviewers: Vec::new(),
            user_ids: Default::default(),
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
        assert_eq!(commit_shas(&[json!({"id": sha})], "id").unwrap(), [sha]);
        for commit in [
            json!({"sha": "abc"}),
            json!({}),
            json!({"sha": "g".repeat(40)}),
        ] {
            assert!(commit_shas(&[commit], "sha").is_err());
        }
        assert_eq!(
            list_item(&json!({"iid": 3, "id": 9, "title": "Bug"}), "iid").unwrap(),
            list::Item {
                number: 3,
                title: "Bug".into()
            }
        );
        assert!(list_item(&json!({"number": 0, "title": "Bug"}), "number").is_err());
        assert!(list_item(&json!({"title": "Bug"}), "number").is_err());
    }

    #[test]
    fn forgejo_statuses_need_every_status_and_known_states() {
        let combined = json!({"total_count": 2, "statuses": [
            {"context": "ci / rust", "status": "success"},
            {"context": "review", "status": "pending"}
        ]});
        assert_eq!(
            forgejo_statuses(&combined).unwrap(),
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
            forgejo_statuses(&json!({"total_count": 0, "statuses": null}))
                .unwrap()
                .is_empty()
        );
        let partial = json!({"total_count": 3, "statuses": combined["statuses"]});
        assert!(forgejo_statuses(&partial).is_err());
        let unknown = json!({"total_count": 1, "statuses": [{"context": "x", "status": "later"}]});
        assert!(forgejo_statuses(&unknown).is_err());
    }

    #[test]
    fn fingerprints_key_items_by_id_and_ignore_other_fields() {
        let comment = json!({"id": 1, "body": "hi", "reactions": 1});
        let reacted = json!({"id": 1, "body": "hi", "reactions": 2});
        assert_eq!(
            fingerprints(std::slice::from_ref(&comment), &["body"]).unwrap(),
            fingerprints(&[reacted], &["body"]).unwrap()
        );
        let edited = json!({"id": 1, "body": "edited"});
        let edited = fingerprints(&[edited], &["body"]).unwrap();
        assert_eq!(edited[0].0, "1");
        assert_ne!(fingerprints(&[comment], &["body"]).unwrap(), edited);
        assert!(fingerprints(&[json!({"body": "hi"})], &["body"]).is_err());
    }
}
