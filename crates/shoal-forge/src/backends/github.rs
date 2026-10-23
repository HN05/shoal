//! GitHub, through `gh`, which owns the login: its CLI reports items and
//! activity as structured data, and `gh api` sends REST and GraphQL requests.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

mod actions;
mod activity;
pub(crate) mod threads;
mod view;

use super::{
    Backend,
    github_rest::{self, Dialect},
    image,
};
use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    capability::{Capability, Support},
    ci::{CiAction, CiCheck, Job, Location, Operation},
    conventions::{Lookup, shared_lookups},
    items::{
        action::{Edit, MergeMethod},
        attach::{Attachment, Holder, Media},
        create::{Head, NewIssue, NewPull},
        issue::require_open,
        item::{Current, Item, ItemState, PrSummary},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    review::{
        status::PrStatus,
        thread::{Target, Thread},
        view::{Details, Review},
    },
    transport::{
        api::{HttpMethod, Request, Upload, encode, full_page},
        programs,
        response::Response,
    },
    updates::{DISCUSSION, Snapshot},
};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

/// The variables gh takes a token from instead of its saved logins.
const ENV_TOKENS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

impl Backend for GitHub {
    /// GitHub's metadata names its SSH keys' password policy, and an
    /// Enterprise Server its version. Only gh's login for the host is sent,
    /// never a token it would read from the environment for any host.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let request = Request::get("meta".into()).withholding(ENV_TOKENS);
        let meta = host.send(account, &request).await?;
        Ok(["verifiable_password_authentication", "installed_version"]
            .iter()
            .any(|field| meta.get(field).is_some()))
    }

    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::ReadJobLogs => Support::Unsupported(None),
            Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::ForkPrs
            | Capability::Forks => Support::Supported,
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

    fn login_hint(&self) -> &'static str {
        "check `gh auth login` and repository access"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        repo.passthrough(programs::GH, account, request).await
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
        github_rest::current(repo, kind, number, value, Dialect::GitHub)
    }

    fn numbers_items_together(&self) -> bool {
        true
    }

    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>> {
        let noun = match kind {
            ItemKind::Issue => "issue",
            ItemKind::Pr => "pr",
        };
        let args = list_args(repo, kind, search);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let text = gh(account, &args, Query::List).await?;
        let listed: Vec<ListedItem> = serde_json::from_str(&text)
            .with_context(|| format!("invalid gh {noun} list response"))?;
        listed
            .into_iter()
            .map(|item| item.into_item(repo, kind))
            .collect()
    }

    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<ItemState> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let args = [
            "issue",
            "view",
            &id,
            "--repo",
            &repo,
            "--json",
            "number,state",
        ];
        let output = gh(account, &args, Query::Issue).await?;
        reported_state(&output, number)
    }

    async fn open_issue_dependencies(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<String>> {
        let endpoint = format!("issues/{number}/dependencies/blocked_by");
        github_rest::open_dependencies(repo, account, &endpoint).await
    }

    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let args = [
            "issue",
            "view",
            &id,
            "--repo",
            &repo,
            "--json",
            "number,title,body,state",
        ];
        let text = gh(account, &args, Query::Issue).await?;
        require_open(reported_state(&text, number)?)?;
        #[derive(serde::Deserialize)]
        struct GitHubIssue {
            number: u64,
            title: String,
            body: Option<String>,
        }
        let issue: GitHubIssue =
            serde_json::from_str(&text).context("invalid gh issue response")?;
        ensure!(issue.number == number, "gh returned a different issue");
        Ok((issue.title, issue.body.unwrap_or_default()))
    }

    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        let id = number.to_string();
        let repo = format!("{}/{}", repo.host, repo.path);
        let fields = "number,title,headRefName,baseRefName,isCrossRepository";
        let args = ["pr", "view", &id, "--repo", &repo, "--json", fields];
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Pull {
            number: u64,
            title: String,
            head_ref_name: String,
            base_ref_name: String,
            is_cross_repository: bool,
        }
        let output = gh(account, &args, Query::Pull("")).await?;
        let pull: Pull = serde_json::from_str(&output).context("invalid gh PR response")?;
        ensure!(pull.number == number, "gh returned a different PR");
        ensure!(
            !pull.is_cross_repository,
            "PR #{number} comes from a fork; only branches in this repository can be opened"
        );
        Ok((pull.title, pull.head_ref_name, pull.base_ref_name))
    }

    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        let number = number.to_string();
        let output = gh(
            account,
            &[
                "pr",
                "view",
                &number,
                "--repo",
                &format!("{}/{}", repo.host, repo.path),
                "--json",
                "number,state,headRefName,commits",
            ],
            Query::Pull(MERGED_HINT),
        )
        .await?;
        #[derive(serde::Deserialize)]
        struct Commit {
            oid: String,
        }
        #[derive(serde::Deserialize)]
        struct Pull {
            number: u64,
            state: String,
            #[serde(rename = "headRefName")]
            branch: String,
            commits: Vec<Commit>,
        }
        let pull: Pull = serde_json::from_str(&output)?;
        ensure!(
            pull.number.to_string() == number && pull.branch == branch,
            "PR does not match the workspace branch"
        );
        ensure!(
            matches!(pull.state.as_str(), "OPEN" | "CLOSED" | "MERGED"),
            "unknown PR state"
        );
        Ok((pull.state == "MERGED").then(|| pull.commits.into_iter().map(|c| c.oid).collect()))
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let mut requests = github_rest::edit(repo, current, edit, Dialect::GitHub);
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
        if delete_branch && current.head_repository.as_deref() == Some(repo.path.as_str()) {
            requests.push(Request::new(
                HttpMethod::Delete,
                repo.repo_endpoint(&format!("git/refs/heads/{}", encode(&pr.head, true))),
                None,
            ));
        }
        Ok(requests)
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

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        github_rest::pushable(repo, account).await
    }

    /// GitHub answers with the account's existing fork when it has one.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        github_rest::create_fork(repo, account).await
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
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>> {
        Ok(threads::thread(
            &threads::threads(repo, account, number).await?,
            comment,
        ))
    }

    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
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
        _number: u64,
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

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot> {
        activity::issue_activity(repo, account, number)
            .await
            .context(self.login_hint())
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        activity::pr_activity(repo, account, number, branch).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        activity::pr_status(repo, account, number, url).await
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

    /// `gh` reads the comments with the item; this reads the reviews.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        view::reviews(repo, account, kind, number).await
    }

    /// Only each check's latest attempt, not those reruns replaced.
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let endpoint = repo.repo_endpoint(&format!(
            "commits/{head}/check-runs?filter=latest&per_page=100"
        ));
        actions::checks(&repo.send(account, &Request::get(endpoint)).await?)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        actions::locate(repo, target)
    }

    fn ci_operation(
        &self,
        jobs: &[Job],
        selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        actions::operation(jobs, selected, job, action)
    }

    async fn check_uploads(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _attachments: &[Attachment],
    ) -> Result<bool> {
        Ok(false)
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

    /// GitHub's REST API has no upload endpoint, so this uses the one gh
    /// itself uses.
    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request> {
        ensure!(
            holder == Holder::Repository,
            "GitHub cannot attach files to {holder:?}"
        );
        let repository = repository.context("GitHub uploads need the repository's ID")?;
        let content_type = attachment.content_type()?;
        let name = encode(attachment.name(), false);
        let content = encode(content_type, false);
        Ok(Request::upload(
            format!(
                "https://uploads.{}/user-attachments/assets?name={name}&content_type={content}&repository_id={repository}",
                repo.host
            ),
            Upload {
                path: attachment.path.clone(),
                content_type,
                field: None,
            },
        ))
    }

    /// GitHub shows a video's bare URL as a player.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::Video => url.to_owned(),
            Media::Image => image(alt, url),
        }
    }

    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        shared_lookups(kind)
    }
}

