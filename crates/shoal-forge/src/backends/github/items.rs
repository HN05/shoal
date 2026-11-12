//! GitHub's issues and PRs: reading, listing, opening, editing and commenting
//! on them, and finding a PR by its head.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::GitHub;
use crate::{
    ForgeRepo,
    account::Account,
    backends::github_rest::GitHubShapes,
    backends::{Items, LoginHint, github_rest, rest},
    conventions::draft::DraftMarker,
    items::{
        action::Edit,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
        reaction::Subject,
    },
    numbers::CommentId,
    numbers::{IssueNumber, PullNumber},
    transport::api::{HttpMethod, Request, encode},
    updates::DISCUSSION,
};

impl Items for GitHub {
    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        github_rest::current(repo, kind, number, value, self)
    }

    fn numbers_items_together(&self) -> bool {
        true
    }

    /// GitHub filters by text only through its search, which leaves out
    /// a PR's branches, so each listed PR is read in full.
    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>> {
        let found = searched(repo, account, kind, search)
            .await
            .hint_login(self)?;
        if kind == ItemKind::Issue {
            return found
                .iter()
                .map(|value| Ok(repo.current(kind, item_number(value)?, value)?.item))
                .collect();
        }
        let numbers = found.iter().map(item_number).collect::<Result<_>>()?;
        rest::read_pulls(repo, account, numbers)
            .await
            .hint_login(self)
    }

    async fn open_issue_blockers(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Vec<String>> {
        let endpoint = format!("issues/{number}/dependencies/blocked_by");
        github_rest::open_blockers(repo, account, &endpoint).await
    }

    fn pull_head_ref(&self, number: PullNumber) -> Option<String> {
        Some(format!("refs/pull/{number}/head"))
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let mut requests = github_rest::edit(repo, current, edit, self)?;
        if let Some(draft) = github_rest::draft_change(current, edit) {
            requests.push(draft_request(current, draft)?);
        }
        Ok(requests)
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        github_rest::comment(repo, item, body)
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        Ok(github_rest::set_state(repo, item, state))
    }

    /// GitHub reads discussion comments by GraphQL node ID, which REST
    /// returns as `node_id`.
    fn comment_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "node_id")
    }

    fn reply_key(&self) -> (&'static str, &'static str) {
        ("inline", "id")
    }

    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &Head<'_>,
    ) -> Result<Vec<Value>> {
        let head = encode(&format!("{}:{}", head.owner(), head.branch), false);
        let endpoint = repo.repo_endpoint(&format!("pulls?state=open&head={head}"));
        repo.send(account, &Request::get(endpoint))
            .await?
            .as_array()
            .cloned()
            .context("the PR list is not a list")
    }

    fn has_head(&self, _repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        github_rest::has_head(pull, head)
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        let endpoint = repo.repo_endpoint(&format!("branches/{}", encode(branch, true)));
        (Request::get(endpoint), "/commit/sha")
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let head = github_rest::head_spec(pull);
        let body = json!({"title": pull.title, "body": pull.body, "head": head,
            "base": pull.base, "draft": pull.draft});
        Request::new(HttpMethod::Post, repo.repo_endpoint("pulls"), Some(body))
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "body": issue.body, "labels": issue.labels });
        Request::new(HttpMethod::Post, repo.repo_endpoint("issues"), Some(body))
    }

    fn labels_issue_after_opening(&self) -> bool {
        false
    }

    fn edit_comment(
        &self,
        repo: &ForgeRepo,
        _kind: ItemKind,
        _number: u64,
        subject: Subject,
        body: &str,
    ) -> Result<Request> {
        github_rest::edit_comment(repo, subject, body, self)
    }
}

impl GitHubShapes for GitHub {
    /// GitHub keeps draft state in the PR's `draft` field.
    fn draft_marker(&self) -> Option<&DraftMarker> {
        None
    }

    fn cleared_milestone(&self) -> Value {
        Value::Null
    }

    fn status_field(&self) -> &'static str {
        "state"
    }

    fn review_comment_path(&self, id: CommentId) -> String {
        format!("pulls/comments/{id}")
    }
}

/// The most items one search page returns.
const SEARCH_PAGE: usize = 100;

/// Up to `search.limit` issues or PRs of the repository that GitHub's
/// search matches, newest first.
async fn searched(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    search: &list::Search,
) -> Result<Vec<Value>> {
    let query = encode(&search_query(repo, kind, search), false);
    let mut found = Vec::new();
    for page in 1.. {
        let endpoint = format!(
            "search/issues?q={query}&sort=created&order=desc&per_page={SEARCH_PAGE}&page={page}"
        );
        let response = repo.send(account, &Request::get(endpoint)).await?;
        let items = response["items"]
            .as_array()
            .context("GitHub's search returned no item list")?;
        found.extend(items.iter().cloned());
        if items.len() < SEARCH_PAGE || found.len() >= search.limit {
            break;
        }
    }
    found.truncate(search.limit);
    Ok(found)
}

/// GitHub's search query for `search` among the repository's items of `kind`.
fn search_query(repo: &ForgeRepo, kind: ItemKind, search: &list::Search) -> String {
    let kind = match kind {
        ItemKind::Issue => "issue",
        ItemKind::Pr => "pr",
    };
    let mut terms = vec![format!("repo:{}", repo.path), format!("is:{kind}")];
    match search.state {
        list::StateFilter::Open => terms.push("is:open".into()),
        // GitHub counts merged PRs as closed.
        list::StateFilter::Closed => terms.push("is:closed".into()),
        list::StateFilter::All => {}
    }
    terms.extend(
        search
            .labels
            .iter()
            .map(|label| format!("label:\"{label}\"")),
    );
    terms.extend(
        search
            .author
            .iter()
            .map(|author| format!("author:{author}")),
    );
    terms.extend(search.text.clone());
    terms.join(" ")
}

fn item_number(value: &Value) -> Result<u64> {
    value["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .context("listed item has no number")
}

/// GitHub changes draft state only through GraphQL.
fn draft_request(current: &Current, draft: bool) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    let mutation = if draft {
        "convertPullRequestToDraft"
    } else {
        "markPullRequestReadyForReview"
    };
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({
            "query": format!(
                "mutation($id: ID!) {{ {mutation}(input: {{pullRequestId: $id}}) {{ clientMutationId }} }}"
            ),
            "variables": { "id": id },
        })),
    ))
}

#[cfg(test)]
mod tests;
