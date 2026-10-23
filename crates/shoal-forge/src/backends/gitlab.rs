//! GitLab's REST shapes: issues and merge requests numbered by `iid`,
//! projects addressed by their encoded path, and states of its own.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    backends::rest::{RestCheck, RestReads, fingerprints},
    capability::{Capability, Support},
    ci::{CiAction, CiCheck, Job, JobState, Location, Operation, check, id},
    conventions::Lookup,
    items::{
        action::{Edit, MergeMethod},
        attach::{Attachment, Holder},
        create::{NewIssue, NewPull},
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
        list,
    },
    repo::locator::ItemRoute,
    review::{
        status::{PrStatus, Verdict},
        thread::{Target, Thread},
        view::{Comment, Details, PrDetails, Review, ReviewComment},
    },
    transport::{
        api::{HttpMethod, Request, Upload, encode, full_page},
        programs,
        response::Response,
    },
    updates::{DISCUSSION, Snapshot},
};

pub(crate) mod glab;

use super::{Backend, rest};

#[derive(Clone, Copy)]
pub(crate) struct GitLab;

/// The variables glab takes a token from instead of its saved logins.
const ENV_TOKENS: &[&str] = &[
    "GITLAB_TOKEN",
    "GITLAB_ACCESS_TOKEN",
    "OAUTH_TOKEN",
    "CI_JOB_TOKEN",
];

impl Backend for GitLab {
    /// GitLab reports its version with the revision it was built from. Only
    /// glab's login for the host is sent, never a token it would read from
    /// the environment for any host.
    async fn serves(&self, host: &ForgeRepo, account: &Account) -> Result<bool> {
        let request = Request::get("version".into()).withholding(ENV_TOKENS);
        let version = host.send(account, &request).await?;
        Ok(version["version"].is_string() && version["revision"].is_string())
    }

