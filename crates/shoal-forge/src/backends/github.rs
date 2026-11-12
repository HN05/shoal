//! GitHub: its REST and GraphQL APIs, on GitHub.com or an Enterprise
//! Server, signed in with the account's token for the host.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

mod actions;
mod activity;
mod attachments;
mod checks;
mod conventions;
mod reactions;
mod review;
mod reviewers;
mod runners;
mod runs;
pub(crate) mod threads;
mod view;

use super::{
    Backend, LoginHint,
    github_rest::{self, GitHubShapes},
    rest,
};
use crate::{
    ForgeRepo,
    account::Account,
    capability::{Capability, Support},
    conventions::draft::DraftMarker,
    items::{
        action::{Edit, MergeMethod, UpdateMethod},
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, text},
        link::ItemKind,
        list,
        reaction::Subject,
        revert::NewRevert,
    },
    numbers::{CommentId, IssueNumber, PullNumber},
    repo::locator::ItemRoute,
    review::{
        diff::Diff,
        reviewers::Reviewer,
        status::PrStatus,
        submit::{PlacedReview, Submission},
        thread::{Target, Thread},
        view::{Details, Review},
    },
    transport::{
        api::{HttpMethod, Request, encode, full_page},
        response::Response,
    },
    updates::{DISCUSSION, Snapshot},
};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

/// GitHub.com's API host; an Enterprise Server serves its API under
/// `/api/v3` of its own host.
const API: &str = "https://api.github.com";

