//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the account's token for the host, else
//! anonymously.
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) mod actions;
mod activity;
mod attachments;
mod checks;
mod conventions;
mod merging;
mod organization;
mod reactions;
mod reviews;
mod runners;
mod runs;
pub(crate) mod threads;
mod version;

use super::{
    Backend, LoginHint,
    github_rest::{self, GitHubShapes},
    rest,
};
use crate::{
    ForgeRepo,
    account::Account,
    capability::{self, Capability, Support},
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
    repo::locator::ItemRoute,
    transport::{
        api::{HttpMethod, Request, encode, full_page},
        response::Response,
    },
    updates::DISCUSSION,
};

/// Why an older Forgejo, or Gitea, runs no jobs on Shoal's runner.
const RUNNER_RELEASE: &str = "that needs Forgejo 15 or later";

/// Forgejo treats a PR whose title starts with one of these prefixes as a
/// draft.
const DRAFT: DraftMarker = DraftMarker {
    prefixes: &["WIP:", "[WIP]"],
    marker: "WIP:",
};

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

#[derive(Clone, Copy)]
pub(crate) struct Forgejo;

impl Backend for Forgejo {
    /// Forgejo and Gitea report their version under `/api/v1`.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let version = host.send(account, &Request::get("version".into())).await?;
        Ok(version["version"].is_string())
    }

    fn name(&self) -> &'static str {
        "Forgejo or Gitea"
    }

    /// Shoal does not sign in to the web UI, where Forgejo resolves threads,
    /// and before release 17 reruns jobs, which [`Self::server_support`]
    /// asks the server about.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::ResolveThreads => Support::Unsupported(Some("reply in the thread instead")),
            Capability::RerunJobs => Support::Unsupported(Some(
                "that needs Forgejo 17 or later; rerun them from the web UI, push a fix, or start a new run with shoal ci run",
            )),
            // Forgejo approves runs only through a web UI route.
            Capability::ApproveRuns => Support::Unsupported(Some("approve them in the web UI")),
            Capability::RunJobs => Support::Unsupported(Some(RUNNER_RELEASE)),
            Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::IssueAssignees
            | Capability::PrAssignees
            | Capability::IssueMilestones
            | Capability::PrMilestones
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::Downloads
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::AutoMerge
            | Capability::UpdateByMerge
            | Capability::UpdateByRebase
            | Capability::ForkPrs
            | Capability::Forks
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges
            | Capability::Reactions => Support::Supported,
        }
    }

    /// Forgejo reruns runs and jobs through its API from release 17 and
    /// hands a runner a named job from release 15; Gitea does neither.
    async fn server_support(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        let supported = match capability {
            Capability::RerunJobs => version::reruns(repo, account).await?,
            Capability::RunJobs => version::runs_jobs(repo, account).await?,
            _ => false,
        };
        if supported {
            return Ok(Support::Supported);
        }
        Ok(self.support(capability))
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
    }

    fn branch_route(&self) -> &'static str {
        "/src/branch/"
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repos/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pulls"
    }

    fn number_field(&self) -> &'static str {
        "number"
    }

    fn commit_field(&self) -> &'static str {
        "sha"
    }

    fn pull_head_ref(&self, number: PullNumber) -> Option<String> {
        Some(format!("refs/pull/{number}/head"))
    }

    fn login_hint(&self) -> &'static str {
        "Forgejo API request failed; private repositories need a token under [machine.forges.\"<host>\"]"
    }

    /// Forgejo ignores the username when the password is a token.
    fn git_username(&self) -> &'static str {
        "token"
    }

    fn authorization(&self, token: &str, _email: Option<&str>) -> String {
        format!("token {token}")
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let url = format!(
            "{}://{}/api/v1/{}",
            repo.web_scheme, repo.host, request.endpoint
        );
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&url, authorization, request).await
    }

    /// Forgejo's Actions lists apply `limit` only with `page`, and without
    /// one list every run, which can exceed the response bound.
    fn raw_read(&self, endpoint: String) -> String {
        first_page(endpoint)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("limit", PAGE_SIZE)
    }

    /// Forgejo reports some empty lists, such as reactions, as `null`.
    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        if response.is_null() {
            return Some((&[], false));
        }
        full_page(response, size)
    }

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

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// Forgejo refuses a second fork, so an existing one is looked up first.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        match owned_fork(repo, account).await? {
            Some(fork) => Ok(fork),
            None => github_rest::create_fork(repo, account).await,
        }
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

    async fn open_issue_blockers(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Vec<String>> {
        github_rest::open_blockers(repo, account, &format!("issues/{number}/dependencies")).await
    }

    /// Public repositories are readable without a login, which has none.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        if account.authorization(repo).await?.is_none() {
            return Ok(None);
        }
        github_rest::own_login(repo, account)
            .await
            .map(Some)
            .hint_login(self)
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
    fn head_branch(&self, number: PullNumber, pull: &Value) -> Result<String> {
        let head = github_rest::head_ref(pull)?;
        if head == format!("refs/pull/{number}/head") {
            return text(&pull["head"]["label"]).context("PR has no head branch");
        }
        Ok(head)
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

/// `owner/repository` of the account's existing fork of the repository.
async fn owned_fork(repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
    let login = github_rest::own_login(repo, account).await?;
    let forks = repo.pages(account, &repo.repo_endpoint("forks")).await?;
    forks
        .iter()
        .find(|fork| {
            fork["owner"]["login"]
                .as_str()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(&login))
        })
        .map(github_rest::full_name)
        .transpose()
}

/// `endpoint` with `page=1` added when its query sets `limit` without a
/// page; the first page is the default where the forge applies `limit`.
fn first_page(endpoint: String) -> String {
    let (request, fragment) = endpoint.split_at(endpoint.find('#').unwrap_or(endpoint.len()));
    let Some((_, query)) = request.split_once('?') else {
        return endpoint;
    };
    let keys: Vec<_> = query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .collect();
    if !keys.contains(&"limit") || keys.contains(&"page") {
        return endpoint;
    }
    format!("{request}&page=1{fragment}")
}

#[cfg(test)]
mod tests;