    fn name(&self) -> &'static str {
        "GitLab"
    }

    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::ReadJobLogs => Support::Unsupported(None),
            Capability::RebaseMerge => Support::Unsupported(Some(
                "GitLab merges with the project's merge method; use merge or squash",
            )),
            Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::MergeCommit
            | Capability::SquashMerge => Support::Supported,
        }
    }

    fn nests_groups(&self) -> bool {
        true
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::GitLabIssue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::MergeRequest
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("projects/{}/{path}", encode(&repo.path, false))
    }

    fn pull_resource(&self) -> &'static str {
        "merge_requests"
    }

    fn number_field(&self) -> &'static str {
        "iid"
    }

    fn commit_field(&self) -> &'static str {
        "id"
    }

    fn login_hint(&self) -> &'static str {
        "GitLab API request failed; check `glab auth login` for the host"
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        repo.passthrough(programs::GLAB, account, request).await
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }

    /// An issue or merge request as GitLab reports it.
    fn current(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        let state = match value["state"].as_str() {
            // A merge request is locked while GitLab merges it.
            Some("opened" | "locked") => ItemState::Open,
            Some("closed") => ItemState::Closed,
            Some("merged") if kind == ItemKind::Pr => ItemState::Merged,
            state => bail!("unrecognized GitLab state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: text(&value["source_branch"])
                    .context("merge request has no source branch")?,
                base: text(&value["target_branch"])
                    .context("merge request has no target branch")?,
                draft: value["draft"]
                    .as_bool()
                    .or(value["work_in_progress"].as_bool())
                    .unwrap_or_default(),
            }),
        };
        let merge_status = value["detailed_merge_status"].as_str().unwrap_or_default();
        let conflict = match (kind, value["has_conflicts"].as_bool()) {
            (ItemKind::Pr, Some(conflicts)) if !UNDECIDED.contains(&merge_status) => {
                Some(conflicts)
            }
            _ => None,
        };
        let same_project = value["source_project_id"] == value["target_project_id"];
        let reviewers: Vec<(String, u64)> = value["reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|user| Some((text(&user["username"])?, user["id"].as_u64()?)))
            .collect();
        Ok(Current {
            item: Item {
                kind,
                number,
                url: repo.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: labels(&value["labels"]),
                pr,
            },
            node_id: None,
            head_repository: (kind == ItemKind::Pr && same_project).then(|| repo.path.clone()),
            body: text(&value["description"]).unwrap_or_default(),
            head_sha: text(&value["sha"]),
            base_sha: None,
            conflict,
            reviewers: reviewers.iter().map(|(name, _)| name.clone()).collect(),
            user_ids: reviewers.into_iter().collect(),
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

    /// Look up the IDs of the users the edit adds that `current` has none for.
    async fn prepare_edit(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        edit: &Edit,
        current: &mut Current,
    ) -> Result<()> {
        for name in &edit.add_reviewers {
            if current.user_ids.contains_key(name) {
                continue;
            }
            let request = Request::get(format!("users?username={}", encode(name, false)));
            let users = repo.send(account, &request).await?;
            let id = users
                .as_array()
                .into_iter()
                .flatten()
                .find(|user| {
                    user["username"]
                        .as_str()
                        .is_some_and(|username| username.eq_ignore_ascii_case(name))
                })
                .and_then(|user| user["id"].as_u64())
                .with_context(|| format!("GitLab has no user {name}"))?;
            current.user_ids.insert(name.clone(), id);
        }
        Ok(())
    }

    /// One update with every changed field; GitLab adds and removes labels by
    /// name, keeps draft state in the title and replaces reviewers by ID.
    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let item = &current.item;
        let mut fields = serde_json::Map::new();
        let mut title = edit.title.clone();
        if let Some(pr) = &item.pr {
            let draft = edit.draft.unwrap_or(pr.draft);
            if title.is_some() || draft != pr.draft {
                let current = title.as_deref().unwrap_or(&item.title);
                title = Some(draft_title(current, draft));
            }
        }
        if let Some(title) = title {
            fields.insert("title".into(), title.into());
        }
        if let Some(body) = &edit.body {
            fields.insert("description".into(), body.clone().into());
        }
        if let Some(base) = &edit.base {
            fields.insert("target_branch".into(), base.clone().into());
        }
        for (field, labels) in [
            ("add_labels", &edit.add_labels),
            ("remove_labels", &edit.remove_labels),
        ] {
            if !labels.is_empty() {
                fields.insert(field.into(), labels.join(",").into());
            }
        }
        if !edit.add_reviewers.is_empty() || !edit.remove_reviewers.is_empty() {
            fields.insert("reviewer_ids".into(), reviewer_ids(current, edit)?.into());
        }
        if fields.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![Request::new(
            HttpMethod::Put,
            repo.item_endpoint(item.kind, item.number),
            Some(fields.into()),
        )])
    }

    fn comment(&self, repo: &ForgeRepo, item: &Item, body: &str) -> Request {
        let endpoint = format!("{}/notes", repo.item_endpoint(item.kind, item.number));
        Request::new(HttpMethod::Post, endpoint, Some(json!({ "body": body })))
    }

    fn set_state(&self, repo: &ForgeRepo, item: &Item, state: ItemState) -> Result<Request> {
        let event = if state == ItemState::Open {
            "reopen"
        } else {
            "close"
        };
        Ok(Request::new(
            HttpMethod::Put,
            repo.item_endpoint(item.kind, item.number),
            Some(json!({ "state_event": event })),
        ))
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
        )?])
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
        branch: &str,
    ) -> Result<Vec<Value>> {
        let branch = encode(branch, false);
        let endpoint = format!("merge_requests?state=opened&source_branch={branch}");
        repo.pages(account, &repo.repo_endpoint(&endpoint)).await
    }

    fn has_head(&self, _repo: &ForgeRepo, pull: &Value, branch: &str) -> bool {
        pull["source_branch"] == branch && pull["source_project_id"] == pull["target_project_id"]
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let title = if pull.draft {
            draft_title(&pull.title, true)
        } else {
            pull.title.clone()
        };
        let body = json!({"title": title, "description": pull.body,
            "source_branch": pull.head, "target_branch": pull.base});
        Request::new(
            HttpMethod::Post,
            repo.repo_endpoint("merge_requests"),
            Some(body),
        )
    }

    fn new_issue(&self, repo: &ForgeRepo, issue: &NewIssue) -> Request {
        let body = json!({ "title": issue.title, "description": issue.body,
            "labels": issue.labels.join(",") });
        Request::new(HttpMethod::Post, repo.repo_endpoint("issues"), Some(body))
    }

    fn labels_issue_after_opening(&self) -> bool {
        false
    }

    fn has_labels(&self) -> bool {
        true
    }

    async fn thread(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
        comment: u64,
    ) -> Result<Option<Thread>> {
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        let discussions = repo.pages(account, &format!("{pr}/discussions")).await?;
        Ok(find_thread(&discussions, comment))
    }

    fn reply(&self, repo: &ForgeRepo, number: u64, thread: &Thread, body: &str) -> Result<Request> {
        let Target::GitLab { discussion } = &thread.target else {
            bail!("not a GitLab review thread");
        };
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        Ok(Request::new(
            HttpMethod::Post,
            format!("{pr}/discussions/{discussion}/notes"),
            Some(json!({ "body": body })),
        ))
    }

    fn resolution(
        &self,
        repo: &ForgeRepo,
        number: u64,
        thread: &Thread,
        resolved: bool,
    ) -> Result<Request> {
        let Target::GitLab { discussion } = &thread.target else {
            bail!("not a GitLab review thread");
        };
        let pr = repo.item_endpoint(ItemKind::Pr, number);
        Ok(Request::new(
            HttpMethod::Put,
            format!("{pr}/discussions/{discussion}"),
            Some(json!({ "resolved": resolved })),
        ))
    }

    /// GitLab lists blocking issues among an issue's links.
    async fn open_issue_dependencies(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<String>> {
        let links = repo
            .pages(
                account,
                &repo.repo_endpoint(&format!("issues/{number}/links")),
            )
            .await
            .context(self.login_hint())?;
        open_blocking(&links)
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
            .context(self.login_hint())
    }

    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
    ) -> Result<Option<Vec<Review>>> {
        view_discussion(repo, account, kind, number, details)
            .await
            .context(self.login_hint())
    }

    async fn ci_checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        head: &str,
    ) -> Result<Vec<CiCheck>> {
        let endpoint = repo.repo_endpoint(&format!("repository/commits/{head}/statuses"));
        latest_checks(&repo.pages(account, &endpoint).await?)
    }

    fn locate_job(&self, repo: &ForgeRepo, target: &str) -> Option<Location> {
        let parts = repo.repository_path(target, "/-/jobs/")?;
        let [job] = parts.as_slice() else {
            return None;
        };
        Some(Location::GitLab { job: id(job)? })
    }

    fn ci_operation(
        &self,
        _jobs: &[Job],
        _selected: &[Job],
        job: &Job,
        action: CiAction,
    ) -> Result<(Operation, Vec<String>)> {
        let Some(Location::GitLab { job: id }) = job.location else {
            bail!("check {} is not a GitLab job", job.name);
        };
        let endpoint = match action {
            CiAction::Rerun => format!("jobs/{id}/retry"),
            CiAction::Cancel => format!("jobs/{id}/cancel"),
        };
        Ok((Operation::Post(endpoint), vec![job.name.clone()]))
    }

    async fn check_uploads(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _attachments: &[Attachment],
    ) -> Result<bool> {
        Ok(false)
    }

    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        _repository: Option<u64>,
    ) -> Result<Request> {
        ensure!(
            holder == Holder::Repository,
            "GitLab cannot attach files to {holder:?}"
        );
        Ok(Request::upload(
            repo.repo_endpoint("uploads"),
            Upload {
                path: attachment.path.clone(),
                content_type: attachment.content_type()?,
                field: Some("file"),
            },
        ))
    }

    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        match kind {
            ItemKind::Pr => vec![(
                ".gitlab/merge_request_templates",
                Lookup::File("default.md"),
            )],
            ItemKind::Issue => vec![(".gitlab", Lookup::Directory("issue_templates"))],
        }
    }

    /// GitLab's scoped labels are named `key::value`.
    fn exclusive_label(&self, name: &str, _label: &Value) -> bool {
        name.contains("::")
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
    let pr = match current.item.pr {
        Some(pr) => Some(PrDetails::new(pr.head, pr.base, pr.draft)),
        None => None,
    };
    Ok(Details {
        number,
        title: current.item.title,
        state: current.item.state.as_str().into(),
        author: username(&value["author"]),
        created_at: text(&value["created_at"]).unwrap_or_default(),
        labels: current.item.labels,
        body: current.body,
        pr,
        comments: None,
        reviews: None,
    })
}

