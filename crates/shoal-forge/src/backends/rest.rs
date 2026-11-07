//! Reads shared by the forges Shoal reaches through their REST APIs; GitHub
//! keeps its own activity reads.
use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, LoginHint},
    ci::check_state::CheckState,
    items::{
        issue::require_open,
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    review::status::{PrState, PrStatus, Verdict},
    updates::{DISCUSSION, Entry, Snapshot, verdict_entries},
};

/// A CI check, by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestCheck {
    pub name: String,
    pub state: CheckState,
}

/// What the forges reached through their REST APIs each read their own way,
/// for the activity and status they otherwise read alike.
pub(crate) trait RestReads: Backend {
    /// Each comment of the item's discussion by ID, with what changes when
    /// it is added or edited and its author. Every page: a comment missing
    /// from the set reads as deleted.
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>>;

    /// Each review by ID with what changes when it is submitted, edited,
    /// dismissed or answered and its reviewer. By default inline comments
    /// are part of the discussion and verdicts have no ID, so each
    /// reviewer's verdicts are one entry.
    async fn review_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let verdicts = self.verdicts(repo, account, number).await?;
        Ok(verdict_entries(verdicts.into_iter().map(
            |(reviewer, verdict)| (reviewer, format!("{verdict:?}")),
        )))
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
        let (issue, discussion) = tokio::join!(
            repo.read(account, ItemKind::Issue, number),
            reads.discussion(repo, account, ItemKind::Issue, number),
        );
        let mut snapshot = Snapshot::item(&issue?);
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
    let (discussion, reviews, checks) = tokio::join!(
        reads.discussion(repo, account, ItemKind::Pr, number),
        reads.review_activity(repo, account, number),
        open_checks(reads, repo, account, &pull),
    );
    let mut snapshot = Snapshot::item(&pull);
    snapshot.record_comments(DISCUSSION, discussion);
    snapshot.record_comments("reviews", reviews);
    if let Some(checks) = checks {
        snapshot.record_checks(&pull, checks);
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
        let (checks, verdicts) = tokio::join!(
            open_checks(reads, repo, account, &pull),
            reads.verdicts(repo, account, number),
        );
        let mut status = PrStatus::new(url.into(), Some(PrState::from(pull.item.state)));
        if let Some(checks) = checks {
            let mut snapshot = Snapshot::default();
            snapshot.record_checks(&pull, checks);
            status.record_snapshot(snapshot);
        }
        status.record_reviews(verdicts);
        Ok::<_, anyhow::Error>(status)
    }
    .await
    .hint_login(reads)
}

/// The checks on `pull`'s head while it is open; a closed PR has none to read.
async fn open_checks(
    reads: &impl RestReads,
    repo: &ForgeRepo,
    account: &Account,
    pull: &Current,
) -> Option<Result<Vec<RestCheck>>> {
    if pull.item.state != ItemState::Open {
        return None;
    }
    Some(reads.checks(repo, account, pull).await)
}

/// The commits of the PR once it has merged, or `None` before. The PR must
/// have `branch` as its head.
pub(crate) async fn merged_commits(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    branch: Option<&str>,
) -> Result<Option<Vec<String>>> {
    let pull = branch_pull(repo, account, number, branch).await?;
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

/// How many listed PRs are read at once.
const PULL_READS: usize = 8;

/// The PRs `numbers` names, each read in full, in that order.
pub(crate) async fn read_pulls(
    repo: &ForgeRepo,
    account: &Account,
    numbers: Vec<u64>,
) -> Result<Vec<Item>> {
    futures_util::stream::iter(numbers)
        .map(|number| async move {
            Ok::<_, anyhow::Error>(repo.read(account, ItemKind::Pr, number).await?.item)
        })
        .buffered(PULL_READS)
        .try_collect()
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

/// An open issue.
pub(crate) async fn issue_details(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
) -> Result<Current> {
    let issue = repo
        .read(account, ItemKind::Issue, number)
        .await
        .hint_login(&repo.kind)?;
    require_open(issue.item.state)?;
    Ok(issue)
}

/// Full commit IDs, refusing anything else so cleanup never matches a prefix.
pub(crate) fn commit_shas(commits: &[Value], field: &str) -> Result<Vec<String>> {
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

/// Each item's ID with the fields of it that matter for activity and its
/// author, which `author` reads from the item.
pub(crate) fn fingerprints(
    items: &[Value],
    fields: &[&str],
    author: fn(&Value) -> String,
) -> Result<Vec<(String, Entry)>> {
    items
        .iter()
        .map(|item| {
            let id = item.get("id").context("comment or review has no ID")?;
            let fields: serde_json::Map<String, Value> = fields
                .iter()
                .map(|field| ((*field).to_owned(), item[*field].clone()))
                .collect();
            let fingerprint = Value::from(fields).to_string();
            Ok((
                crate::updates::id_text(id),
                Entry::new(fingerprint, author(item)),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;
