//! Reads for the forges Shoal reaches through their REST APIs, every forge
//! but GitHub, whose CLI reports structured data itself.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, LoginHint},
    items::{
        issue::require_open,
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    review::status::{PrState, PrStatus, Verdict},
    updates::{DISCUSSION, Snapshot},
};

/// A CI check in the states every REST forge is mapped to; `Pending` is the
/// only incomplete one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestCheck {
    pub name: String,
    pub state: &'static str,
}

/// What the forges reached through their REST APIs each read their own way,
/// for the activity and status they otherwise read alike.
pub(crate) trait RestReads: Backend {
    /// Each comment of the item's discussion by ID, with what changes when
    /// it is added or edited. Every page: a comment missing from the set
    /// reads as deleted.
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, String)>>;

    /// Each review by ID with what changes when it is submitted, edited,
    /// dismissed or answered. By default inline comments are part of the
    /// discussion and verdicts have no ID, so they change as one entry.
    async fn review_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        Ok(vec![(
            "verdicts".into(),
            self.verdicts(repo, account, number)
                .await?
                .iter()
                .map(|(reviewer, verdict)| format!("{reviewer}:{verdict:?}\n"))
                .collect(),
        )])
    }

    /// The CI checks on the PR's head commit.
    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>>;

    /// Each submitted review's reviewer and verdict.
    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>>;
}