/// Fills the item's comments and returns a merge request's reviews: one
/// per diff thread, and one per approval or change request.
async fn view_discussion(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
    details: &mut Details,
) -> Result<Option<Vec<Review>>> {
    let endpoint = format!("{}/discussions", repo.item_endpoint(kind, number));
    let (comments, mut reviews) = discussions(&repo.pages(account, &endpoint).await?);
    details.comments = Some(comments);
    if kind == ItemKind::Issue {
        return Ok(None);
    }
    let verdicts = GitLab.verdicts(repo, account, number).await?;
    reviews.extend(verdicts.into_iter().filter_map(|(author, verdict)| {
        let state = match verdict {
            Verdict::Approved => "approved",
            Verdict::ChangesRequested => "changes_requested",
            Verdict::Commented => return None,
        };
        Some(Review {
            author,
            state: state.into(),
            submitted_at: String::new(),
            body: String::new(),
            comments: Vec::new(),
        })
    }));
    Ok(Some(reviews))
}

impl RestReads for GitLab {
    async fn discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<Vec<(String, String)>> {
        let notes = item_notes(repo, account, kind, number).await?;
        fingerprints(&notes, &["body", "updated_at", "resolved"])
    }

    async fn checks(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        pull: &Current,
    ) -> Result<Vec<RestCheck>> {
        let head = pull.head_sha.as_deref().context("PR has no head commit")?;
        let endpoint = repo.repo_endpoint(&format!("repository/commits/{head}/statuses"));
        statuses(&repo.pages(account, &endpoint).await?)
    }