/// The `gh issue list` or `gh pr list` arguments for `search`. Values go
/// in `--flag=value` form, so one starting with `-` stays a value.
fn list_args(repo: &ForgeRepo, kind: ItemKind, search: &list::Search) -> Vec<String> {
    let (noun, fields) = match kind {
        ItemKind::Issue => ("issue", "number,title,state,labels"),
        ItemKind::Pr => (
            "pr",
            "number,title,state,labels,headRefName,baseRefName,isDraft",
        ),
    };
    let mut args: Vec<String> = [noun, "list"].map(String::from).into();
    args.extend([
        format!("--repo={}/{}", repo.host, repo.path),
        // gh counts merged PRs as closed.
        format!("--state={}", search.state),
        format!("--limit={}", search.limit),
        format!("--json={fields}"),
    ]);
    args.extend(search.labels.iter().map(|label| format!("--label={label}")));
    args.extend(
        search
            .author
            .iter()
            .map(|author| format!("--author={author}")),
    );
    args.extend(search.text.iter().map(|text| format!("--search={text}")));
    args
}

/// An issue or PR as `gh issue list` or `gh pr list` reports it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedItem {
    number: u64,
    title: String,
    state: String,
    labels: Vec<ListedLabel>,
    head_ref_name: Option<String>,
    base_ref_name: Option<String>,
    is_draft: Option<bool>,
}

