//! GitHub, through `gh`, which owns the login: its CLI reports items and
//! activity as structured data, and `gh api` sends REST and GraphQL requests.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

mod actions;
mod activity;
mod review;
mod reviewers;
pub(crate) mod threads;
mod view;

use super::{
    Backend, LoginHint,
    github_rest::{self, Dialect},
    image, link,
};
use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    capability::{Capability, Support},
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        checks::plain,
        id,
        run::{NewRun, workflow_segment},
        run_action::Run,
        run_view::{JOBS_PAGE, RunFilter, RunJob},
    },
    conventions::{Lookup, shared_lookups},
    items::{
        action::{Edit, MergeMethod},
        attach::{Attachment, Holder, Media},
        create::{Head, NewIssue, NewPull},
        issue::require_open,
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
        list,
        reaction::{Listed, Subject},
    },
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
            Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::AutoMerge
            | Capability::ForkPrs
            | Capability::Forks
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges
            | Capability::Reactions => Support::Supported,
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

    /// GitHub counts both sides of a comparison; one commit to a page keeps
    /// its list of the head's commits short.
    async fn behind(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
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
        if delete_branch && current.head_repository.as_deref() == Some(repo.path.as_str()) {
            requests.push(Request::new(
                HttpMethod::Delete,
                repo.repo_endpoint(&format!("git/refs/heads/{}", encode(&pr.head, true))),
                None,
            ));
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

    async fn pr_diff(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        current: &Current,
    ) -> Result<Diff> {
        review::diff(repo, account, current).await
    }

    fn review(&self, repo: &ForgeRepo, number: u64, review: &PlacedReview) -> Result<Submission> {
        Ok(review::review(repo, number, review))
    }

    async fn review_keys(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
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
        number: u64,
    ) -> Result<Snapshot> {
        activity::issue_activity(repo, account, number)
            .await
            .hint_login(self)
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
        _errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view::reviews(repo, account, kind, number).await
    }

    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        _kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String> {
        Ok(github_rest::reactions_endpoint(
            repo,
            number,
            subject,
            Dialect::GitHub,
        ))
    }

    fn listed_reaction(&self, reaction: &Value) -> Result<Listed> {
        github_rest::listed_reaction(reaction)
    }

    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        github_rest::own_login(repo, account).await.map(Some)
    }

    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request> {
        Ok(github_rest::add_reaction(endpoint, content))
    }

    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request> {
        let id = own.number()?;
        Ok(Request::new(
            HttpMethod::Delete,
            format!("{endpoint}/{id}"),
            None,
        ))
    }

    /// Only each check's latest attempt, not those reruns replaced, and
    /// each commit status, which reports no Actions job.
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let runs = Request::get(repo.repo_endpoint(&format!(
            "commits/{head}/check-runs?filter=latest&per_page=100"
        )));
        let statuses =
            Request::get(repo.repo_endpoint(&format!("commits/{head}/status?per_page=100")));
        let (runs, statuses) =
            tokio::try_join!(repo.send(account, &runs), repo.send(account, &statuses))?;
        let mut checks = actions::checks(&runs)?;
        checks.extend(github_rest::commit_statuses(&statuses, Dialect::GitHub)?);
        Ok(checks)
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

    /// GitHub accepts the dispatch without reporting the run it starts.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let workflow = run.workflow_file(self)?;
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("actions/workflows/{workflow}/dispatches")),
            Some(json!({"ref": run.branch, "inputs": run.inputs})),
        ))
    }

    /// A run's URL holds its API ID.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<u64> {
        id(repo.repository_path(target, "/actions/runs/")?.first()?)
    }

    fn run_path(&self, number: u64) -> String {
        format!("actions/runs/{number}")
    }

    /// A run names its workflow by the file's path in the repository.
    fn run_item(&self, _repo: &ForgeRepo, value: &Value) -> Result<Run> {
        let path = text(&value["path"]);
        Ok(Run {
            id: value["id"].as_u64().context("Actions run has no ID")?,
            url: text(&value["html_url"]).context("Actions run has no URL")?,
            branch: text(&value["head_branch"]),
            workflow: path.map(|path| path.rsplit('/').next().unwrap_or_default().into()),
            state: actions::state(value)?,
        })
    }

    fn runs_path(&self, filter: &RunFilter) -> Result<String> {
        let mut path = match &filter.workflow {
            Some(workflow) => format!("actions/workflows/{}/runs", workflow_segment(workflow)?),
            None => "actions/runs".into(),
        };
        path.push_str(&format!("?per_page={}", filter.limit));
        if let Some(branch) = &filter.branch {
            path.push_str(&format!("&branch={}", encode(branch, false)));
        }
        Ok(path)
    }

    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String> {
        Some(format!(
            "actions/runs/{}/jobs?per_page={JOBS_PAGE}&page={page}",
            run.id
        ))
    }

    fn run_jobs(&self, _run: &Run, _offset: usize, value: &Value) -> Result<Vec<RunJob>> {
        let jobs = value["jobs"]
            .as_array()
            .context("Actions jobs are not a list")?;
        jobs.iter()
            .map(|job| {
                Ok(RunJob {
                    id: job["id"].as_u64().context("Actions job has no ID")?,
                    name: text(&job["name"]).context("Actions job has no name")?,
                    state: actions::state(job)?,
                    url: text(&job["html_url"]),
                })
            })
            .collect()
    }

    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        run: &Run,
        job: &RunJob,
    ) -> Result<String> {
        let location = Location::GitHub {
            run: run.id,
            job: job.id,
        };
        repo.job_log(account, location).await
    }

    fn run_endpoint(&self, run: &Run, action: CiAction, failed: bool) -> Result<String> {
        let id = run.id;
        Ok(match (action, failed) {
            (CiAction::Rerun, false) => format!("actions/runs/{id}/rerun"),
            (CiAction::Rerun, true) => format!("actions/runs/{id}/rerun-failed-jobs"),
            (CiAction::Cancel, _) => format!("actions/runs/{id}/cancel"),
        })
    }

    fn job_log_endpoint(&self, location: Location) -> Result<String> {
        actions::log_endpoint(location)
    }

    /// Each log line starts with its time.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(&plain::without_timestamps(log))
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
        let content_type = attachment.content_type();
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
            Media::File => link(alt, url),
        }
    }

    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        shared_lookups(kind)
    }

    /// GitHub keeps its own spelling of a label added in another case.
    fn label_names_fold_case(&self) -> bool {
        true
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
