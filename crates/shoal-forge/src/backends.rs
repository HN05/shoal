//! What differs between forges. Each forge implements [`Backend`];
//! [`ForgeKind`] names the one a repository uses and forwards each call to
//! it, so a new forge implements the trait and adds its variant here.
pub mod bitbucket;
pub(crate) mod forgejo;
pub(crate) mod github;
mod github_rest;
pub(crate) mod gitlab;
pub(crate) mod rest;

use anyhow::Result;
use serde_json::Value;

use anyhow::Context;

use crate::{
    ForgeRepo, MERGED_HINT,
    account::Account,
    capability::{Capability, Support},
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        checks::{CheckLog, plain},
        run::NewRun,
        run_action::{Run, RunOperation},
        run_view::{RunFilter, RunJob},
        runner::{JobCommand, JobLaunch, JobRun, RegisteredRunner, RunnerState, WaitingJob},
    },
    conventions::Lookup,
    items::{
        action::{Edit, MergeMethod, UpdateMethod},
        attach::{Attachment, Holder, Media},
        comment::Found,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
        reaction::{Listed, Subject, allowed::AllowedReactions},
        revert::NewRevert,
    },
    numbers::{CommentId, IssueNumber, JobId, PullNumber, RunId, RunNumber, RunnerId},
    repo::locator::ItemRoute,
    review::{
        diff::Diff,
        reviewers::Reviewer,
        status::PrStatus,
        submit::{PlacedReview, Submission},
        thread::Thread,
        view::{Details, Review},
    },
    transport::{
        api::Request,
        resend::Transient,
        response::{Fetched, Response},
    },
    updates::Snapshot,
};
use bitbucket::Bitbucket;
use forgejo::Forgejo;
use github::GitHub;
use gitlab::GitLab;

crate::state::states!(
    /// The API a forge host serves.
    ForgeKind: Variants {
        GitHub => "github",
        /// Forgejo, and Gitea, which serves the same API.
        Forgejo => "forgejo",
        GitLab => "gitlab",
        /// Bitbucket Cloud.
        Bitbucket => "bitbucket",
    }
);

impl ForgeKind {
    /// The well-known hosts, where Bitbucket means Bitbucket Cloud.
    pub fn named(host: &str) -> Option<Self> {
        match host {
            "github.com" => Some(Self::GitHub),
            "gitlab.com" => Some(Self::GitLab),
            "bitbucket.org" => Some(Self::Bitbucket),
            _ => None,
        }
    }

    /// What this process learned `host` serves, else the well-known hosts by
    /// name, else `linked`, the forge whose routes a link of the host uses;
    /// failing all three a host whose name has a `gitlab` label counts as
    /// GitLab and any other as Forgejo or Gitea.
    pub(crate) fn detect(host: &str, linked: Option<Self>) -> Self {
        crate::hosts::learned(host)
            .or_else(|| Self::named(host))
            .or(linked)
            .unwrap_or_else(|| {
                if host.split(['.', ':']).any(|label| label == "gitlab") {
                    Self::GitLab
                } else {
                    Self::Forgejo
                }
            })
    }

    /// Whether a token for the forge signs in together with the email of
    /// the account it belongs to.
    pub fn uses_account_email(self) -> bool {
        Backend::uses_account_email(&self)
    }
}

/// Run `$body` with `$backend` bound to the backend `$kind` names.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            ForgeKind::GitHub => {
                let $backend = GitHub;
                $body
            }
            ForgeKind::Forgejo => {
                let $backend = Forgejo;
                $body
            }
            ForgeKind::GitLab => {
                let $backend = GitLab;
                $body
            }
            ForgeKind::Bitbucket => {
                let $backend = Bitbucket;
                $body
            }
        }
    };
}