#[derive(serde::Deserialize)]
struct ListedLabel {
    name: String,
}

impl ListedItem {
    fn into_item(self, repo: &ForgeRepo, kind: ItemKind) -> Result<Item> {
        let state = match self.state.as_str() {
            "OPEN" => ItemState::Open,
            "CLOSED" => ItemState::Closed,
            "MERGED" if kind == ItemKind::Pr => ItemState::Merged,
            state => bail!("unrecognized gh state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: self
                    .head_ref_name
                    .context("gh listed a PR without its head")?,
                base: self
                    .base_ref_name
                    .context("gh listed a PR without its base")?,
                draft: self.is_draft.unwrap_or_default(),
            }),
        };
        Ok(Item {
            kind,
            number: self.number,
            url: repo.item_url(kind, self.number)?,
            title: self.title,
            state,
            labels: self.labels.into_iter().map(|label| label.name).collect(),
            pr,
        })
    }
}

/// What a `gh` read is for, which decides its timeout and error guidance.
#[derive(Clone, Copy)]
pub(crate) enum Query {
    Issue,
    Pull(&'static str),
    List,
    /// Reading an issue or PR for `shoal view`.
    View,
}

/// Run `gh` as `account`, the CLI that reads GitHub's items.
pub(crate) async fn gh(account: &Account, args: &[&str], query: Query) -> Result<String> {
    let tool = programs::GH;
    let mut command = tokio::process::Command::new(tool);
    command.args(args).env("NO_COLOR", "1");
    account.apply(&mut command);
    let seconds = match query {
        Query::Issue | Query::List | Query::View => 30,
        Query::Pull(_) => 20,
    };
    let output = shoal_subprocess::Run::new(command)
        .timeout(std::time::Duration::from_secs(seconds))
        .capture()
        .await;
    query.response(tool, output)
}

impl Query {
    fn response(self, tool: &str, output: std::io::Result<std::process::Output>) -> Result<String> {
        match self {
            Self::Issue => {
                shoal_subprocess::checked_output(tool, output).with_context(|| {
                    format!("{tool} issue lookup failed; install it and check `{tool} auth login` and repository access before using --issue")
                })
            }
            Self::Pull(hint) => shoal_subprocess::checked_output(tool, output)
                .with_context(|| format!("PR lookup requires {tool} and its existing login{hint}")),
            Self::List => shoal_subprocess::checked_output(tool, output).with_context(|| {
                format!("{tool} search failed; install it and check `{tool} auth login` and repository access")
            }),
            Self::View => shoal_subprocess::checked_output(tool, output).with_context(|| {
                format!("{tool} lookup failed; install it and check `{tool} auth login` and repository access")
            }),
        }
    }
}

/// The state `gh issue view --json number,state` reports.
fn reported_state(output: &str, number: u64) -> Result<ItemState> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
    }
    #[derive(serde::Deserialize)]
    struct Issue {
        number: u64,
        state: State,
    }
    let issue: Issue = serde_json::from_str(output).context("invalid gh issue response")?;
    ensure!(issue.number == number, "gh returned a different issue");
    Ok(match issue.state {
        State::Open => ItemState::Open,
        State::Closed => ItemState::Closed,
    })
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
mod tests {
    use super::*;

