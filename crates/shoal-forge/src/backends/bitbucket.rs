//! Bitbucket Cloud: its REST shapes, and the credentials Shoal sends to it.
//! Bitbucket has no official CLI to own a login, so requests sign in with
//! the token the account's login for the host fetches.
use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Runners,
    capability::{self, Capability, Support},
    items::{
        action::{Edit, MilestoneChange, changed_set},
        comment::Found,
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
        list,
        reaction::Subject,
    },
    numbers::PullNumber,
    repo::locator::ItemRoute,
    transport::{
        api::{HttpMethod, Request, encode},
        response::Response,
    },
    updates::DISCUSSION,
};

mod activity;
mod attachments;
mod checks;
mod conventions;
mod merging;
mod reactions;
mod reviews;
mod runs;

use super::{Backend, rest};

/// Bitbucket Cloud's API, which is not served from its web host.
const API: &str = "https://api.bitbucket.org/2.0";

/// The one host Bitbucket Cloud serves, whose token alone reaches [`API`].
const HOST: &str = "bitbucket.org";
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
            Capability::RunJobs => Support::Unsupported(None),
            Capability::ReopenPrs => Support::Unsupported(Some(
                "Bitbucket cannot reopen a declined pull request; open a new one",
            )),
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::Labels => Support::Unsupported(Some("Bitbucket has no labels")),
            Capability::PrAssignees => {
                Support::Unsupported(Some("Bitbucket pull requests have no assignees"))
            }
            Capability::PrMilestones => {
                Support::Unsupported(Some("Bitbucket pull requests have no milestones"))
            }
            Capability::Attachments => {
                Support::Unsupported(Some("Bitbucket has no API for attaching files"))
            }
            Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReadJobLogs
            | Capability::ReadRuns
            | Capability::ApproveRuns => Support::Unsupported(None),
            Capability::Downloads => Support::Unsupported(Some(
                "Bitbucket serves images and attached files only to a signed-in browser",
            )),
            Capability::Reactions => Support::Unsupported(Some("Bitbucket has no reactions")),
            Capability::AutoMerge => {
                Support::Unsupported(Some("Bitbucket's API cannot merge a pull request later"))
            }
            Capability::UpdateByMerge | Capability::UpdateByRebase => Support::Unsupported(Some(
                "Bitbucket has no API for updating a PR's branch, so update it with Git and push it",
            )),
            Capability::ResolveThreads
            | Capability::IssueAssignees
            | Capability::IssueMilestones
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

    fn branch_route(&self) -> &'static str {
        "/branch/"
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

    // Bitbucket keeps no ref for a pull request's head.
    fn pull_head_ref(&self, _number: PullNumber) -> Option<String> {
        None
    }

    fn login_hint(&self) -> &'static str {
        "Bitbucket API request failed; private repositories need a token under [machine.forges.\"bitbucket.org\"]"
    }

    /// Both API tokens and access tokens take this static username.
    fn git_username(&self) -> &'static str {
        "x-token-auth"
    }

    /// An API token signs in with its account's email.
    fn uses_account_email(&self) -> bool {
        true
    }

    /// An access token is a bearer token; an API token signs in with its email.
    fn authorization(&self, token: &str, email: Option<&str>) -> String {
        match email {
            Some(email) => format!("Basic {}", BASE64.encode(format!("{email}:{token}"))),
            None => format!("Bearer {token}"),
        }
    }

    async fn send(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        request: &Request,
    ) -> Result<Response> {
        // Another host's token must never reach Bitbucket Cloud's API.
        ensure!(
            repo.host == HOST,
            "{} is not Bitbucket Cloud, which Shoal reaches only at {HOST}",
            repo.host
        );
        let url = format!("{API}/{}", request.endpoint);
        let authorization = account.authorization(repo).await?;
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
                assignees: text(&value["assignee"]["account_id"]).into_iter().collect(),
                milestone: text(&value["milestone"]["name"]),
                pr,
            },
            node_id: None,
            head_repository: text(&value["source"]["repository"]["full_name"]),
            body: text(body).unwrap_or_default(),
            author: text(&value["author"]["account_id"]),
            head_sha: text(&value["source"]["commit"]["hash"]),
            base_sha: text(&value["destination"]["commit"]["hash"]),
            merge_commit: text(&value["merge_commit"]["hash"]),
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
            milestone_id: None,
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
    /// draft state as a flag, replaces reviewers by Atlassian account ID,
    /// gives an issue at most one assignee, by account ID, and takes its
    /// milestone by name.
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
            let reviewers = changed_set(
                &current.reviewers,
                &edit.add_reviewers,
                &edit.remove_reviewers,
            );
            fields["reviewers"] = reviewers
                .into_iter()
                .map(|id| json!({ "account_id": id }))
                .collect();
        }
        if edit.assigns() {
            fields["assignee"] = match edit.assignees_after(&item.assignees).as_slice() {
                [] => Value::Null,
                [id] => json!({ "account_id": id }),
                _ => bail!("a Bitbucket issue has at most one assignee"),
            };
        }
        if let Some(change) = &edit.milestone {
            fields["milestone"] = match change {
                MilestoneChange::Set(name) => json!({ "name": name }),
                MilestoneChange::Clear => Value::Null,
            };
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

    async fn own_login(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<String>> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    /// Nicknames are not unique, so the comment's account is compared with
    /// the login's by UUID.
    async fn wrote(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comment: &Found,
    ) -> Result<bool> {
        let endpoint = comment_endpoint(repo, kind, number, comment.subject)?;
        let comment = repo.send(account, &Request::get(endpoint)).await?;
        let user = repo.send(account, &Request::get("user".into())).await?;
        same_account(&user, &comment["user"])
    }

    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["mainbranch"]["name"].as_str()
    }

    /// Inline comments are among a pull request's comments.
    fn edit_comment(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
        body: &str,
    ) -> Result<Request> {
        Ok(Request::new(
            HttpMethod::Put,
            comment_endpoint(repo, kind, number, subject)?,
            Some(json!({ "content": { "raw": body } })),
        ))
    }
}

impl Runners for Bitbucket {}

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

/// The endpoint of the comment `subject` of item `number`, inline or not.
fn comment_endpoint(
    repo: &ForgeRepo,
    kind: ItemKind,
    number: u64,
    subject: Subject,
) -> Result<String> {
    let (Subject::Comment(id) | Subject::ReviewComment(id)) = subject else {
        bail!("not a comment");
    };
    Ok(format!(
        "{}/comments/{id}",
        repo.item_endpoint(kind, number)
    ))
}

/// Whether `author`, as a comment names its user, is the account `user`
/// describes; a deleted account's comment has no user.
fn same_account(user: &Value, author: &Value) -> Result<bool> {
    let own = text(&user["uuid"]).context("Bitbucket did not report your account's UUID")?;
    Ok(text(&author["uuid"]).is_some_and(|uuid| uuid == own))
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