/// An issue's state and discussion.
pub(crate) async fn issue_activity(
    reads: &impl RestReads,
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Snapshot> {
    async {
        let issue = repo.read(account, ItemKind::Issue, number).await?;
        let mut snapshot = Snapshot::new(issue.item.state.as_str());
        let discussion = reads
            .discussion(repo, account, ItemKind::Issue, number)
            .await;
        snapshot.record_comments(DISCUSSION, discussion);
        Ok::<_, anyhow::Error>(snapshot)
    }
    .await
    .hint_login(reads)
}

/// A PR's state, discussion, reviews and, while it is open, mergeability
/// and checks. The PR must have `branch` as its head when one is given.
pub(crate) async fn pr_activity(
    reads: &impl RestReads,
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    branch: Option<&str>,
) -> Result<Snapshot> {
    let pull = branch_pull(repo, account, number, branch).await?;
    let mut snapshot = Snapshot::new(pull.item.state.as_str());
    let discussion = reads.discussion(repo, account, ItemKind::Pr, number).await;
    snapshot.record_comments(DISCUSSION, discussion);
    let reviews = reads.review_activity(repo, account, number).await;
    snapshot.record_comments("reviews", reviews);
    if pull.item.state == ItemState::Open {
        snapshot.record_checks(&pull, reads.checks(repo, account, &pull).await);
    }
    Ok(snapshot)
}

/// A PR's state, mergeability, checks and review state.
pub(crate) async fn pr_status(
    reads: &impl RestReads,
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    url: &str,
) -> Result<PrStatus> {
    async {
        let pull = repo.read(account, ItemKind::Pr, number).await?;
        let state = match pull.item.state {
            ItemState::Open => PrState::Open,
            ItemState::Merged => PrState::Merged,
            ItemState::Closed => PrState::Closed,
        };
        let mut status = PrStatus::new(url.into(), Some(state));
        if state == PrState::Open {
            let mut snapshot = Snapshot::default();
            snapshot.record_checks(&pull, reads.checks(repo, account, &pull).await);
            status.record_snapshot(snapshot);
        }
        status.record_reviews(reads.verdicts(repo, account, number).await);
        Ok::<_, anyhow::Error>(status)
    }
    .await
    .hint_login(reads)
}

/// The commits of the PR once it has merged, or `None` before. The PR must
/// have `branch` as its head.
pub(crate) async fn merged_commits(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    branch: &str,
) -> Result<Option<Vec<String>>> {
    let pull = branch_pull(repo, account, number, Some(branch)).await?;
    if pull.item.state != ItemState::Merged {
        return Ok(None);
    }
    let endpoint = format!("{}/commits", repo.item_endpoint(ItemKind::Pr, number));
    let commits = repo
        .pages(account, &endpoint)
        .await
        .hint_login(&repo.kind)?;
    commit_shas(&commits, repo.kind.commit_field()).map(Some)
}

/// The PR, which must have `branch` as its head when one is given. Only a
/// failed request suggests logging in.
pub(crate) async fn branch_pull(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    branch: Option<&str>,
) -> Result<Current> {
    let pull = repo
        .read(account, ItemKind::Pr, number)
        .await
        .hint_login(&repo.kind)?;
    if let Some(branch) = branch {
        ensure!(
            pull.item.pr.as_ref().is_some_and(|pr| pr.head == branch),
            "PR does not match the workspace branch"
        );
    }
    Ok(pull)
}

/// Title, head and base of a PR whose head branch is in this repository.
pub(crate) async fn pull_details(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<(String, String, String)> {
    let pull = repo.read(account, ItemKind::Pr, number).await?;
    branches(repo, pull)
}

/// Up to `search.limit` items of `kind` that a list endpoint under the
/// repository returns, keeping those in a state the search asks for.
pub(crate) async fn list_items(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    endpoint: &str,
    search: &list::Search,
) -> Result<Vec<Item>> {
    listed(repo, account, endpoint, search.limit, |value| {
        let item = repo.current(kind, listed_number(repo, value)?, value)?.item;
        Ok(search.includes(item.state).then_some(item))
    })
    .await
}

/// The numbers of up to `limit` items a list endpoint under the repository
/// returns.
pub(crate) async fn listed_numbers(
    repo: &ForgeRepo,
    account: &Account,
    endpoint: &str,
    limit: usize,
) -> Result<Vec<u64>> {
    listed(repo, account, endpoint, limit, |value| {
        listed_number(repo, value).map(Some)
    })
    .await
}

/// Up to `limit` entries of a list endpoint under the repository as `read`
/// reads them, skipping those it reads as none, page by page until enough.
async fn listed<T>(
    repo: &ForgeRepo,
    account: &Account,
    endpoint: &str,
    limit: usize,
    mut read: impl FnMut(&Value) -> Result<Option<T>>,
) -> Result<Vec<T>> {
    let mut entries = Vec::new();
    repo.pages_while(account, &repo.repo_endpoint(endpoint), |batch| {
        for value in batch {
            entries.extend(read(value)?);
        }
        Ok(entries.len() < limit)
    })
    .await
    .hint_login(&repo.kind)?;
    entries.truncate(limit);
    Ok(entries)
}

fn listed_number(repo: &ForgeRepo, value: &Value) -> Result<u64> {
    value[repo.kind.number_field()]
        .as_u64()
        .filter(|number| *number > 0)
        .context("listed item has no number")
}

/// An issue's state, read from its REST item.
pub(crate) async fn issue_state(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<ItemState> {
    Ok(repo
        .read(account, ItemKind::Issue, number)
        .await
        .hint_login(&repo.kind)?
        .item
        .state)
}

/// An open issue's title and description.
pub(crate) async fn issue_details(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<(String, String)> {
    let issue = repo
        .read(account, ItemKind::Issue, number)
        .await
        .hint_login(&repo.kind)?;
    require_open(issue.item.state)?;
    Ok((issue.item.title, issue.body))
}

fn branches(repo: &ForgeRepo, pull: Current) -> Result<(String, String, String)> {
    let number = pull.item.number;
    // A deleted fork leaves no head repository.
    ensure!(
        pull.head_repository
            .is_some_and(|repository| repository.eq_ignore_ascii_case(&repo.path)),
        "PR #{number} comes from a fork; only branches in this repository can be opened"
    );
    let pr = pull.item.pr.context("not a PR")?;
    Ok((pull.item.title, pr.head, pr.base))
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
pub(crate) fn fingerprints(items: &[Value], fields: &[&str]) -> Result<Vec<(String, String)>> {
    items
        .iter()
        .map(|item| {
            let id = item.get("id").context("comment or review has no ID")?;
            let fields: serde_json::Map<String, Value> = fields
                .iter()
                .map(|field| ((*field).to_owned(), item[*field].clone()))
                .collect();
            Ok((crate::updates::id_text(id), Value::from(fields).to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::item::PrSummary;
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
            replaced_labels: Vec::new(),
        }
    }

    #[test]
    fn only_branches_of_this_repository_can_be_opened() {
        let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert_eq!(
            branches(&forge, pull(Some("Team/Repo"))).unwrap(),
            ("Fix".into(), "topic".into(), "main".into())
        );
        for fork in [Some("someone/repo"), None] {
            let error = branches(&forge, pull(fork)).unwrap_err();
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
        let forge = ForgeRepo::parse("https://gitlab.com/team/repo").unwrap();
        assert_eq!(
            listed_number(&forge, &json!({"iid": 3, "id": 9})).unwrap(),
            3
        );
        let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert!(listed_number(&forge, &json!({"number": 0})).is_err());
        assert!(listed_number(&forge, &json!({"title": "Bug"})).is_err());
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
