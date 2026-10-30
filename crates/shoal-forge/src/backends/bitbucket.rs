//! Bitbucket Cloud: its REST shapes, and the credentials Shoal sends to it.
//! Bitbucket has no official CLI to own a login, so a user-configured
//! command supplies a token for each request; Shoal stores none.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    backends::rest::{RestCheck, RestReads, fingerprints},
    capability::{self, Capability, Support},
    ci::{
        CiAction, CiCheck, Job, Location, Operation,
        run::NewRun,
        run_action::Run,
        run_view::{RunFilter, RunJob},
    },
    conventions::Lookup,
    items::{
        action::{Edit, MergeMethod, UpdateMethod},
        attach::{Attachment, Holder},
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
        list,
        reaction::{Listed, Subject},
    },
    repo::locator::ItemRoute,
    review::{
        diff::Diff,
        reviewers::Reviewer,
        status::{PrStatus, Verdict},
        submit::{PlacedReview, Submission},
        thread::{Target, Thread},
        view::{Comment, Details, PrDetails, Review, ReviewComment},
    },
    transport::{
        api::{HttpMethod, Request, encode},
        response::Response,
    },
    updates::{DISCUSSION, Entry, Snapshot},
};

mod review;

use super::{Backend, LoginHint, rest};

/// Bitbucket Cloud's API, which is not served from its web host.
const API: &str = "https://api.bitbucket.org/2.0";
/// The page size Bitbucket's lists take by default.
const PAGE_SIZE: usize = 50;

/// The issue states Shoal reads as open, and as closed.
const OPEN_ISSUE_STATES: [&str; 3] = ["new", "open", "on hold"];
const CLOSED_ISSUE_STATES: [&str; 5] = ["resolved", "invalid", "duplicate", "wontfix", "closed"];

#[derive(Clone, Copy)]
pub(crate) struct Bitbucket;

impl Backend for Bitbucket {
    /// Bitbucket Cloud serves only `bitbucket.org`, which is known by name.
    async fn serves(&self, _host: &ForgeRepo, _account: &Account) -> Result<bool> {
        Ok(false)
    }