    /// Reviewers' states, with every approval as an approving review.
    async fn verdicts(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<Vec<(String, Verdict)>> {
        let merge_request = repo.item_endpoint(ItemKind::Pr, number);
        let reviewers = Request::get(format!("{merge_request}/reviewers"));
        let approvals = Request::get(format!("{merge_request}/approvals"));
        let (reviewers, approvals) = tokio::try_join!(
            repo.send(account, &reviewers),
            repo.send(account, &approvals)
        )?;
        verdicts(&reviewers, &approvals)
    }
}

/// The item's notes that people wrote, without GitLab's system notes.
async fn item_notes(
    repo: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    number: u64,
) -> Result<Vec<Value>> {
    let endpoint = format!("{}/notes", repo.item_endpoint(kind, number));
    let mut notes = repo.pages(account, &endpoint).await?;
    notes.retain(|note| note["system"] != true);
    Ok(notes)
}

/// Merge statuses during which GitLab has not yet decided mergeability.
const UNDECIDED: [&str; 4] = ["unchecked", "checking", "preparing", "approvals_syncing"];

/// Title prefixes that make a merge request a draft, including the former `WIP:`.
const DRAFT_PREFIXES: [&str; 5] = ["Draft:", "[Draft]", "(Draft)", "Draft -", "WIP:"];

/// GitLab merges with the project's merge method, optionally squashing.
fn merge_request(endpoint: String, method: MergeMethod, delete_branch: bool) -> Result<Request> {
    ensure!(
        method != MergeMethod::Rebase,
        "GitLab merges with the project's merge method; use merge or squash"
    );
    Ok(Request::new(
        HttpMethod::Put,
        endpoint,
        Some(json!({
            "squash": method == MergeMethod::Squash,
            "should_remove_source_branch": delete_branch,
        })),
    ))
}

/// The title with GitLab's draft prefix added or removed.
pub(crate) fn draft_title(title: &str, draft: bool) -> String {
    let bare = DRAFT_PREFIXES
        .iter()
        .find_map(|prefix| {
            title
                .get(..prefix.len())
                .filter(|start| start.eq_ignore_ascii_case(prefix))
                .map(|_| title[prefix.len()..].trim_start())
        })
        .unwrap_or(title);
    if draft {
        format!("Draft: {bare}")
    } else {
        bare.to_owned()
    }
}

/// The reviewers after the edit, by user ID.
fn reviewer_ids(current: &Current, edit: &Edit) -> Result<Vec<u64>> {
    let mut names: Vec<&String> = current
        .reviewers
        .iter()
        .filter(|name| !edit.remove_reviewers.contains(name))
        .collect();
    for name in &edit.add_reviewers {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            current
                .user_ids
                .get(name)
                .copied()
                .with_context(|| format!("GitLab user {name} was not looked up"))
        })
        .collect()
}