impl Backend for GitHub {
    /// GitHub's metadata names its SSH keys' password policy, and an
    /// Enterprise Server its version.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let meta = host.send(account, &Request::get("meta".into())).await?;
        Ok(["verifiable_password_authentication", "installed_version"]
            .iter()
            .any(|field| meta.get(field).is_some()))
    }

    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RunJobs
            | Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ApproveRuns
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
            | Capability::Reactions
            | Capability::RevertPrs => Support::Supported,
        }
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
    }

    fn branch_route(&self) -> &'static str {
        "/tree/"
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
        "GitHub API request failed; private repositories need a token under [machine.forges.\"<host>\"]"
    }

    fn git_username(&self) -> &'static str {
        "x-access-token"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let authorization = account.authorization(repo).await?;
        repo.curl_send(&api_url(repo, &request.endpoint), authorization, request)
            .await
    }

    /// GraphQL reports a failed mutation in a successful response.
    fn reported_failure(&self, body: &Value) -> Option<String> {
        graphql_errors(body)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
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

    /// GitHub counts both sides of a comparison; one commit to a page keeps
    /// its list of the head's commits short.
    async fn behind(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<u64>> {
        let compare = |base: &str, head: &str| format!("compare/{base}...{head}?per_page=1");
        github_rest::behind(repo, account, number, compare, "behind_by").await
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let item = &current.item;
        let pr = item.pr.as_ref().context("only PRs merge")?;
        let merge = format!("{}/merge", repo.item_endpoint(ItemKind::Pr, item.number));
        // GitHub can answer success without merging.
        let mut requests = vec![
            Request::new(
                HttpMethod::Put,
                merge,
                Some(json!({ "merge_method": method.as_str() })),
            )
            .confirming("merged"),
        ];
        // A fork's branch is not this repository's to delete.
        if delete_branch && current.head_in(repo) {
            requests.push(self.branch_delete(repo, &pr.head));
        }
        Ok(requests)
    }

    /// GitHub refuses to merge later a PR it would merge now, so that one
    /// merges now.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        ensure!(
            !delete_branch,
            "GitHub deletes the branch of an automatic merge only when the repository's settings say so"
        );
        if current.merges_now {
            return self.merge(repo, current, method, false);
        }
        Ok(vec![auto_merge_request(current, Some(method))?])
    }

    fn cancel_auto_merge(&self, _repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>> {
        Ok(vec![auto_merge_request(current, None)?])
    }

    /// Only GitHub's GraphQL API rebases a PR's branch, and it merges one
    /// too. Naming the head the PR had when read leaves a branch that moved
    /// since unchanged.
    fn update_branch(
        &self,
        _repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        let id = current
            .node_id
            .as_deref()
            .context("GitHub did not report the PR's node ID")?;
        let head = current
            .head_sha
            .as_deref()
            .context("GitHub did not report the PR's head commit")?;
        let method = match method {
            UpdateMethod::Merge => "MERGE",
            UpdateMethod::Rebase => "REBASE",
        };
        Ok(vec![Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({
                "query": "mutation($id: ID!, $head: GitObjectID!, $method: PullRequestBranchUpdateMethod!) { updatePullRequestBranch(input: {pullRequestId: $id, expectedHeadOid: $head, updateMethod: $method}) { clientMutationId } }",
                "variables": { "id": id, "head": head, "method": method },
            })),
        )])
    }

    async fn revert(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &Current,
        revert: &NewRevert,
    ) -> Result<(PullNumber, String)> {
        let response = repo
            .send(account, &revert_request(current, revert)?)
            .await?;
        reverted(&response)
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

    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request {
        let endpoint = repo.repo_endpoint(&format!("git/refs/heads/{}", encode(branch, true)));
        Request::new(HttpMethod::Delete, endpoint, None)
    }

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// GitHub answers with the account's existing fork when it has one.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        github_rest::create_fork(repo, account).await
    }

    /// The users GitHub can assign the repository's items to, and the
    /// teams of an organization's repository.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
        let (users, teams) = tokio::try_join!(
            reviewers::users(repo, account),
            reviewers::teams(repo, account),
        )?;
        Ok(users.into_iter().chain(teams).collect())
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

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>> {
        Ok(threads::thread(
            &threads::threads(repo, account, number).await?,
            comment,
        ))
    }

    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request> {
        let endpoint =
            repo.repo_endpoint(&format!("pulls/{number}/comments/{}/replies", thread.first));
        Ok(Request::new(
            HttpMethod::Post,
            endpoint,
            Some(json!({ "body": body })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let Target::GitHub { node } = &thread.target else {
            bail!("not a GitHub review thread");
        };
        let mutation = if resolved {
            "resolveReviewThread"
        } else {
            "unresolveReviewThread"
        };
        Ok(Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({
                "query": format!(
                    "mutation($id: ID!) {{ {mutation}(input: {{threadId: $id}}) {{ thread {{ isResolved }} }} }}"
                ),
                "variables": { "id": node },
            })),
        ))
    }

    async fn pr_diff(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &Current,
    ) -> Result<Diff> {
        review::diff(repo, account, current).await
    }

    fn review(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        review: &PlacedReview,
    ) -> Result<Submission> {
        Ok(review::review(repo, number, review))
    }

    async fn review_keys(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        let response = responses.first().context("the review was not posted")?;
        review::keys(repo, account, number, review, response).await
    }

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot> {
        activity::issue_activity(repo, account, number)
            .await
            .hint_login(self)
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot> {
        activity::pr_activity(repo, account, number).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        url: &str,
    ) -> Result<PrStatus> {
        activity::pr_status(repo, account, number, url)
            .await
            .hint_login(self)
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details> {
        view::details(repo, account, kind, number, comments).await
    }

    /// The comments are read with the item; this reads the reviews.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _details: &mut Details,
        _errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view::reviews(repo, account, kind, number).await
    }

    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        github_rest::own_login(repo, account).await.map(Some)
    }

    /// GitHub uploads name the repository by its numeric ID.
    async fn repository_id(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<u64>> {
        let repository = repo
            .send(account, &Request::get(format!("repos/{}", repo.path)))
            .await?;
        repository["id"]
            .as_u64()
            .context("GitHub did not report the repository's ID")
            .map(Some)
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

/// Where `endpoint` is under GitHub's API, which takes GraphQL at
/// `graphql` on GitHub.com and `/api/graphql` on an Enterprise Server. An
/// endpoint that is already a URL, such as one on GitHub's upload host,
/// stays as it is.
fn api_url(repo: &ForgeRepo, endpoint: &str) -> String {
    if endpoint.starts_with("https://") {
        return endpoint.to_owned();
    }
    if repo.host == "github.com" {
        return format!("{API}/{endpoint}");
    }
    let root = format!("{}://{}/api", repo.web_scheme, repo.host);
    match endpoint {
        "graphql" => format!("{root}/graphql"),
        _ => format!("{root}/v3/{endpoint}"),
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

/// The GraphQL request that has GitHub merge the PR `current` with `method`
/// once it can, or withdraws that without one.
fn auto_merge_request(current: &Current, method: Option<MergeMethod>) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    let (query, variables) = match method {
        Some(method) => (
            "mutation($id: ID!, $method: PullRequestMergeMethod!) { enablePullRequestAutoMerge(input: {pullRequestId: $id, mergeMethod: $method}) { clientMutationId } }",
            json!({ "id": id, "method": method.as_str().to_ascii_uppercase() }),
        ),
        None => (
            "mutation($id: ID!) { disablePullRequestAutoMerge(input: {pullRequestId: $id}) { clientMutationId } }",
            json!({ "id": id }),
        ),
    };
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({ "query": query, "variables": variables })),
    ))
}

/// GitHub reverts a PR only through GraphQL, opening the revert from a
/// branch it creates.
fn revert_request(current: &Current, revert: &NewRevert) -> Result<Request> {
    let id = current
        .node_id
        .as_deref()
        .context("GitHub did not report the PR's node ID")?;
    Ok(Request::new(
        HttpMethod::Post,
        "graphql".into(),
        Some(json!({
            "query": REVERT_PULL,
            "variables": {
                "id": id,
                "title": revert.title,
                "body": revert.body,
                "draft": revert.draft,
            },
        })),
    ))
}

const REVERT_PULL: &str = "mutation($id: ID!, $title: String!, $body: String!, $draft: Boolean!) { revertPullRequest(input: {pullRequestId: $id, title: $title, body: $body, draft: $draft}) { revertPullRequest { number headRefName } } }";

/// The number and head branch of the PR a revert's response reports.
fn reverted(response: &Value) -> Result<(PullNumber, String)> {
    let pull = &response["data"]["revertPullRequest"]["revertPullRequest"];
    let number = pull["number"]
        .as_u64()
        .map(PullNumber::new)
        .context("GitHub did not report the revert's number")?;
    let head = text(&pull["headRefName"]).context("GitHub did not report the revert's branch")?;
    Ok((number, head))
}

/// The messages of a GraphQL response's `errors`, when it has any.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())?;
    Some(
        errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

#[cfg(test)]
mod tests;