    fn name(&self) -> &'static str {
        "Bitbucket Cloud"
    }

    /// Shoal starts Pipelines but reads none of their jobs.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::ReopenPrs => Support::Unsupported(Some(
                "Bitbucket cannot reopen a declined pull request; open a new one",
            )),
            Capability::Labels => Support::Unsupported(Some("Bitbucket has no labels")),
            Capability::Attachments => {
                Support::Unsupported(Some("Bitbucket has no API for attaching files"))
            }
            Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReadJobLogs
            | Capability::ReadRuns => Support::Unsupported(None),
            Capability::Reactions => Support::Unsupported(Some("Bitbucket has no reactions")),
            Capability::AutoMerge => {
                Support::Unsupported(Some("Bitbucket's API cannot merge a pull request later"))
            }
            Capability::UpdateByMerge | Capability::UpdateByRebase => Support::Unsupported(Some(
                "Bitbucket has no API for updating a PR's branch, so update it with Git and push it",
            )),
            Capability::ResolveThreads
            | Capability::DraftPrs
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::ForkPrs
            | Capability::StartRuns
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges => Support::Supported,
            Capability::Forks => Support::Unsupported(Some(
                "fork it on Bitbucket, add the fork as a remote and set remote.pushDefault to it",
            )),
        }
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::PullRequests
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repositories/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pullrequests"
    }

    fn number_field(&self) -> &'static str {
        "id"
    }

    fn commit_field(&self) -> &'static str {
        "hash"
    }

    fn login_hint(&self) -> &'static str {
        "Bitbucket API request failed; private repositories need [machine.bitbucket] token_command"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        let url = format!("{API}/{}", request.endpoint);
        let authorization = account.bitbucket_authorization().await?;
        repo.curl_send(&url, authorization, request).await
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("pagelen", PAGE_SIZE)
    }

    /// Bitbucket links the next page.
    fn page<'a>(&self, response: &'a Value, _size: usize) -> Option<(&'a [Value], bool)> {
        Some((response["values"].as_array()?, !response["next"].is_null()))
    }

    /// An issue or pull request as Bitbucket reports it.
    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        let state = match (kind, value["state"].as_str()) {
            (ItemKind::Pr, Some("OPEN")) => ItemState::Open,
            (ItemKind::Pr, Some("MERGED")) => ItemState::Merged,
            (ItemKind::Pr, Some("DECLINED" | "SUPERSEDED")) => ItemState::Closed,
            (ItemKind::Issue, Some(state)) if OPEN_ISSUE_STATES.contains(&state) => ItemState::Open,
            (ItemKind::Issue, Some(state)) if CLOSED_ISSUE_STATES.contains(&state) => {
                ItemState::Closed
            }
            (_, state) => bail!("unrecognized Bitbucket state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: text(&value["source"]["branch"]["name"])
                    .context("pull request has no source branch")?,
                base: text(&value["destination"]["branch"]["name"])
                    .context("pull request has no destination branch")?,
                draft: value["draft"].as_bool().unwrap_or_default(),
            }),
        };
        let body = match kind {
            ItemKind::Issue => &value["content"]["raw"],
            ItemKind::Pr => &value["description"],
        };
        Ok(Current {
            item: Item {
                kind,
                number,
                url: repo.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: Vec::new(),
                pr,
            },
            node_id: None,
            head_repository: text(&value["source"]["repository"]["full_name"]),
            body: text(body).unwrap_or_default(),
            author: text(&value["author"]["account_id"]),
            head_sha: text(&value["source"]["commit"]["hash"]),
            base_sha: text(&value["destination"]["commit"]["hash"]),
            // Bitbucket does not report whether a pull request conflicts.
            conflict: None,
            merges_now: false,
            reviewers: value["reviewers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|user| text(&user["account_id"]))
                .collect(),
            user_ids: Default::default(),
            replaced_labels: Vec::new(),
        })
    }

    fn numbers_items_together(&self) -> bool {
        false
    }

    async fn list_items(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        search: &list::Search,
    ) -> Result<Vec<Item>> {
        let endpoint = search_endpoint(kind, search);
        rest::list_items(repo, account, kind, &endpoint, search).await
    }

    /// One update of the changed fields. Bitbucket has no labels, keeps
    /// draft state as a flag, and replaces reviewers by Atlassian account ID.
    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        if !edit.add_labels.is_empty() || !edit.remove_labels.is_empty() {
            return Err(capability::unsupported(self, Capability::Labels));
        }
        let item = &current.item;
        // A pull request update needs its title even when it keeps it.
        let mut fields = json!({ "title": edit.title.as_deref().unwrap_or(&item.title) });
        if let Some(body) = &edit.body {
            match item.kind {
                ItemKind::Issue => fields["content"] = json!({ "raw": body }),
                ItemKind::Pr => fields["description"] = body.clone().into(),
            }
        }
        if let Some(base) = &edit.base {
            fields["destination"] = json!({ "branch": { "name": base } });
        }
        if let Some(draft) = edit.draft {
            fields["draft"] = draft.into();
        }
        if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
            let mut reviewers: Vec<&String> = current
                .reviewers
                .iter()
                .filter(|id| !edit.remove_reviewers.contains(id))
                .collect();
            for id in &edit.add_reviewers {
                if !reviewers.contains(&id) {
                    reviewers.push(id);
                }
            }
            fields["reviewers"] = reviewers
                .into_iter()
                .map(|id| json!({ "account_id": id }))
                .collect();
        }
        Ok(vec![Request::new(
            HttpMethod::Put,
            repo.item_endpoint(item.kind, item.number),
            Some(fields),
        )])
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        let endpoint = format!("{}/comments", repo.item_endpoint(item.kind, item.number));
        Request::new(
            HttpMethod::Post,
            endpoint,
            Some(json!({ "content": { "raw": body } })),
        )
    }

    /// Decline a pull request, or resolve or reopen an issue. Bitbucket
    /// cannot reopen a declined pull request.
    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        let endpoint = repo.item_endpoint(item.kind, item.number);
        Ok(match (item.kind, state) {
            (ItemKind::Pr, ItemState::Open) => {
                return Err(capability::unsupported(self, Capability::ReopenPrs));
            }
            (ItemKind::Pr, _) => {
                Request::new(HttpMethod::Post, format!("{endpoint}/decline"), None)
            }
            (ItemKind::Issue, ItemState::Open) => {
                Request::new(HttpMethod::Put, endpoint, Some(json!({ "state": "open" })))
            }
            (ItemKind::Issue, _) => Request::new(
                HttpMethod::Put,
                endpoint,
                Some(json!({ "state": "resolved" })),
            ),
        })
    }

    fn merge(
        &self,
        repo: &ForgeRepo,
        current: &Current,
        method: MergeMethod,
        delete_branch: bool,
    ) -> Result<Vec<Request>> {
        let endpoint = repo.item_endpoint(ItemKind::Pr, current.item.number);
        Ok(vec![merge_request(
            format!("{endpoint}/merge"),
            method,
            delete_branch,
        )])
    }

    fn auto_merge(
        &self,
        _repo: &ForgeRepo,
        _current: &Current,
        _method: MergeMethod,
        _delete_branch: bool,
    ) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::AutoMerge))
    }

    fn cancel_auto_merge(&self, _repo: &ForgeRepo, _current: &Current) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::AutoMerge))
    }

    fn update_branch(
        &self,
        _repo: &ForgeRepo,
        _current: &Current,
        method: UpdateMethod,
    ) -> Result<Vec<Request>> {
        Err(capability::unsupported(self, Capability::update(method)))
    }

    fn comment_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "id")
    }

    fn reply_key(&self) -> (&'static str, &'static str) {
        (DISCUSSION, "id")
    }

    async fn open_pulls_from(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &Head<'_>,
    ) -> Result<Vec<Value>> {
        let query = encode(
            &format!("source.branch.name={}", literal(head.branch)),
            false,
        );
        let endpoint = format!("pullrequests?state=OPEN&q={query}");
        repo.pages(account, &repo.repo_endpoint(&endpoint)).await
    }

    fn has_head(&self, _repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        pull["source"]["branch"]["name"] == head.branch
            && pull["source"]["repository"]["full_name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(head.repository))
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        let endpoint = repo.repo_endpoint(&format!("refs/branches/{}", encode(branch, false)));
        (Request::get(endpoint), "/target/hash")
    }

    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool> {
        let query = encode(
            &format!("repository.full_name={}", literal(&repo.path)),
            false,
        );
        let endpoint = format!("user/permissions/repositories?q={query}");
        let grants = repo.pages(account, &endpoint).await?;
        Ok(grants
            .iter()
            .any(|grant| matches!(grant["permission"].as_str(), Some("write" | "admin"))))
    }

    async fn fork(&self, _repo: &ForgeRepo, _account: &Account) -> Result<String> {
        Err(capability::unsupported(self, Capability::Forks))
    }

    /// The members of the repository's workspace, by Atlassian account ID.
    async fn reviewers(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Reviewer>> {
        let endpoint = format!("workspaces/{}/members", repo.owner());
        let members = repo.pages(account, &endpoint).await?;
        Ok(members
            .iter()
            .filter_map(|member| {
                let user = &member["user"];
                Some(Reviewer::user(
                    text(&user["account_id"])?,
                    user["display_name"].as_str(),
                ))
            })
            .collect())
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let mut source = json!({"branch": {"name": pull.head}});
        if let Some(repository) = &pull.head_repository {
            source["repository"] = json!({"full_name": repository});
        }
        let body = json!({"title": pull.title, "description": pull.body, "draft": pull.draft,
            "source": source, "destination": {"branch": {"name": pull.base}}});
        Request::new(
            HttpMethod::Post,
            repo.repo_endpoint("pullrequests"),
            Some(body),
        )
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "content": { "raw": issue.body } });
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
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        let comments = repo.pages(account, &format!("{pr}/comments")).await?;
        Ok(find_thread(&comments, comment))
    }

    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        Ok(Request::new(
            HttpMethod::Post,
            format!("{pr}/comments"),
            Some(json!({ "content": { "raw": body }, "parent": { "id": thread.first } })),
        ))
    }

    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: u64,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        let method = if resolved {
            HttpMethod::Post
        } else {
            HttpMethod::Delete
        };
        Ok(Request::new(
            method,
            format!("{pr}/comments/{}/resolve", thread.first),
            None,
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
        view_details(repo, account, kind, number)
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
        _errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>> {
        view_discussion(repo, account, kind, number, details)
            .await
            .hint_login(self)
    }

    fn reactions_endpoint(
        &self,
        _repo: &ForgeRepo,
        _kind: ItemKind,
        _number: u64,
        _subject: Subject,
    ) -> Result<String> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn listed_reaction(&self, _reaction: &Value) -> Result<Listed> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    async fn own_login(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<String>> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn add_reaction(&self, _endpoint: String, _content: &str) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn remove_reaction(&self, _endpoint: String, _own: &Listed) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    /// No CI job Shoal can act on, since it reads no Pipelines jobs.
    async fn ci_checks(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _head: &str,
    ) -> Result<Vec<CiCheck>> {
        Ok(Vec::new())
    }

    fn locate_job(&self, _repo: &ForgeRepo, _target: &str) -> Option<Location> {
        None
    }

    fn ci_operation(
        &self,
        _jobs: &[Job],
        _selected: &[Job],
        job: &Job,
        _action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        bail!("check {} is not a CI job Shoal can act on", job.name)
    }

    /// A workflow names a custom pipeline; without one the branch's own
    /// pipeline runs. Inputs are its variables.
    fn start_run(&self, repo: &ForgeRepo, run: &NewRun) -> Result<Request> {
        let mut target = json!({
            "type": "pipeline_ref_target",
            "ref_type": "branch",
            "ref_name": run.branch,
        });
        if let Some(pipeline) = &run.workflow {
            target["selector"] = json!({"type": "custom", "pattern": pipeline});
        }
        let variables = run
            .inputs
            .iter()
            .map(|(key, value)| json!({"key": key, "value": value}))
            .collect::<Vec<_>>();
        Ok(Request::new(
            HttpMethod::Post,
            repo.repo_endpoint("pipelines"),
            Some(json!({"target": target, "variables": variables})),
        ))
    }

    fn locate_run(&self, _repo: &ForgeRepo, _target: &str) -> Option<u64> {
        None
    }

    fn run_path(&self, number: u64) -> String {
        format!("pipelines/{number}")
    }

    fn run_web_path(&self, _number: u64) -> Option<String> {
        None
    }

    /// Shoal reads no Pipelines run, which the capabilities refuse first.
    fn run_item(&self, _repo: &ForgeRepo, _value: &Value) -> Result<Run> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    fn runs_path(&self, _filter: &RunFilter) -> Result<String> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    fn run_jobs_path(&self, _run: &Run, _page: u32) -> Option<String> {
        None
    }

    fn run_jobs(&self, _run: &Run, _offset: usize, _value: &Value) -> Result<Vec<RunJob>> {
        Err(capability::unsupported(self, Capability::ReadRuns))
    }

    async fn run_job_log(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _run: &Run,
        _job: &RunJob,
    ) -> Result<String> {
        Err(capability::unsupported(self, Capability::ReadJobLogs))
    }

    fn run_endpoint(&self, _run: &Run, action: CiAction, _failed: bool) -> Result<String> {
        Err(capability::unsupported(self, Capability::ci(action)))
    }

    fn started_run_url(&self, repo: &ForgeRepo, response: &Value) -> Option<String> {
        let number = response["build_number"].as_u64()?;
        Some(format!(
            "{}/pipelines/results/{number}",
            repo.repository_url()
        ))
    }

    async fn check_uploads(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _attachments: &[Attachment],
    ) -> Result<bool> {
        Err(capability::unsupported(self, Capability::Attachments))
    }

    fn upload(
        &self,
        _repo: &ForgeRepo,
        _holder: Holder,
        _attachment: &Attachment,
        _repository: Option<u64>,
    ) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Attachments))
    }

    fn template_lookups(&self, _kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        Vec::new()
    }
}