/// The newest status of each name; retried jobs keep their earlier ones.
fn statuses(statuses: &[Value]) -> Result<Vec<RestCheck>> {
    let mut latest = BTreeMap::new();
    for status in statuses {
        let name = status["name"]
            .as_str()
            .context("GitLab status has no name")?;
        let id = status["id"].as_u64().context("GitLab status has no ID")?;
        let state = match status["status"].as_str() {
            Some(
                "created"
                | "waiting_for_resource"
                | "preparing"
                | "pending"
                | "running"
                | "scheduled"
                | "canceling"
                | "waiting_for_callback",
            ) => "Pending",
            Some("success") => "Success",
            Some("failed") if status["allow_failure"] == true => "Warning",
            Some("failed") => "Failure",
            Some("canceled") => "Cancelled",
            // A manual job waits for someone to start it, which CI cannot.
            Some("skipped" | "manual") => "Skipped",
            state => bail!("unknown GitLab CI state {state:?}"),
        };
        if latest.get(name).is_none_or(|(newest, _)| *newest < id) {
            latest.insert(name.to_owned(), (id, state));
        }
    }
    Ok(latest
        .into_iter()
        .map(|(name, (_, state))| RestCheck { name, state })
        .collect())
}

/// Reviewers' latest states, then approvals, which outrank them.
fn verdicts(reviewers: &Value, approvals: &Value) -> Result<Vec<(String, Verdict)>> {
    let mut verdicts = Vec::new();
    for reviewer in reviewers.as_array().context("reviewers are not a list")? {
        let verdict = match reviewer["state"].as_str() {
            Some("approved") => Verdict::Approved,
            Some("requested_changes") => Verdict::ChangesRequested,
            Some("reviewed") => Verdict::Commented,
            Some("unreviewed" | "unapproved" | "review_started") => continue,
            state => bail!("unknown GitLab reviewer state {state:?}"),
        };
        verdicts.push((username(&reviewer["user"]), verdict));
    }
    for approval in approvals["approved_by"]
        .as_array()
        .context("approvals have no approvers")?
    {
        verdicts.push((username(&approval["user"]), Verdict::Approved));
    }
    Ok(verdicts)
}

/// Comments, and a review for each thread on the diff.
fn discussions(discussions: &[Value]) -> (Vec<Comment>, Vec<Review>) {
    let mut comments = Vec::new();
    let mut reviews = Vec::new();
    for discussion in discussions {
        let notes = notes(discussion);
        let Some(first) = notes.first() else { continue };
        if first["type"] == "DiffNote" {
            reviews.push(Review {
                author: username(&first["author"]),
                state: "commented".into(),
                submitted_at: text(&first["created_at"]).unwrap_or_default(),
                body: String::new(),
                comments: notes
                    .iter()
                    .map(|note| review_comment(note, first["id"].as_u64().unwrap_or_default()))
                    .collect(),
            });
        } else {
            comments.extend(notes.iter().map(|note| Comment {
                author: username(&note["author"]),
                created_at: text(&note["created_at"]).unwrap_or_default(),
                body: text(&note["body"]).unwrap_or_default(),
            }));
        }
    }
    (comments, reviews)
}

/// The discussion with note `comment`, which replies and resolution name.
fn find_thread(discussions: &[Value], comment: u64) -> Option<Thread> {
    discussions.iter().find_map(|discussion| {
        let notes = notes(discussion);
        notes.iter().find(|note| note["id"] == comment)?;
        let first = notes.first()?;
        Some(Thread {
            first: first["id"].as_u64()?,
            replies: notes.len() - 1,
            resolved: first["resolved"].as_bool(),
            target: Target::GitLab {
                discussion: text(&discussion["id"])?,
            },
        })
    })
}