/// What a forge does its own way. Methods that need the repository take it
/// as `repo`.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Backend {
    /// Whether `host`, a repository without a path, answers this forge's
    /// version or metadata endpoint as `account`; an error when it could
    /// not be asked.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool>;

    /// The forge's name in messages.
    fn name(&self) -> &'static str;

    /// Whether the forge has `capability`.
    fn support(&self, capability: Capability) -> Support;

    /// Whether the server `repo` is on has `capability`, where the forge's
    /// versions differ in it.
    async fn server_support(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        Ok(self.support(capability))
    }

    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// What precedes a branch's name in the forge's branch URLs.
    fn branch_route(&self) -> &'static str;

    /// Whether `url` uses one of the forge's issue, PR or branch routes.
    fn links_item(&self, url: &str) -> bool {
        [
            self.issue_route().marker(),
            self.pull_route().marker(),
            self.branch_route(),
        ]
        .iter()
        .any(|route| url.contains(route))
    }

    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

    /// The REST collection PRs are under.
    fn pull_resource(&self) -> &'static str;

    /// The field that holds an issue's or PR's number in the repository.
    fn number_field(&self) -> &'static str;

    /// The field that holds a commit's full ID in a list of PR commits.
    fn commit_field(&self) -> &'static str;

    /// The ref a remote of the repository keeps PR `number`'s head
    /// commit under, where the forge keeps one after its branch goes.
    fn pull_head_ref(&self, number: PullNumber) -> Option<String>;

    /// What a failed read as the user most likely needs.
    fn login_hint(&self) -> &'static str;

    /// Whether a token signs in together with the email of the account it
    /// belongs to.
    fn uses_account_email(&self) -> bool {
        false
    }

    /// The Authorization header value that signs a request in with
    /// `token`, which belongs to `email` where the forge needs it.
    fn authorization(&self, token: &str, _email: Option<&str>) -> String {
        format!("Bearer {token}")
    }

    /// Send `request` as `account` and return the forge's response, which
    /// may report a failure.
    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response>;

    /// A failure the forge reports in the body of a successful response.
    fn reported_failure(&self, _body: &Value) -> Option<String> {
        None
    }

    /// The endpoint for a read of `endpoint` a caller wrote, such as
    /// through `shoal forge api`, so the forge applies the page size its
    /// query asks for.
    fn raw_read(&self, endpoint: String) -> String {
        endpoint
    }

    /// The query parameter that sizes a list page, and the size to ask for.
    fn page_size(&self) -> (&'static str, usize);

    /// A list page's items, and whether another page follows.
    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)>;

    /// The item an item endpoint returned as `value`; an issue read that
    /// returns a PR is refused.
    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current>;

    /// Whether issues and PRs share one sequence, so a number names one item.
    fn numbers_items_together(&self) -> bool;

    /// Issues or PRs that `search` matches, newest first, through the
    /// forge's search where it has one: the first `search.limit` matches at
    /// least. The caller drops items beyond the limit and those in a state
    /// the search leaves out.
    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>>;

    /// An issue's state; a PR's number is refused.
    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<ItemState> {
        rest::issue_state(repo, account, number).await
    }

    /// An open issue.
    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Current> {
        rest::issue_details(repo, account, number).await
    }

    /// The URLs of the open issues the forge records as blocking an issue.
    /// A forge without issue dependencies records none.
    async fn open_issue_dependencies(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: IssueNumber,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// The PR `number`, for checking out its head.
    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Current> {
        repo.read(account, ItemKind::Pr, number.get())
            .await
            .hint_login(self)
    }

    /// Fail closed on changed output: only commits the merged PR lists can
    /// authorize cleanup, including squash and rebase merges. None while the
    /// PR is unmerged.
    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<Vec<String>>> {
        rest::merged_commits(repo, account, number)
            .await
            .context(MERGED_HINT.trim_start_matches("; ").to_owned())
    }

    /// How many commits the base branch of PR `number` has that its head
    /// lacks; none once it is closed, or where the forge cannot tell.
    async fn behind(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Read what `edit` needs beyond `current` before it is planned.
    async fn prepare_edit(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _edit: &Edit,
        _current: &mut Current,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that change `current` as `edit` says, in order.
    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>>;

    /// Post `body` as a comment on the item.
    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request;

    /// Close or reopen the item.
    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request>;

    /// The requests that merge the open PR `current`.
    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that have the forge merge the open PR `current` once its
    /// required checks and approvals pass.
    fn auto_merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>>;

    /// The requests that withdraw the automatic merge of the open PR `current`.
    fn cancel_auto_merge(&self, repo: &ForgeRepo, current: &Current) -> Result<Vec<Request>>;

    /// The requests that bring the branch of the open PR `current` up to
    /// date with its base by `method`.
    fn update_branch(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>>;

    /// Have the forge open `revert` for the merged PR `current` as
    /// `account`, returning the new PR's number and head branch.
    async fn revert(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _current: &Current,
        _revert: &NewRevert,
    ) -> Result<(PullNumber, String)> {
        Err(crate::capability::unsupported(self, Capability::RevertPrs))
    }

    /// Where activity records a posted comment: its source, and the
    /// response field that holds its ID.
    fn comment_key(&self) -> (&'static str, &'static str);

    /// Where activity records a reply in a review thread.
    fn reply_key(&self) -> (&'static str, &'static str);

    /// The open PRs that may have `head` as their head, filtered as far
    /// as the forge can.
    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &Head<'_>,
    ) -> Result<Vec<Value>>;

    /// Whether a listed PR's head is `head`.
    fn has_head(&self, repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool;

    /// The request that reads `branch`, and the JSON pointer to its commit
    /// in the response.
    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str);

    /// The request that deletes `branch` from the repository.
    fn branch_delete(&self, repo: &ForgeRepo, branch: &str) -> Request;

    /// Whether `account` may push branches to the repository.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool>;

    /// `owner/repository` of `account`'s fork of the repository, which the
    /// forge creates when the account has none.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String>;

    /// The users and teams the repository's PRs can ask for a review.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>>;

    /// The request that opens `pull`.
    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request;

    /// The request that opens `issue`.
    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request;

    /// Whether a new issue's labels are added after opening it, because
    /// opening it cannot set them by name.
    fn labels_issue_after_opening(&self) -> bool;

    /// The review thread of `comment` on PR `number`, when it is in one.
    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        comment: CommentId,
    ) -> Result<Option<Thread>>;

    /// The request that replies in `thread` on PR `number`.
    fn reply(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        body: &str,
    ) -> Result<Request>;

    /// The request that resolves or unresolves `thread` on PR `number`.
    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request>;

    /// The diff of the PR `current`, at its head commit where the forge
    /// reads a diff there.
    async fn pr_diff(&self, repo: &ForgeRepo, account: &Account, current: &Current)
    -> Result<Diff>;

    /// Refuse a review of PR `number` as `account` that submitting would
    /// change more than the review on the forge.
    async fn check_review(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
    ) -> Result<()> {
        Ok(())
    }

    /// The requests that submit `review` of PR `number`.
    fn review(
        &self,
        repo: &ForgeRepo,
        number: PullNumber,
        review: &PlacedReview,
    ) -> Result<Submission>;

    /// The request that deletes the review draft a draft request's
    /// response describes, where the forge keeps drafts.
    fn discard_draft(
        &self,
        _repo: &ForgeRepo,
        _number: PullNumber,
        _draft: &Value,
    ) -> Result<Request> {
        anyhow::bail!("{} keeps no review drafts", self.name())
    }

    /// The activity keys of what `review` of PR `number` posted, from the
    /// `responses` to its submission's requests: by default each response
    /// describes a posted comment.
    async fn review_keys(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: PullNumber,
        _review: &PlacedReview<'_>,
        responses: &[Value],
    ) -> Result<Vec<String>> {
        let key = self.comment_key();
        Ok(responses
            .iter()
            .filter_map(|response| crate::updates::posted_key(key, response))
            .collect())
    }

    /// An issue's state and discussion.
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, discussion, reviews, mergeability and checks.
    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, mergeability, checks and review state.
    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        url: &str,
    ) -> Result<PrStatus>;

    /// The item's content for a view, with its comments when `comments` is
    /// set and the forge reads them with the item.
    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details>;

    /// Fills the comments `details` lacks and returns a PR's reviews,
    /// recording in `errors` the parts it could not read.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>>;

    /// The endpoint that lists the reactions on `subject` of item `number`.
    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String>;

    /// Whether the forge lists the reactions on `subject` in pages; by
    /// default it does.
    fn pages_reactions(&self, _subject: Subject) -> bool {
        true
    }

    /// A reaction as the forge lists it, with GitHub's name for its content.
    fn listed_reaction(&self, reaction: &Value) -> Result<Listed>;

    /// The login `account` signs in to the forge as; none when it reads
    /// anonymously.
    async fn own_login(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>>;

    /// Whether the login of `account` wrote `comment` of item `number`.
    async fn wrote(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        _kind: ItemKind,
        _number: u64,
        comment: &Found,
    ) -> Result<bool> {
        let login = (self.own_login(repo, account).await?)
            .context("editing a comment needs a login on the forge")?;
        Ok(comment.author.eq_ignore_ascii_case(&login))
    }

    /// The content [`Self::listed_reaction`] reports for a reaction the
    /// forge names `name`; by default the name itself.
    fn reaction_content(&self, name: &str) -> String {
        name.to_owned()
    }

    /// The request that adds a reaction of `content` to the reactions at
    /// `endpoint`.
    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request>;

    /// The request that removes `own`, the account's reaction, from the
    /// reactions at `endpoint`.
    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request>;

    /// The reactions the forge allows; by default GitHub's, and only those.
    async fn allowed_reactions(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
    ) -> Result<AllowedReactions> {
        Ok(AllowedReactions::github(false))
    }

    /// Each check's latest attempt on `head`.
    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>>;

    /// The CI job a check's web URL links in the repository, if any.
    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location>;

    /// The operation that applies `action` to `job`, one of `selected` among
    /// the head's `jobs`, and the checks it reaches.
    fn ci_operation(
        &self,
        jobs: &[Job],
        selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)>;

    /// The endpoint under the repository that carries out `operation`.
    async fn ci_endpoint(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _head: &str,
        operation: Operation,
    ) -> Result<String> {
        match operation {
            Operation::Post(endpoint) => Ok(endpoint),
            Operation::ForgejoRun { .. } | Operation::RerunForgejoJob { .. } => {
                anyhow::bail!("not a Forgejo repository")
            }
        }
    }

    /// The log of the failed check `name` on the current head of PR `url`.
    async fn failed_check_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        repo.failed_job_log(account, url, name).await
    }

    /// The request that starts `run`, or why the forge cannot start it.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request>;

    /// The web URL of the run a start request's `response` reports, if any.
    fn started_run_url(&self, _repo: &ForgeRepo, _response: &Value) -> Option<String> {
        None
    }

    /// The default branch a read of the repository's own endpoint returned
    /// as `value`.
    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["default_branch"].as_str()
    }

    /// The number a workflow run's web URL in the repository names, if
    /// `target` links one.
    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber>;

    /// The path under the repository that reads the run numbered `number`
    /// in its web URL.
    fn run_path(&self, number: RunNumber) -> String;

    /// What precedes a run's number in the forge's run web URLs, or `None`
    /// where Shoal reads no runs.
    fn run_route(&self) -> Option<&'static str> {
        Some("/actions/runs/")
    }

    /// The run numbered `number` that a read of its path returned as `value`.
    fn run(&self, repo: &ForgeRepo, _number: RunNumber, value: &Value) -> Result<Run> {
        self.run_item(repo, value)
    }

    /// One run as the forge reads or lists it.
    fn run_item(&self, repo: &ForgeRepo, value: &Value) -> Result<Run>;

    /// The path under the repository that lists the runs `filter` selects,
    /// newest first.
    fn runs_path(&self, filter: &RunFilter) -> Result<String>;

    /// The path under the repository that lists page `page` of the runs on
    /// the commit `head` that may wait for approval,
    /// [`crate::ci::approval::RUNS_PAGE`] to a page, or `None` where the forge
    /// holds no runs for it.
    fn waiting_runs_path(&self, _head: &str, _page: u32) -> Option<String> {
        None
    }

    /// The runs a read of the list path returned.
    fn listed_runs<'a>(&self, value: &'a Value) -> Option<&'a [Value]> {
        value["workflow_runs"].as_array().map(Vec::as_slice)
    }

    /// The path under the repository that lists page `page` of `run`'s
    /// jobs, [`crate::ci::run_view::JOBS_PAGE`] to a page, or `None` past the
    /// last page the forge
    /// can return.
    fn run_jobs_path(&self, run: &Run, page: u32) -> Option<String>;

    /// The jobs of `run` that a read of a page of its jobs returned as
    /// `value`, after `offset` jobs on earlier pages.
    fn run_jobs(&self, run: &Run, offset: usize, value: &Value) -> Result<Vec<RunJob>>;

    /// The log of `run`'s `job` as plain text.
    async fn run_job_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        run: &Run,
        job: &RunJob,
    ) -> Result<String>;

    /// The requests that apply `action` to `run`; `failed` reruns only its
    /// failed jobs.
    fn run_operation(&self, run: &Run, action: CiAction, failed: bool) -> Result<RunOperation>;

    /// The program that runs one job on this machine.
    fn runner_program(&self) -> Result<&'static str> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The jobs waiting for a runner with `labels`.
    async fn waiting_jobs(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _labels: &[String],
    ) -> Result<Vec<WaitingJob>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The path under the repository that reads the run with API ID `id`.
    fn job_run_path(&self, _id: RunId) -> Result<String> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The run a read of its path returned as `value`.
    fn job_run(&self, _repo: &ForgeRepo, _value: &Value) -> Result<JobRun> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// Register a runner named `name` for the repository, or, where the
    /// forge registers one per job, check that `account` may.
    async fn register_runner(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _name: &str,
    ) -> Result<Option<RegisteredRunner>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The request that deletes the repository's runner `id`.
    fn delete_runner(&self, _repo: &ForgeRepo, _id: RunnerId) -> Result<Request> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The command that claims and runs the job `launch` names.
    async fn job_command(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _launch: &JobLaunch<'_>,
    ) -> Result<JobCommand> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The state of the runner `id`, or `None` once it is gone.
    async fn runner_state(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _id: RunnerId,
    ) -> Result<Option<RunnerState>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The job of `run` that the runner `runner` took, if any.
    async fn runner_job(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _run: RunId,
        _runner: RunnerId,
    ) -> Result<Option<WaitingJob>> {
        Err(crate::capability::unsupported(self, Capability::RunJobs))
    }

    /// The endpoint under the repository that approves `run`, where the
    /// forge approves runs.
    fn approve_endpoint(&self, _run: &Run) -> Result<String> {
        Err(crate::capability::unsupported(
            self,
            Capability::ApproveRuns,
        ))
    }

    /// `log`, a job log as the forge serves it, as plain text.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(log)
    }

    /// The step the plain job log `log` names as failed, where the forge's
    /// runner marks its steps.
    fn failed_step(&self, _log: &str) -> Option<String> {
        None
    }

    /// The step that failed in the job with API ID `job`, where the forge
    /// lists a job's steps with their states.
    async fn listed_failed_step(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _job: JobId,
    ) -> Option<String> {
        None
    }

    /// The endpoint under the repository that serves the plain-text log of
    /// the job at `location`.
    fn job_log_endpoint(&self, _location: Location) -> Result<String> {
        Err(crate::capability::unsupported(
            self,
            Capability::ReadJobLogs,
        ))
    }

    /// Refuse `attachments` the forge or `account` cannot upload, before
    /// anything is posted. Returns whether the forge keeps uploads with the
    /// item or comment that shows them rather than with the repository.
    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool>;

    /// The response to a GET of `url`, a file on the forge's web host,
    /// sent as `account`.
    async fn download(&self, _repo: &ForgeRepo, _account: &Account, _url: &str) -> Result<Fetched> {
        Err(crate::capability::unsupported(self, Capability::Downloads))
    }

    /// The repository's numeric ID, where the forge's uploads name it.
    async fn repository_id(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<u64>> {
        Ok(None)
    }

    /// The request that uploads `attachment` to `holder`.
    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request>;

    /// Where an upload's response says the file is served.
    fn uploaded_url(&self, response: &Value) -> Result<String> {
        served_at(response, "url")
    }

    /// The Markdown that shows an uploaded image or video, or links to
    /// another file.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::File => link(alt, url),
            Media::Image | Media::Video => image(alt, url),
        }
    }

    /// The request that replaces the body of the comment `subject` of item
    /// `number`.
    fn edit_comment(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
        body: &str,
    ) -> Result<Request>;

    /// Where the forge looks for the description templates of a new item
    /// of `kind`, in its lookup order.
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)>;

    /// Every label the repository's items can take, as the forge lists them.
    async fn labels(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Value>> {
        repo.pages(account, &repo.repo_endpoint("labels")).await
    }

    /// Whether the label `name`, as the forge lists it in `label`, allows an
    /// item at most one label of its scope.
    fn exclusive_label(&self, _name: &str, _label: &Value) -> bool {
        false
    }

    /// Whether label names differing only in case name the same label.
    fn label_names_fold_case(&self) -> bool {
        false
    }

    /// The paginated list of the repository's milestones, open and closed.
    fn milestones_endpoint(&self, repo: &ForgeRepo) -> String {
        repo.repo_endpoint("milestones?state=all")
    }

    /// The title of a listed milestone and the ID edits take for it.
    fn milestone(&self, listed: &Value) -> Option<(String, u64)> {
        Some((listed["title"].as_str()?.to_owned(), listed["id"].as_u64()?))
    }
}

/// Adds a backend's login hint to a failed read.
pub(crate) trait LoginHint<T> {
    /// This result, with `backend`'s login hint on a failure a login may
    /// fix: not one that may pass, such as a refused connection.
    fn hint_login(self, backend: &(impl Backend + ?Sized)) -> Result<T>;
}

impl<T> LoginHint<T> for Result<T> {
    fn hint_login(self, backend: &(impl Backend + ?Sized)) -> Result<T> {
        self.map_err(|error| match error.downcast_ref::<Transient>() {
            Some(_) => error,
            None => error.context(backend.login_hint()),
        })
    }
}

/// The URL in an upload response's `field`.
fn served_at(response: &Value, field: &str) -> Result<String> {
    response[field]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .context("the forge did not report where it saved the file")
}

/// The Markdown image of `url`.
fn image(alt: &str, url: &str) -> String {
    format!("!{}", link(alt, url))
}

/// The Markdown link to `url`.
fn link(text: &str, url: &str) -> String {
    format!("[{}]({url})", text.replace(['[', ']'], ""))
}

#[cfg(test)]
mod tests;