async fn view_details(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Details> {
    let value = repo
        .send(account, &Request::get(repo.item_endpoint(kind, number)))
        .await?;
    let current = repo.current(kind, number, &value)?;
    let author = match kind {
        ItemKind::Issue => &value["reporter"],
        ItemKind::Pr => &value["author"],
    };
    Ok(Details {
        number,
        title: current.item.title,
        state: current.item.state.as_str().into(),
        author: nickname(author),
        created_at: text(&value["created_on"]).unwrap_or_default(),
        labels: Vec::new(),
        body: current.body,
        pr: current
            .item
            .pr
            .map(|pr| PrDetails::new(pr.head, pr.base, pr.draft)),
        comments: None,
        reviews: None,
        reactions: Vec::new(),
    })
}

/// Fills the item's comments and returns a pull request's reviews: one
/// per inline thread, and one per approval or change request.
async fn view_discussion(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    details: &mut Details,
) -> Result<Option<Vec<Review>>> {
    let endpoint = format!("{}/comments", repo.item_endpoint(kind, number));
    let (comments, mut reviews) = threads(&repo.pages(account, &endpoint).await?);
    details.comments = Some(comments);
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let verdicts = Bitbucket.verdicts(repo, account, number).await?;
    reviews.extend(verdicts.into_iter().map(|(author, verdict)| {
        Review {
            author,
            state: match verdict {
                Verdict::Approved => "approved",
                Verdict::ChangesRequested => "changes_requested",
                Verdict::Commented => "commented",
            }
            .into(),
            submitted_at: String::new(),
            body: String::new(),
            comments: Vec::new(),
        }
    }));
    Ok(Some(reviews))
}

impl RestReads for Bitbucket {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, Entry)>> {
        let endpoint = format!("{}/comments", repo.item_endpoint(kind, number));
        let comments = repo.pages(account, &endpoint).await?;
        fingerprints(
            &comments,
            &["content", "updated_on", "deleted", "resolution"],
            |comment| nickname(&comment["user"]),
        )
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let pr = repo.item_endpoint(ItemKind::Pr, pull.item.number);
        statuses(&repo.pages(account, &format!("{pr}/statuses")).await?)
    }

    /// Each participant's approval or change request.
    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let pull = repo
            .send(
                account,
                &Request::get(repo.item_endpoint(ItemKind::Pr, number)),
            )
            .await?;
        verdicts(&pull)
    }
}

