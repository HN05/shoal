//! Bitbucket's issues and PRs: reading, listing, opening, editing and
//! commenting on them, and finding a PR by its head.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use shoal_refs::BranchName;

use super::{Bitbucket, literal};
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Items, rest},
    capability::{self, Capability},
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
    transport::api::{HttpMethod, Request, encode},
    updates::DISCUSSION,
};

impl Items for Bitbucket {
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
                    .map(BranchName::from)
                    .context("pull request has no source branch")?,
                base: text(&value["destination"]["branch"]["name"])
                    .map(BranchName::from)
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
            head_sha: text(&value["source"]["commit"]["hash"]).map(Into::into),
            base_sha: text(&value["destination"]["commit"]["hash"]).map(Into::into),
            merge_commit: text(&value["merge_commit"]["hash"]).map(Into::into),
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

    // Bitbucket keeps no ref for a pull request's head.
    fn pull_head_ref(&self, _number: PullNumber) -> Option<String> {
        None
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
            &format!("source.branch.name={}", literal(head.branch.as_str())),
            false,
        );
        let endpoint = format!("pullrequests?state=OPEN&q={query}");
        repo.pages(account, &repo.repo_endpoint(&endpoint)).await
    }

    fn has_head(&self, _repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        pull["source"]["branch"]["name"] == head.branch.as_str()
            && pull["source"]["repository"]["full_name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(head.repository))
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        let endpoint = repo.repo_endpoint(&format!("refs/branches/{}", encode(branch, false)));
        (Request::get(endpoint), "/target/hash")
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

    /// Bitbucket names an issue's assignee by account ID.
    async fn own_assignee(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<String>> {
        let user = repo.send(account, &Request::get("user".into())).await?;
        let id = text(&user["account_id"]).context("Bitbucket did not report your account ID")?;
        Ok(Some(id))
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

/// The issue states Shoal reads as open, and as closed.
const OPEN_ISSUE_STATES: [&str; 3] = ["new", "open", "on hold"];

const CLOSED_ISSUE_STATES: [&str; 5] = ["resolved", "invalid", "duplicate", "wontfix", "closed"];

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

#[cfg(test)]
mod tests;
