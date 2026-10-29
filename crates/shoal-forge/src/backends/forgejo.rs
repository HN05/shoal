//! Forgejo, and Gitea, which serves the same API: GitHub's REST shapes
//! under `/api/v1`, sent with the token fj saved for the host, else through
//! tea's login, else anonymously.
use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, TryStreamExt};
use serde_json::{Value, json};

mod actions;
mod organization;
mod review;
mod reviewers;
mod sniff;
pub(crate) mod tea;
pub(crate) mod threads;
mod uploads;
mod view;

use super::{
    Backend, LoginHint,
    github_rest::{self, Dialect},
    image, link, rest, served_at,
};
use crate::{
    ForgeRepo,
    account::{Account, ForgejoLogin},
    backends::rest::{RestCheck, RestReads, fingerprints},
    capability::{self, Capability, Support},
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        checks::{CheckLog, plain},
        run::NewRun,
        run_action::Run,
        run_view::{RunFilter, RunJob},
    },
    conventions::{Lookup, shared_lookups},
    items::{
        action::{Edit, MergeMethod},
        attach::{Attachment, Holder, Media},
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, text},
        link::ItemKind,
        list,
        reaction::{Listed, Subject},
    },
    repo::locator::ItemRoute,
    review::{
        diff::Diff,
        reviewers::Reviewer,
        status::{PrStatus, Verdict, forgejo_reviews},
        submit::{PlacedReview, Submission},
        thread::{Target, Thread},
        view::{Details, Review, login},
    },
    transport::{
        api::{HttpMethod, MAX_RESPONSE_BYTES, Request, Upload, authorization, encode, full_page},
        response::Response,
    },
    updates::{DISCUSSION, Entry, Snapshot},
};

/// Forgejo treats a PR whose title starts with one of these as a draft.
pub(crate) const DRAFT_PREFIXES: [&str; 2] = ["WIP:", "[WIP]"];

/// Forgejo's default maximum page size.
const PAGE_SIZE: usize = 50;