/// The `[machine.bitbucket]` global config: how to sign in to Bitbucket Cloud.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Credentials {
    /// Argument array whose standard output is an access token, or an
    /// Atlassian API token when `email` is set.
    pub token_command: Vec<String>,
    /// The Atlassian account email an API token belongs to.
    pub email: Option<String>,
}

impl Credentials {
    pub fn validate(&self) -> Result<()> {
        if let Some(program) = self.token_command.first() {
            ensure!(
                !program.is_empty(),
                "machine.bitbucket.token_command must name a program"
            );
        }
        ensure!(
            self.token_command.iter().all(|word| !word.contains('\0')),
            "machine.bitbucket.token_command contains NUL"
        );
        if let Some(email) = &self.email {
            ensure!(
                !self.token_command.is_empty(),
                "machine.bitbucket.email needs machine.bitbucket.token_command"
            );
            ensure!(
                email.contains('@') && email.chars().all(|c| c.is_ascii_graphic()),
                "machine.bitbucket.email must be an email address"
            );
        }
        Ok(())
    }

    /// The curl config line that signs a request in, or none without a
    /// configured token, which reaches only public repositories.
    pub(crate) async fn authorization(&self) -> Result<Option<Vec<u8>>> {
        let Some((program, args)) = self.token_command.split_first() else {
            return Ok(None);
        };
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        let token = shoal_subprocess::Run::new(command)
            .timeout(crate::transport::api::TIMEOUT)
            .output()
            .await
            .context("run machine.bitbucket.token_command")?;
        Ok(Some(authorization(token.trim(), self.email.as_deref())?))
    }
}