    #[test]
    fn lists_keep_filter_values_attached_to_their_flags() {
        let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        let search = list::Search {
            state: list::StateFilter::Closed,
            labels: vec!["-odd".into()],
            author: Some("dev".into()),
            text: Some("crash in:title".into()),
            limit: 5,
        };
        assert_eq!(
            list_args(&repo, ItemKind::Pr, &search),
            [
                "pr",
                "list",
                "--repo=github.com/team/repo",
                "--state=closed",
                "--limit=5",
                "--json=number,title,state,labels,headRefName,baseRefName,isDraft",
                "--label=-odd",
                "--author=dev",
                "--search=crash in:title",
            ]
        );
        let listed: ListedItem = serde_json::from_value(json!({"number": 4, "title": "Fix",
            "state": "MERGED", "labels": [{"name": "bug"}], "headRefName": "topic",
            "baseRefName": "main", "isDraft": false}))
        .unwrap();
        let item = listed.into_item(&repo, ItemKind::Pr).unwrap();
        assert_eq!(item.state, ItemState::Merged);
        assert_eq!(item.url, "https://github.com/team/repo/pull/4");
        assert_eq!(item.pr.unwrap().head, "topic");
    }

    #[test]
    fn query_errors_preserve_login_guidance_and_diagnostic_limits() {
        use std::{io, os::unix::process::ExitStatusExt, process::Output};

        for tool in ["gh", "fj"] {
            for (query, limit, hint) in [
                (
                    Query::Issue,
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "auth login",
                ),
                (
                    Query::Pull(MERGED_HINT),
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "shoal done",
                ),
                (
                    Query::Pull(""),
                    shoal_subprocess::MAX_DIAGNOSTIC_CHARS,
                    "existing login",
                ),
            ] {
                let missing = query
                    .response(tool, Err(io::Error::from(io::ErrorKind::NotFound)))
                    .unwrap_err();
                let missing = format!("{missing:#}");
                assert!(missing.contains(tool), "{missing}");
                assert!(missing.contains(hint), "{missing}");

                let failed = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(256),
                            stdout: b"ignored".to_vec(),
                            stderr: format!("{}END", "é".repeat(limit)).into_bytes(),
                        }),
                    )
                    .unwrap_err();
                let failed = format!("{failed:#}");
                assert!(failed.contains(tool), "{failed}");
                assert!(failed.contains(hint), "{failed}");
                assert!(failed.ends_with(&"é".repeat(limit)));
                assert!(!failed.contains("END"));

                let invalid = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(0),
                            stdout: vec![0xff],
                            stderr: Vec::new(),
                        }),
                    )
                    .unwrap_err();
                assert!(format!("{invalid:#}").contains("output is not UTF-8"));
            }
        }
    }

    #[test]
    fn github_requires_matching_number_and_known_state() {
        for (state, expected) in [("OPEN", ItemState::Open), ("CLOSED", ItemState::Closed)] {
            let output = serde_json::json!({"number": 316, "state": state}).to_string();
            assert_eq!(reported_state(&output, 316).unwrap(), expected);
            assert!(reported_state(&output, 1).is_err());
        }
        for output in [
            r#"{"number":316,"state":"MERGED"}"#,
            r#"{"number":316}"#,
            "",
        ] {
            assert!(reported_state(output, 316).is_err());
        }
    }

    #[test]
    fn graphql_errors_fail_a_successful_response() {
        let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
        assert_eq!(
            graphql_errors(&failed).unwrap(),
            "not allowed; GraphQL error"
        );
        assert_eq!(
            graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
            None
        );
        assert_eq!(graphql_errors(&serde_json::json!([])), None);
    }
}