/// A discussion's notes without the ones GitLab writes for events.
fn notes(discussion: &Value) -> Vec<&Value> {
    discussion["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|note| note["system"] != true)
        .collect()
}

/// A diff note on the new side, or the old side of a removed line.
fn review_comment(note: &Value, thread: u64) -> ReviewComment {
    let position = &note["position"];
    ReviewComment {
        id: note["id"].as_u64().unwrap_or_default(),
        thread,
        author: username(&note["author"]),
        created_at: text(&note["created_at"]).unwrap_or_default(),
        path: text(&position["new_path"])
            .or_else(|| text(&position["old_path"]))
            .unwrap_or_default(),
        line: position["new_line"]
            .as_u64()
            .or(position["old_line"].as_u64()),
        body: text(&note["body"]).unwrap_or_default(),
        resolved: note["resolved"].as_bool(),
    }
}

/// A deleted account has no user.
fn username(user: &Value) -> String {
    user["username"].as_str().unwrap_or("ghost").to_owned()
}

/// Label names, listed as strings unless details were requested.
fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| text(label).or_else(|| text(&label["name"])))
        .collect()
}

/// GitLab's newest status of each name; a retried job keeps its earlier ones.
fn latest_checks(statuses: &[Value]) -> Result<Vec<CiCheck>> {
    let mut latest = std::collections::BTreeMap::new();
    for status in statuses {
        let id = status["id"].as_u64().context("GitLab status has no ID")?;
        let state = match status["status"].as_str() {
            Some(
                "created"
                | "waiting_for_resource"
                | "preparing"
                | "pending"
                | "running"
                | "scheduled"
                | "waiting_for_callback",
            ) => JobState::Running,
            Some("failed" | "canceled") => JobState::Failed,
            Some("success" | "skipped" | "manual" | "canceling") => JobState::Finished,
            state => bail!("unknown GitLab CI state {state:?}"),
        };
        let check = check(status, "name", state, "target_url")?;
        if latest
            .get(&check.0)
            .is_none_or(|(newest, _): &(u64, CiCheck)| *newest < id)
        {
            latest.insert(check.0.clone(), (id, check));
        }
    }
    Ok(latest.into_values().map(|(_, check)| check).collect())
}

/// The list endpoint that runs `search` for items of `kind`. GitLab keeps
/// merged merge requests apart from closed ones, so a search for closed ones
/// lists all and leaves the open ones for the caller to drop.
fn search_endpoint(kind: ItemKind, search: &list::Search) -> String {
    let state = match (search.state, kind) {
        (list::StateFilter::Open, _) => "opened",
        (list::StateFilter::Closed, ItemKind::Issue) => "closed",
        (list::StateFilter::Closed, ItemKind::Pr) | (list::StateFilter::All, _) => "all",
    };
    let resource = match kind {
        ItemKind::Issue => "issues",
        ItemKind::Pr => "merge_requests",
    };
    let mut endpoint = format!("{resource}?state={state}");
    if !search.labels.is_empty() {
        endpoint += &format!("&labels={}", encode(&search.labels.join(","), false));
    }
    if let Some(author) = &search.author {
        endpoint += &format!("&author_username={}", encode(author, false));
    }
    if let Some(text) = &search.text {
        endpoint += &format!("&search={}", encode(text, false));
    }
    endpoint
}