/// An access token is a bearer token; an API token signs in with its email.
fn authorization(token: &str, email: Option<&str>) -> Result<Vec<u8>> {
    // The value goes into a curl config line; anything but a plain token is refused.
    ensure!(
        !token.is_empty()
            && token
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\')),
        "machine.bitbucket.token_command printed no token or an unexpected token format"
    );
    let value = match email {
        Some(email) => format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
        None => format!("Bearer {token}"),
    };
    Ok(format!("header = \"Authorization: {value}\"\n").into_bytes())
}

/// Bitbucket's strategy for each merge method; rebasing fast-forwards.
fn merge_request(endpoint: String, method: MergeMethod, delete_branch: bool) -> Request {
    let strategy = match method {
        MergeMethod::Merge => "merge_commit",
        MergeMethod::Squash => "squash",
        MergeMethod::Rebase => "rebase_fast_forward",
    };
    Request::new(
        HttpMethod::Post,
        endpoint,
        Some(json!({ "merge_strategy": strategy, "close_source_branch": delete_branch })),
    )
}

/// The newest status of each build, by its key.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    let mut latest = BTreeMap::new();
    for status in statuses {
        let key = status["key"]
            .as_str()
            .context("Bitbucket status has no key")?;
        let name = status["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(key);
        let updated = status["updated_on"].as_str().unwrap_or_default();
        let state = match status["state"].as_str() {
            Some("INPROGRESS") => "Pending",
            Some("SUCCESSFUL") => "Success",
            Some("FAILED") => "Failure",
            Some("STOPPED") => "Cancelled",
            state => bail!("unknown Bitbucket build state {state:?}"),
        };
        if latest
            .get(key)
            .is_none_or(|(newest, _, _): &(&str, &str, &str)| *newest < updated)
        {
            latest.insert(key, (updated, name, state));
        }
    }
    Ok(latest
        .into_values()
        .map(|(_, name, state)| RestCheck {
            name: name.into(),
            state,
        })
        .collect())
}

