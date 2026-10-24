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
    },
    conventions::Lookup,
    items::{
        action::{Edit, MergeMethod},
        attach::{Attachment, Holder},
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    review::{
        status::PrStatus,
        thread::Thread,
        view::{Details, Review},
    },
    transport::{api::Request, resend::Transient, response::Response},
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
    /// failing all three a host glab has a login for, or whose name has a
    /// `gitlab` label, counts as GitLab and any other as Forgejo or Gitea.
    pub(crate) fn detect(host: &str, linked: Option<Self>) -> Self {
        crate::hosts::learned(host)
            .or_else(|| Self::named(host))
            .or(linked)
            .unwrap_or_else(|| {
                if host.split(['.', ':']).any(|label| label == "gitlab")
                    || gitlab::glab::has_login(host)
                {
                    Self::GitLab
                } else {
                    Self::Forgejo
                }
            })
    }
}

/// What a forge does its own way. Methods that need the repository take it
/// as `repo`.
pub(crate) trait Backend {
    /// Whether `host`, a repository without a path, answers this forge's
    /// version or metadata endpoint as `account`; an error when it could
    /// not be asked.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool>;

    /// The forge's name in messages.
    fn name(&self) -> &'static str;

    /// Whether the forge has `capability`.
    fn support(&self, capability: Capability) -> Support;

    /// Whether a repository path may have more than `owner/repository`.
    fn nests_groups(&self) -> bool;

    fn issue_route(&self) -> ItemRoute;

    fn pull_route(&self) -> ItemRoute;

    /// The API endpoint for `path` under the repository.
    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String;

    /// The REST collection PRs are under.
    fn pull_resource(&self) -> &'static str;

    /// The field that holds an issue's or PR's number in the repository.
    fn number_field(&self) -> &'static str;

    /// The field that holds a commit's full ID in a list of PR commits.
    fn commit_field(&self) -> &'static str;