/// The URLs of the open issues among `links` that block the linked issue.
fn open_blocking(links: &[Value]) -> Result<Vec<String>> {
    links
        .iter()
        .filter(|link| link["link_type"] == "is_blocked_by" && link["state"] == "opened")
        .map(|link| {
            link["web_url"]
                .as_str()
                .map(str::to_owned)
                .context("linked issue has no URL")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_blocking_issues_are_the_open_is_blocked_by_links() {
        let links = [
            json!({"link_type": "is_blocked_by", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/1"}),
            json!({"link_type": "is_blocked_by", "state": "closed", "web_url": "https://gitlab.com/g/p/-/issues/2"}),
            json!({"link_type": "blocks", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/3"}),
            json!({"link_type": "relates_to", "state": "opened", "web_url": "https://gitlab.com/g/p/-/issues/4"}),
        ];
        assert_eq!(
            open_blocking(&links).unwrap(),
            ["https://gitlab.com/g/p/-/issues/1"]
        );
    }

    #[test]
    fn closed_merge_request_searches_list_all_to_include_merged_ones() {
        let mut search = list::Search {
            state: list::StateFilter::Closed,
            labels: vec!["bug".into(), "ui".into()],
            author: Some("dev".into()),
            text: Some("crash on start".into()),
            limit: 30,
        };
        assert_eq!(
            search_endpoint(ItemKind::Pr, &search),
            "merge_requests?state=all&labels=bug%2Cui&author_username=dev&search=crash%20on%20start"
        );
        search.labels.clear();
        search.author = None;
        search.text = None;
        assert_eq!(
            search_endpoint(ItemKind::Issue, &search),
            "issues?state=closed"
        );
        search.state = list::StateFilter::Open;
        assert_eq!(
            search_endpoint(ItemKind::Pr, &search),
            "merge_requests?state=opened"
        );
    }
    use serde_json::json;

    fn gitlab() -> ForgeRepo {
        ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap()
    }

    #[test]
    fn merge_requests_read_branches_drafts_and_conflicts() {
        let merge_request = json!({"iid": 7, "title": "Draft: Fix", "state": "opened",
            "description": "Why", "labels": ["bug"], "source_branch": "topic",
            "target_branch": "main", "draft": true, "sha": "abc", "has_conflicts": true,
            "detailed_merge_status": "conflict", "source_project_id": 1, "target_project_id": 1});
        let current = gitlab().current(ItemKind::Pr, 7, &merge_request).unwrap();
        assert_eq!(
            current.item,
            Item {
                kind: ItemKind::Pr,
                number: 7,
                url: "https://gitlab.com/group/sub/project/-/merge_requests/7".into(),
                title: "Draft: Fix".into(),
                state: ItemState::Open,
                labels: vec!["bug".into()],
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: true,
                }),
            }
        );
        assert_eq!(current.conflict, Some(true));
        assert_eq!(current.head_sha.as_deref(), Some("abc"));
        assert_eq!(
            current.head_repository.as_deref(),
            Some("group/sub/project")
        );
        assert_eq!(current.body, "Why");

        let mut checking = merge_request.clone();
        checking["detailed_merge_status"] = json!("checking");
        checking["source_project_id"] = json!(2);
        let current = gitlab().current(ItemKind::Pr, 7, &checking).unwrap();
        assert_eq!(current.conflict, None);
        assert_eq!(current.head_repository, None);

        let mut merged = merge_request;
        merged["state"] = json!("merged");
        let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
        assert_eq!(current.item.state, ItemState::Merged);
        assert!(gitlab().current(ItemKind::Pr, 8, &merged).is_err());
    }

    #[test]
    fn statuses_keep_the_newest_of_each_name_in_shared_states() {
        let listed = json!([
            {"id": 1, "name": "rust", "status": "failed"},
            {"id": 3, "name": "rust", "status": "running"},
            {"id": 2, "name": "lint", "status": "failed", "allow_failure": true},
            {"id": 4, "name": "deploy", "status": "manual"},
            {"id": 5, "name": "external", "status": "canceled"}
        ]);
        let checks = statuses(listed.as_array().unwrap()).unwrap();
        assert_eq!(
            checks
                .iter()
                .map(|check| (check.name.as_str(), check.state))
                .collect::<Vec<_>>(),
            [
                ("deploy", "Skipped"),
                ("external", "Cancelled"),
                ("lint", "Warning"),
                ("rust", "Pending")
            ]
        );
        assert!(statuses(&[json!({"id": 1, "name": "x", "status": "later"})]).is_err());
    }

    #[test]
    fn approvals_and_reviewer_states_become_verdicts() {
        let reviewers = json!([
            {"user": {"username": "ann"}, "state": "requested_changes"},
            {"user": {"username": "bob"}, "state": "reviewed"},
            {"user": {"username": "cat"}, "state": "unreviewed"}
        ]);
        let approvals = json!({"approved_by": [{"user": {"username": "dan"}}]});
        assert_eq!(
            verdicts(&reviewers, &approvals).unwrap(),
            [
                ("ann".to_owned(), Verdict::ChangesRequested),
                ("bob".to_owned(), Verdict::Commented),
                ("dan".to_owned(), Verdict::Approved)
            ]
        );
        assert!(verdicts(&json!([{"state": "later"}]), &approvals).is_err());
        assert!(verdicts(&json!([]), &json!({})).is_err());
    }

    #[test]
    fn diff_threads_become_reviews_and_other_notes_comments() {
        let listed = json!([
            {"notes": [{"system": true, "body": "added 1 commit"}]},
            {"notes": [{"author": {"username": "ann"}, "body": "Looks fine", "created_at": "t1"}]},
            {"notes": [
                {"id": 30, "type": "DiffNote", "author": {"username": "bob"}, "body": "Bug", "created_at": "t2",
                    "resolved": false, "position": {"new_path": "a.rs", "new_line": 3}},
                {"id": 31, "type": "DiffNote", "author": null, "body": "Fixed", "created_at": "t3",
                    "resolved": true, "position": {"old_path": "b.rs", "new_line": null, "old_line": 9}}
            ]}
        ]);
        let (comments, reviews) = discussions(listed.as_array().unwrap());
        assert_eq!(comments.len(), 1);
        assert_eq!(
            (comments[0].author.as_str(), comments[0].body.as_str()),
            ("ann", "Looks fine")
        );
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            (reviews[0].author.as_str(), reviews[0].state.as_str()),
            ("bob", "commented")
        );
        let threads = &reviews[0].comments;
        assert_eq!(
            (
                threads[0].path.as_str(),
                threads[0].line,
                threads[0].resolved
            ),
            ("a.rs", Some(3), Some(false))
        );
        assert_eq!(
            (
                threads[1].author.as_str(),
                threads[1].path.as_str(),
                threads[1].line
            ),
            ("ghost", "b.rs", Some(9))
        );
        assert_eq!(
            threads.iter().map(|c| (c.id, c.thread)).collect::<Vec<_>>(),
            [(30, 30), (31, 30)]
        );
    }

    #[test]
    fn notes_find_their_discussion() {
        let listed = json!([
            {"id": "d1", "notes": [{"id": 1, "system": true}, {"id": 2, "resolved": false}, {"id": 3}]},
            {"id": "d2", "notes": [{"id": 4, "resolved": true}]}
        ]);
        let listed = listed.as_array().unwrap();
        let thread = find_thread(listed, 3).unwrap();
        assert_eq!(
            (thread.first, thread.replies, thread.resolved),
            (2, 1, Some(false))
        );
        assert_eq!(
            thread.target,
            Target::GitLab {
                discussion: "d1".into()
            }
        );
        assert_eq!(find_thread(listed, 4).unwrap().resolved, Some(true));
        // System notes are events, not comments.
        assert!(find_thread(listed, 1).is_none());
    }

    #[test]
    fn issues_use_their_own_numbers_routes_and_states() {
        let issue = json!({"iid": 3, "id": 900, "title": "Bug", "state": "closed",
            "labels": [{"name": "type/bug"}]});
        let current = gitlab().current(ItemKind::Issue, 3, &issue).unwrap();
        assert_eq!(
            current.item.url,
            "https://gitlab.com/group/sub/project/-/issues/3"
        );
        assert_eq!(current.item.state, ItemState::Closed);
        assert_eq!(current.item.labels, ["type/bug"]);
        assert!(gitlab().current(ItemKind::Issue, 900, &issue).is_err());
        let mut merged = issue;
        merged["state"] = json!("merged");
        assert!(gitlab().current(ItemKind::Issue, 3, &merged).is_err());
    }

    #[test]
    fn retried_jobs_report_their_newest_attempt() {
        // A retried job's newest attempt decides; its failed first one is history.
        let gitlab = [
            json!({"id": 7, "name": "rust", "status": "failed", "target_url": "old"}),
            json!({"id": 9, "name": "rust", "status": "running", "target_url": "new"}),
            json!({"id": 8, "name": "docs", "status": "canceled"}),
            json!({"id": 10, "name": "deploy", "status": "waiting_for_callback"}),
        ];
        assert_eq!(
            latest_checks(&gitlab).unwrap(),
            [
                ("deploy".into(), JobState::Running, String::new()),
                ("docs".into(), JobState::Failed, String::new()),
                ("rust".into(), JobState::Running, "new".into())
            ]
        );
        let unknown = [json!({"id": 1, "name": "x", "status": "later"})];
        assert!(latest_checks(&unknown).is_err());
    }
}