/// Participants who approved or requested changes.
fn verdicts(pull: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for participant in pull["participants"]
        .as_array()
        .context("pull request has no participants")?
    {
        let verdict = match participant["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("changes_requested") => Verdict::ChangesRequested,
            None if participant["approved"] == true => Verdict::Approved,
            None => continue,
            Some(state) => bail!("unknown Bitbucket participant state {state}"),
        };
        verdicts.push((nickname(&participant["user"]), verdict));
    }
    Ok(verdicts)
}

/// Comments, and a review for each inline thread with its replies.
fn threads(comments: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let comments = listed(comments);
    let root = roots(&comments);
    let mut general = Vec::new();
    let mut reviews: Vec<(u64, Review)> = Vec::new();
    for comment in &comments {
        let Some(id) = comment["id"].as_u64() else {
            continue;
        };
        let thread = root(id);
        let start = comments.iter().find(|candidate| candidate["id"] == thread);
        if !start.is_some_and(|start| start["inline"].is_object()) {
            general.push(Comment {
                id,
                author: nickname(&comment["user"]),
                created_at: text(&comment["created_on"]).unwrap_or_default(),
                body: text(&comment["content"]["raw"]).unwrap_or_default(),
                reactions: Vec::new(),
            });
            continue;
        }
        // Bitbucket records resolution on the thread's first comment.
        let resolved = start.is_some_and(|start| start["resolution"].is_object());
        let review_comment = review_comment(comment, thread, resolved);
        match reviews.iter_mut().find(|(root, _)| *root == thread) {
            Some((_, review)) => review.comments.push(review_comment),
            None => reviews.push((
                thread,
                Review {
                    author: review_comment.author.clone(),
                    state: "commented".into(),
                    submitted_at: review_comment.created_at.clone(),
                    body: String::new(),
                    comments: vec![review_comment],
                },
            )),
        }
    }
    (
        general,
        reviews.into_iter().map(|(_, review)| review).collect(),
    )
}