/// How many listed PRs are read at once.
const PULL_READS: usize = 8;

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

    /// Shoal does not sign in to the web UI, where Forgejo resolves threads
    /// and reruns jobs.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::ResolveThreads => Support::Unsupported(Some("reply in the thread instead")),
            Capability::RerunJobs => Support::Unsupported(Some(
                "rerun them from the web UI, push a fix, or start a new run with shoal forge ci run",
            )),
            Capability::CancelJobs
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
        ItemRoute::Pulls
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
        "Forgejo API request failed; private repositories need an fj or tea login"
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
        match account.forgejo_login(&repo.host).await? {
            ForgejoLogin::Tea(login) => {
                tea::exchange(account, &login, request, MAX_RESPONSE_BYTES).await
            }
            ForgejoLogin::Token(token) => {
                repo.curl_send(&url, Some(authorization(token)), request)
                    .await
            }
            ForgejoLogin::Anonymous => repo.curl_send(&url, None, request).await,
        }
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
        github_rest::current(repo, kind, number, value, Dialect::Forgejo)
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
        read_pulls(repo, account, numbers).await.hint_login(self)
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
        Ok(github_rest::edit(repo, current, edit, Dialect::Forgejo))
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
        let number = current.item.number;
        Ok(vec![Request::new(
            HttpMethod::Post,
            format!("{}/merge", repo.item_endpoint(ItemKind::Pr, number)),
            Some(json!({
                "Do": method.as_str(),
                "delete_branch_after_merge": delete_branch,
            })),
        )])
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

    /// The users Forgejo offers as reviewers, and the teams of an
    /// organization's repository.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
        let (users, teams) = tokio::try_join!(
            reviewers::users(repo, account),
            reviewers::teams(repo, account),
        )?;
        Ok(users.into_iter().chain(teams).collect())
    }

    /// Forgejo keeps draft state in the title.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let title = if pull.draft {
            draft_title(&pull.title, true)
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

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>> {
        let reviews = threads::all_review_comments(repo, account, number).await?;
        Ok(threads::thread(&reviews, comment))
    }

    /// A reply is a comment of the thread's review on the first comment's
    /// lines.
    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        let Target::Forgejo {
            review,
            path,
            line,
            extra_lines,
        } = &thread.target
        else {
            bail!("not a Forgejo review thread");
        };
        let side = if *line < 0 {
            "old_position"
        } else {
            "new_position"
        };
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("pulls/{number}/reviews/{review}/comments")),
            Some(json!({ "body": body, "path": path, side: line.abs(),
                "extra_lines_count": extra_lines })),
        ))
    }

    fn resolution(
        &self,
        _repo: &ForgeRepo,
        _number: u64,
        _thread: &Thread,
        _resolved: bool,
    ) -> Result<Request> {
        Err(capability::unsupported(self, Capability::ResolveThreads))
    }

    async fn open_issue_dependencies(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<String>> {
        github_rest::open_dependencies(repo, account, &format!("issues/{number}/dependencies"))
            .await
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
        _repo: &ForgeRepo,
        _account: &Account,
        _number: u64,
        _review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        review::keys(responses.first().context("the review was not posted")?)
    }

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot> {
        rest::issue_activity(self, repo, account, number).await
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        rest::pr_activity(self, repo, account, number, branch).await
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        rest::pr_status(self, repo, account, number, url).await
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        _comments: bool,
    ) -> Result<Details> {
        view::details(repo, account, kind, number)
            .await
            .hint_login(self)
    }

    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view::discussion(repo, account, kind, number, details, errors)
            .await
            .hint_login(self)
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
            Dialect::Forgejo,
        ))
    }

    fn listed_reaction(&self, reaction: &Value) -> Result<Listed> {
        github_rest::listed_reaction(reaction)
    }

    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        github_rest::own_login(repo, account).await.hint_login(self)
    }

    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request> {
        Ok(github_rest::add_reaction(endpoint, content))
    }

    /// Forgejo removes the account's reaction by its content.
    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request> {
        Ok(Request::new(
            HttpMethod::Delete,
            endpoint,
            Some(json!({ "content": own.content })),
        ))
    }

    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let endpoint = repo.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        github_rest::commit_statuses(
            &repo.send(account, &Request::get(endpoint)).await?,
            Dialect::Forgejo,
        )
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        let (run_number, _) = actions::job_location(repo, target).ok()?;
        Some(Location::Forgejo { run_number })
    }

    fn ci_operation(
        &self,
        _jobs: &[Job],
        _selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        let Some(Location::Forgejo { run_number }) = job.location else {
            bail!("check {} is not a Forgejo Actions job", job.name);
        };
        if action != CiAction::Cancel {
            return Err(capability::unsupported(self, Capability::ci(action)));
        }
        Ok((
            Operation::CancelForgejoRun { run_number },
            vec![job.name.clone()],
        ))
    }

    /// Forgejo links a run by its number in the repository, not the API ID
    /// its cancellation takes.
    async fn ci_endpoint(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
        operation: Operation,
    ) -> Result<String> {
        match operation {
            Operation::Post(endpoint) => Ok(endpoint),
            Operation::CancelForgejoRun { run_number } => {
                let run = actions::run_id_for(repo, account, run_number, head).await?;
                Ok(format!("actions/runs/{run}/cancel"))
            }
        }
    }

    async fn failed_check_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        actions::failed_check_log(repo, account, url, name).await
    }

    /// Forgejo reports the run it starts when asked to; Gitea ignores the
    /// request and reports none.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let workflow = run.workflow_file(self)?;
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint(&format!("actions/workflows/{workflow}/dispatches")),
            Some(json!({"ref": run.branch, "inputs": run.inputs, "return_run_info": true})),
        ))
    }

    fn started_run_url(&self, repo: &ForgeRepo, response: &Value) -> Option<String> {
        let number = response["run_number"].as_u64()?;
        Some(format!("{}/actions/runs/{number}", repo.repository_url()))
    }

    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<u64> {
        actions::run_number(repo, target)
    }

    /// Forgejo links a run by its number in the repository, not its API ID.
    fn run_path(&self, number: u64) -> String {
        format!("actions/runs?run_number={number}")
    }

    fn run(&self, repo: &ForgeRepo, number: u64, value: &Value) -> Result<Run> {
        self.run_item(repo, actions::numbered_run(value, number)?)
    }

    /// A run names its workflow by file name.
    fn run_item(&self, repo: &ForgeRepo, value: &Value) -> Result<Run> {
        let number = value["index_in_repo"]
            .as_u64()
            .context("Actions run has no number")?;
        Ok(Run {
            id: value["id"].as_u64().context("Actions run has no ID")?,
            url: format!("{}/actions/runs/{number}", repo.repository_url()),
            branch: text(&value["prettyref"]),
            workflow: text(&value["workflow_id"]),
            state: actions::state(&value["status"])?,
        })
    }

    /// The list reads a page only when it names one.
    fn runs_path(&self, filter: &RunFilter) -> Result<String> {
        let mut path = format!("actions/runs?limit={}&page=1", filter.limit);
        if let Some(branch) = &filter.branch {
            let reference = format!("refs/heads/{branch}");
            path.push_str(&format!("&ref={}", encode(&reference, false)));
        }
        if let Some(workflow) = &filter.workflow {
            path.push_str(&format!("&workflow_id={}", encode(workflow, false)));
        }
        Ok(path)
    }

    /// Forgejo lists every job of a run at once.
    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String> {
        (page == 1).then(|| format!("actions/runs/{}/jobs", run.id))
    }

    /// The web UI links a job by its index in the run.
    fn run_jobs(&self, run: &Run, offset: usize, value: &Value) -> Result<Vec<RunJob>> {
        let jobs = value.as_array().context("Actions jobs are not a list")?;
        jobs.iter()
            .enumerate()
            .map(|(index, job)| {
                let index = offset + index;
                Ok(RunJob {
                    id: job["id"].as_u64().context("Actions job has no ID")?,
                    name: text(&job["name"]).context("Actions job has no name")?,
                    state: actions::state(&job["status"])?,
                    url: Some(format!("{}/jobs/{index}", run.url)),
                })
            })
            .collect()
    }

    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _run: &Run,
        job: &RunJob,
    ) -> Result<String> {
        let endpoint = repo.repo_endpoint(&format!("actions/jobs/{}/logs", job.id));
        Ok(self.plain_log(&actions::job_log(repo, account, &endpoint).await?))
    }

    /// Forgejo's API cannot rerun, which the capability refuses first.
    fn run_endpoint(&self, run: &Run, action: CiAction, _failed: bool) -> Result<String> {
        match action {
            CiAction::Cancel => Ok(format!("actions/runs/{}/cancel", run.id)),
            CiAction::Rerun => Err(capability::unsupported(self, Capability::ci(action))),
        }
    }

    /// Each log line starts with its time.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(&plain::without_timestamps(log))
    }

    fn failed_step(&self, log: &str) -> Option<String> {
        actions::failed_step(log)
    }

    /// tea cannot send a file, so uploads need fj's token. The instance's
    /// settings are checked here, since an upload they refuse would fail only
    /// after its comment or item is posted.
    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool> {
        let login = account.forgejo_login(&repo.host).await?;
        ensure!(
            !matches!(login, ForgejoLogin::Tea(_)),
            "tea cannot upload files; save an fj login for {} with fj auth login",
            repo.host
        );
        let settings = uploads::Settings::read(repo, account).await?;
        for attachment in attachments {
            settings.check(attachment)?;
        }
        Ok(true)
    }

    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        _repository: Option<u64>,
    ) -> Result<Request> {
        let name = encode(attachment.name(), false);
        let endpoint = match holder {
            Holder::Item(number) => format!("issues/{number}/assets?name={name}"),
            Holder::Comment(id) => format!("issues/comments/{id}/assets?name={name}"),
            Holder::Repository => bail!("Forgejo keeps attached files with items and comments"),
        };
        Ok(Request::upload(
            repo.repo_endpoint(&endpoint),
            Upload {
                path: attachment.path.clone(),
                content_type: attachment.content_type(),
                field: Some("attachment"),
            },
        ))
    }

    fn uploaded_url(&self, response: &Value) -> Result<String> {
        served_at(response, "browser_download_url")
    }

    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::Video => format!("<video src=\"{url}\" controls></video>"),
            Media::Image => image(alt, url),
            Media::File => link(alt, url),
        }
    }

    fn edit_comment(&self, repo: &ForgeRepo, id: u64, body: &str) -> Result<Request> {
        let endpoint = repo.repo_endpoint(&format!("issues/comments/{id}"));
        Ok(Request::new(
            HttpMethod::Patch,
            endpoint,
            Some(json!({ "body": body })),
        ))
    }

    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        shared_lookups(kind)
    }

    /// Forgejo lists an organization's labels, which its repositories'
    /// items take too, apart from the repository's own.
    async fn labels(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Value>> {
        let endpoint = repo.repo_endpoint("labels");
        let (mut labels, organization) = tokio::try_join!(
            repo.pages(account, &endpoint),
            organization::labels(repo, account),
        )?;
        labels.extend(organization);
        Ok(labels)
    }

    fn exclusive_label(&self, _name: &str, label: &Value) -> bool {
        label["exclusive"].as_bool().unwrap_or(false)
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

impl RestReads for Forgejo {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = repo.repo_endpoint(&format!("issues/{number}/comments"));
        let comments = repo.pages(account, &endpoint).await?;
        fingerprints(&comments, &["body", "updated_at"], |comment| {
            login(&comment["user"])
        })
    }

    async fn review_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        let reviews = repo.pages(account, &endpoint).await?;
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
            |review| login(&review["user"]),
        )
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull.head_sha.as_deref().context("PR has no head commit")?;
        let endpoint = repo.repo_endpoint(&format!("commits/{head}/status?limit=50"));
        let combined = repo.send(account, &Request::get(endpoint)).await?;
        statuses(&combined)
    }

    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let endpoint = repo.repo_endpoint(&format!("pulls/{number}/reviews"));
        forgejo_reviews(&repo.pages(account, &endpoint).await?)
    }
}