    /// What a failed read as the user most likely needs.
    fn login_hint(&self) -> &'static str;

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
        number: u64,
    ) -> Result<ItemState> {
        rest::issue_state(repo, account, number).await
    }

    /// An open issue's title and description.
    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        rest::issue_details(repo, account, number).await
    }

    /// The URLs of the open issues the forge records as blocking an issue.
    /// A forge without issue dependencies records none.
    async fn open_issue_dependencies(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _number: u64,
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Title, head and base of a PR whose head branch is in the repository.
    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        rest::pull_details(repo, account, number)
            .await
            .hint_login(self)
    }

    /// Fail closed on changed output: only commits the merged PR lists can
    /// authorize cleanup, including squash and rebase merges. None while the
    /// PR, which must have `branch` as its head, is unmerged.
    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        rest::merged_commits(repo, account, number, branch)
            .await
            .context(MERGED_HINT.trim_start_matches("; ").to_owned())
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

    /// Whether `account` may push branches to the repository.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool>;

    /// `owner/repository` of `account`'s fork of the repository, which the
    /// forge creates when the account has none.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String>;

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
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>>;

    /// The request that replies in `thread` on PR `number`.
    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request>;

    /// The request that resolves or unresolves `thread` on PR `number`.
    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: u64,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request>;

    /// An issue's state and discussion.
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot>;

    /// A PR's state, discussion, reviews, mergeability and checks. The PR
    /// must have `branch` as its head when one is given.
    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot>;

    /// A PR's state, mergeability, checks and review state.
    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
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
            Operation::CancelForgejoRun { .. } => anyhow::bail!("not a Forgejo repository"),
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

    /// `log`, a job log as the forge serves it, as plain text.
    fn plain_log(&self, log: &str) -> String {
        plain::readable(log)
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

    /// The Markdown that shows an uploaded file.
    fn markup(&self, _attachment: &Attachment, alt: &str, url: &str) -> String {
        image(alt, url)
    }

    /// The request that replaces the body of the discussion comment `id`,
    /// where the forge keeps uploads with comments.
    fn edit_comment(&self, _repo: &ForgeRepo, _id: u64, _body: &str) -> Result<Request> {
        anyhow::bail!("this forge keeps attached files with the repository")
    }

    /// Where the forge looks for the description templates of a new item
    /// of `kind`, in its lookup order.
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)>;

    /// Whether the label `name`, as the forge lists it in `label`, allows an
    /// item at most one label of its scope.
    fn exclusive_label(&self, _name: &str, _label: &Value) -> bool {
        false
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
    format!("![{}]({url})", alt.replace(['[', ']'], ""))
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

impl Backend for ForgeKind {
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        dispatch!(self, backend => backend.serves(host, account).await)
    }

    fn name(&self) -> &'static str {
        dispatch!(self, backend => backend.name())
    }

    fn support(&self, capability: Capability) -> Support {
        dispatch!(self, backend => backend.support(capability))
    }

    fn nests_groups(&self) -> bool {
        dispatch!(self, backend => backend.nests_groups())
    }

    fn issue_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.issue_route())
    }

    fn pull_route(&self) -> ItemRoute {
        dispatch!(self, backend => backend.pull_route())
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        dispatch!(self, backend => backend.repo_endpoint(repo, path))
    }

    fn pull_resource(&self) -> &'static str {
        dispatch!(self, backend => backend.pull_resource())
    }

    fn number_field(&self) -> &'static str {
        dispatch!(self, backend => backend.number_field())
    }

    fn commit_field(&self) -> &'static str {
        dispatch!(self, backend => backend.commit_field())
    }

    fn login_hint(&self) -> &'static str {
        dispatch!(self, backend => backend.login_hint())
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        dispatch!(self, backend => backend.send(repo, account, request).await)
    }

    fn reported_failure(&self, body: &Value) -> Option<String> {
        dispatch!(self, backend => backend.reported_failure(body))
    }

    fn page_size(&self) -> (&'static str, usize) {
        dispatch!(self, backend => backend.page_size())
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        dispatch!(self, backend => backend.page(response, size))
    }

    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        dispatch!(self, backend => backend.current(repo, kind, number, value))
    }

    fn numbers_items_together(&self) -> bool {
        dispatch!(self, backend => backend.numbers_items_together())
    }

    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>> {
        dispatch!(self, backend => backend.list_items(repo, account, kind, search).await)
    }

    async fn issue_state(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<ItemState> {
        dispatch!(self, backend => backend.issue_state(repo, account, number).await)
    }

    async fn issue_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String)> {
        dispatch!(self, backend => backend.issue_details(repo, account, number).await)
    }

    async fn open_issue_dependencies(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<String>> {
        dispatch!(self, backend => backend.open_issue_dependencies(repo, account, number).await)
    }

    async fn pull_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(String, String, String)> {
        dispatch!(self, backend => backend.pull_details(repo, account, number).await)
    }

    async fn merged_commits(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        dispatch!(self, backend => backend.merged_commits(repo, account, number, branch).await)
    }
    async fn prepare_edit(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        edit: &Edit,
        current: &mut Current,
    ) -> Result<()> {
        dispatch!(self, backend => backend.prepare_edit(repo, account, edit, current).await)
    }

    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        dispatch!(self, backend => backend.edit(repo, current, edit))
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        dispatch!(self, backend => backend.comment(repo, item, body))
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        dispatch!(self, backend => backend.set_state(repo, item, state))
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        dispatch!(self, backend => backend.merge(repo, current, method, delete_branch))
    }

    fn comment_key(&self) -> (&'static str, &'static str) {
        dispatch!(self, backend => backend.comment_key())
    }

    fn reply_key(&self) -> (&'static str, &'static str) {
        dispatch!(self, backend => backend.reply_key())
    }

    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &Head<'_>,
    ) -> Result<Vec<Value>> {
        dispatch!(self, backend => backend.open_pulls_from(repo, account, head).await)
    }

    fn has_head(&self, repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        dispatch!(self, backend => backend.has_head(repo, pull, head))
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        dispatch!(self, backend => backend.branch_read(repo, branch))
    }

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        dispatch!(self, backend => backend.pushable(repo, account).await)
    }

    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String> {
        dispatch!(self, backend => backend.fork(repo, account).await)
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        dispatch!(self, backend => backend.new_pull(repo, pull))
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        dispatch!(self, backend => backend.new_issue(repo, issue))
    }

    fn labels_issue_after_opening(&self) -> bool {
        dispatch!(self, backend => backend.labels_issue_after_opening())
    }

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>> {
        dispatch!(self, backend => backend.thread(repo, account, number, comment).await)
    }

    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        dispatch!(self, backend => backend.reply(repo, number, thread, body))
    }

    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: u64,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        dispatch!(self, backend => backend.resolution(repo, number, thread, resolved))
    }

    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Snapshot> {
        dispatch!(self, backend => backend.issue_activity(repo, account, number).await)
    }

    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        branch: Option<&str>,
    ) -> Result<Snapshot> {
        dispatch!(self, backend => backend.pr_activity(repo, account, number, branch).await)
    }

    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        url: &str,
    ) -> Result<PrStatus> {
        dispatch!(self, backend => backend.pr_status(repo, account, number, url).await)
    }

    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details> {
        dispatch!(self, backend => backend.view_details(repo, account, kind, number, comments).await)
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
        dispatch!(self, backend => backend.view_discussion(repo, account, kind, number, details, errors).await)
    }

    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        dispatch!(self, backend => backend.ci_checks(repo, account, head).await)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        dispatch!(self, backend => backend.locate_job(repo, target))
    }

    fn ci_operation(
        &self,
        jobs: &[Job],
        selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        dispatch!(self, backend => backend.ci_operation(jobs, selected, job, action))
    }

    async fn ci_endpoint(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
        operation: Operation,
    ) -> Result<String> {
        dispatch!(self, backend => backend.ci_endpoint(repo, account, head, operation).await)
    }

    async fn failed_check_log(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        dispatch!(self, backend => backend.failed_check_log(repo, account, url, name).await)
    }

    fn plain_log(&self, log: &str) -> String {
        dispatch!(self, backend => backend.plain_log(log))
    }

    fn job_log_endpoint(&self, location: Location) -> Result<String> {
        dispatch!(self, backend => backend.job_log_endpoint(location))
    }

    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool> {
        dispatch!(self, backend => backend.check_uploads(repo, account, attachments).await)
    }

    async fn repository_id(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<u64>> {
        dispatch!(self, backend => backend.repository_id(repo, account).await)
    }

    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request> {
        dispatch!(self, backend => backend.upload(repo, holder, attachment, repository))
    }

    fn uploaded_url(&self, response: &Value) -> Result<String> {
        dispatch!(self, backend => backend.uploaded_url(response))
    }

    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        dispatch!(self, backend => backend.markup(attachment, alt, url))
    }

    fn edit_comment(&self, repo: &ForgeRepo, id: u64, body: &str) -> Result<Request> {
        dispatch!(self, backend => backend.edit_comment(repo, id, body))
    }

    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        dispatch!(self, backend => backend.template_lookups(kind))
    }

    fn exclusive_label(&self, name: &str, label: &Value) -> bool {
        dispatch!(self, backend => backend.exclusive_label(name, label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_failures_a_login_may_fix_carry_the_login_hint() {
        let hinted = |error: anyhow::Error| {
            let failed: Result<()> = Err(error.context("GET pulls/1"));
            format!("{:#}", failed.hint_login(&ForgeKind::Forgejo).unwrap_err())
        };
        let refused = hinted(Transient::Unsent("curl failed (exit status: 7)".into()).into());
        assert_eq!(refused, "GET pulls/1: curl failed (exit status: 7)");
        let denied = hinted(anyhow::anyhow!("HTTP 404"));
        assert_eq!(
            denied,
            format!("{}: GET pulls/1: HTTP 404", ForgeKind::Forgejo.login_hint())
        );
    }

    #[test]
    fn well_known_and_gitlab_named_hosts_are_recognized() {
        for (host, kind) in [
            ("github.com", ForgeKind::GitHub),
            ("gitlab.com", ForgeKind::GitLab),
            ("bitbucket.org", ForgeKind::Bitbucket),
            ("gitlab.corp.example", ForgeKind::GitLab),
            ("git.corp.example", ForgeKind::Forgejo),
            ("mygitlab.example", ForgeKind::Forgejo),
            ("codeberg.org", ForgeKind::Forgejo),
        ] {
            assert_eq!(ForgeKind::detect(host, None), kind, "{host}");
        }
    }
}