/// The thread with `comment`, named by its first comment.
fn find_thread(comments: &[Value], comment: u64) -> Option<Thread> {
    let comments = listed(comments);
    let root = roots(&comments);
    comments.iter().find(|value| value["id"] == comment)?;
    let first = root(comment);
    let start = comments.iter().find(|value| value["id"] == first)?;
    let replies = comments
        .iter()
        .filter_map(|value| value["id"].as_u64())
        .filter(|id| *id != first && root(*id) == first)
        .count();
    Some(Thread {
        first,
        replies,
        resolved: Some(start["resolution"].is_object()),
        target: Target::Bitbucket,
    })
}

fn listed(comments: &[Value]) -> Vec<&Value> {
    comments
        .iter()
        .filter(|comment| comment["deleted"] != true)
        .collect()
}

/// The first comment of the thread of each comment ID.
fn roots(comments: &[&Value]) -> impl Fn(u64) -> u64 {
    let parents: BTreeMap<u64, u64> = comments
        .iter()
        .filter_map(|comment| Some((comment["id"].as_u64()?, comment["parent"]["id"].as_u64()?)))
        .collect();
    move |mut id| {
        while let Some(parent) = parents.get(&id) {
            id = *parent;
        }
        id
    }
}

/// An inline comment on the new side, or the old side of a removed line.
fn review_comment(comment: &Value, thread: u64, resolved: bool) -> ReviewComment {
    let inline = &comment["inline"];
    ReviewComment {
        id: comment["id"].as_u64().unwrap_or_default(),
        thread,
        author: nickname(&comment["user"]),
        created_at: text(&comment["created_on"]).unwrap_or_default(),
        path: text(&inline["path"]).unwrap_or_default(),
        line: inline["to"].as_u64().or(inline["from"].as_u64()),
        body: text(&comment["content"]["raw"]).unwrap_or_default(),
        resolved: Some(resolved),
        reactions: Vec::new(),
    }
}

/// A deleted account has no user.
pub(crate) fn nickname(user: &Value) -> String {
    text(&user["nickname"])
        .or_else(|| text(&user["display_name"]))
        .unwrap_or_else(|| "ghost".into())
}

/// The list endpoint that runs `search` for items of `kind`, newest first,
/// as a Bitbucket query. Bitbucket has no labels.
fn search_endpoint(kind: ItemKind, search: &list::Search) -> String {
    let (resource, text_fields) = match kind {
        ItemKind::Issue => ("issues", ["title", "content.raw"]),
        ItemKind::Pr => ("pullrequests", ["title", "description"]),
    };
    let mut endpoint = format!("{resource}?sort=-created_on");
    let mut clauses = Vec::new();
    match kind {
        ItemKind::Issue => {
            let states: &[&str] = match search.state {
                list::StateFilter::Open => &OPEN_ISSUE_STATES,
                list::StateFilter::Closed => &CLOSED_ISSUE_STATES,
                list::StateFilter::All => &[],
            };
            if !states.is_empty() {
                let states: Vec<String> = states
                    .iter()
                    .map(|state| format!("state={}", literal(state)))
                    .collect();
                clauses.push(format!("({})", states.join(" OR ")));
            }
        }
        // Without a state Bitbucket lists only open pull requests.
        ItemKind::Pr => {
            let states: &[&str] = match search.state {
                list::StateFilter::Open => &["OPEN"],
                list::StateFilter::Closed => &["MERGED", "DECLINED", "SUPERSEDED"],
                list::StateFilter::All => &["OPEN", "MERGED", "DECLINED", "SUPERSEDED"],
            };
            for state in states {
                endpoint += &format!("&state={state}");
            }
        }
    }
    if let Some(text) = &search.text {
        let [first, second] = text_fields.map(|field| format!("{field} ~ {}", literal(text)));
        clauses.push(format!("({first} OR {second})"));
    }
    if let Some(author) = &search.author {
        let field = match kind {
            ItemKind::Issue => "reporter",
            ItemKind::Pr => "author",
        };
        clauses.push(format!("{field}.nickname={}", literal(author)));
    }
    if !clauses.is_empty() {
        endpoint += &format!("&q={}", encode(&clauses.join(" AND "), false));
    }
    endpoint
}

/// `text` as a quoted Bitbucket query string.
fn literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests;