/// Forgejo's combined status; a commit without statuses has `"statuses": null`.
fn statuses(combined: &Value) -> Result<Vec<RestCheck>> {
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

/// The length of a Forgejo draft prefix at the start of `title`.
pub(crate) fn draft_prefix(title: &str) -> Option<usize> {
    DRAFT_PREFIXES.iter().find_map(|prefix| {
        title
            .get(..prefix.len())
            .filter(|start| start.eq_ignore_ascii_case(prefix))
            .map(|_| prefix.len())
    })
}

/// The title with Forgejo's draft prefix added or removed.
pub(crate) fn draft_title(title: &str, draft: bool) -> String {
    let bare = draft_prefix(title).map_or(title, |len| title[len..].trim_start());
    if draft {
        format!("WIP: {bare}")
    } else {
        bare.to_owned()
    }
}

/// The PRs numbered `numbers`, in order, read a few at a time.
async fn read_pulls(repo: &ForgeRepo, account: &Account, numbers: Vec<u64>) -> Result<Vec<Item>> {
    futures_util::stream::iter(numbers)
        .map(|number| async move {
            Ok::<_, anyhow::Error>(repo.read(account, ItemKind::Pr, number).await?.item)
        })
        .buffered(PULL_READS)
        .try_collect()
        .await
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
