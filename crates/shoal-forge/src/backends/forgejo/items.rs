//! Forgejo's issues and PRs: reading, listing, opening, editing and commenting
//! on them, and finding a PR by its head.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use shoal_refs::BranchName;

use super::{Forgejo, actions};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{
        Items, LoginHint,
        github_rest::{self, GitHubShapes},
        rest,
    },
    conventions::draft::DraftMarker,
    items::{
        action::Edit,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, text},
        link::ItemKind,
        list,
        reaction::Subject,
    },
    numbers::{CommentId, IssueNumber, PullNumber},
    transport::api::{HttpMethod, Request, encode},
    updates::DISCUSSION,
};

impl Items for Forgejo {
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

    /// Forgejo searches PRs only through its issue list, which leaves out
    /// their branches, so each listed PR is read in full.
    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>> {
        let endpoint = search_endpoint(kind, search);
        if kind == ItemKind::Issue {
            return rest::list_items(repo, account, kind, &endpoint, search).await;
        }
        let numbers = rest::listed_numbers(repo, account, &endpoint, search.limit).await?;
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
        github_rest::open_blockers(repo, account, &format!("issues/{number}/dependencies")).await
    }

    fn pull_head_ref(&self, number: PullNumber) -> Option<String> {
        Some(format!("refs/pull/{number}/head"))
    }

    /// Forgejo keeps one exclusive label of a scope from the set it is
    /// given, not necessarily the added one, so the added one replaces the
    /// item's.
    async fn prepare_edit(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        edit: &Edit,
        current: &mut Current,
    ) -> Result<()> {
        // Only an item with a label of an added label's scope lists the
        // repository's labels to learn which are exclusive.
        if replaced_labels(edit, &current.item.labels, |_| true).is_empty() {
            return Ok(());
        }
        let exclusive: std::collections::HashSet<_> = repo
            .labels(account)
            .await?
            .into_iter()
            .filter(|label| label.exclusive)
            .map(|label| label.name)
            .collect();
        current.replaced_labels =
            replaced_labels(edit, &current.item.labels, |name| exclusive.contains(name));
        Ok(())
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        github_rest::edit(repo, current, edit, self)
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        github_rest::comment(repo, item, body)
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        Ok(github_rest::set_state(repo, item, state))
    }

    fn comment_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "id")
    }

    /// Forgejo counts a reply in its review.
    fn reply_key(&self) -> (&'static str, &'static str) {
        ("reviews", "pull_request_review_id")
    }

    /// Forgejo cannot filter its list by head branch.
    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _head: &Head<'_>,
    ) -> Result<Vec<Value>> {
        repo.pages(account, &repo.repo_endpoint("pulls?state=open"))
            .await
    }

    fn has_head(&self, _repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        github_rest::has_head(pull, head)
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        let endpoint = repo.repo_endpoint(&format!("branches/{}", encode(branch, false)));
        (Request::get(endpoint), "/commit/id")
    }

    /// Forgejo keeps draft state in the title.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let title = if pull.draft {
            DRAFT.draft_title(&pull.title, true)
        } else {
            pull.title.clone()
        };
        let head = github_rest::head_spec(pull);
        let body = json!({"title": title, "body": pull.body, "head": head, "base": pull.base});
        Request::new(HttpMethod::Post, repo.repo_endpoint("pulls"), Some(body))
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "body": issue.body });
        Request::new(HttpMethod::Post, repo.repo_endpoint("issues"), Some(body))
    }

    /// Forgejo takes only label IDs when opening an issue; the label set
    /// takes names.
    fn labels_issue_after_opening(&self) -> bool {
        true
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

impl GitHubShapes for Forgejo {
    /// Forgejo keeps draft state in the title.
    fn draft_marker(&self) -> Option<&DraftMarker> {
        Some(&DRAFT)
    }

    /// Forgejo reports a draft as not mergeable, whatever its conflicts.
    fn conflict(&self, mergeable: bool, draft: bool) -> bool {
        !mergeable && !draft
    }

    /// Once a PR's branch is deleted, Forgejo reports the PR's own ref as its
    /// head and keeps the branch name only as the head's label.
    fn head_branch(&self, number: PullNumber, pull: &Value) -> Result<BranchName> {
        let head = github_rest::head_ref(pull)?;
        if head == format!("refs/pull/{number}/head") {
            return (text(&pull["head"]["label"]).map(BranchName::from))
                .context("PR has no head branch");
        }
        Ok(head.into())
    }

    /// Forgejo clears a milestone with ID 0.
    fn cleared_milestone(&self) -> Value {
        0.into()
    }

    fn status_field(&self) -> &'static str {
        "status"
    }

    fn status_waits(&self, status: &Value) -> bool {
        actions::status_waits(status)
    }

    /// Forgejo keeps a PR's inline comments among its issue comments.
    fn review_comment_path(&self, id: CommentId) -> String {
        format!("issues/comments/{id}")
    }
}

/// Forgejo treats a PR whose title starts with one of these prefixes as a
/// draft.
const DRAFT: DraftMarker = DraftMarker {
    prefixes: &["WIP:", "[WIP]"],
    marker: "WIP:",
};

/// The `labels` that an exclusive label `edit` adds replaces: the other
/// exclusive labels of its scope.
fn replaced_labels(
    edit: &Edit,
    labels: &[String],
    exclusive: impl Fn(&str) -> bool,
) -> Vec<String> {
    let replaces = |label: &String, added: &String| {
        label != added
            && exclusive(label)
            && exclusive(added)
            && exclusive_scope(label).is_some_and(|scope| exclusive_scope(added) == Some(scope))
    };
    labels
        .iter()
        .filter(|label| edit.add_labels.iter().any(|added| replaces(label, added)))
        .cloned()
        .collect()
}

/// The scope of a Forgejo label that allows one exclusive label of it: its
/// name before the last `/`, when neither side is empty.
fn exclusive_scope(name: &str) -> Option<&str> {
    let (scope, value) = name.rsplit_once('/')?;
    (!scope.is_empty() && !value.is_empty()).then_some(scope)
}

/// The issue list endpoint that runs `search` for items of `kind`.
fn search_endpoint(kind: ItemKind, search: &list::Search) -> String {
    let kind = match kind {
        ItemKind::Issue => "issues",
        ItemKind::Pr => "pulls",
    };
    let mut endpoint = format!("issues?type={kind}&state={}", search.state);
    if !search.labels.is_empty() {
        endpoint += &format!("&labels={}", encode(&search.labels.join(","), false));
    }
    if let Some(author) = &search.author {
        endpoint += &format!("&created_by={}", encode(author, false));
    }
    if let Some(text) = &search.text {
        endpoint += &format!("&q={}", encode(text, false));
    }
    endpoint
}

#[cfg(test)]
mod tests;
