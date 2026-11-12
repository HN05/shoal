//! GitLab's issues and PRs: reading, listing, opening, editing and commenting
//! on them, and finding a PR by its head.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::GitLab;
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Items, LoginHint, rest},
    conventions::draft::DraftMarker,
    items::{
        action::{Edit, MilestoneChange, changed_set},
        create::{Head, NewIssue, NewPull},
        item::{Current, Item, ItemState, PrSummary, text},
        link::ItemKind,
        list,
        reaction::Subject,
    },
    numbers::{IssueNumber, PullNumber},
    transport::api::{HttpMethod, Request, encode},
    updates::DISCUSSION,
};

impl Items for GitLab {
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
        let users = |field: &str| -> Vec<(String, u64)> {
            value[field]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|user| Some((text(&user["username"])?, user["id"].as_u64()?)))
                .collect()
        };
        let (reviewers, assignees) = (users("reviewers"), users("assignees"));
        Ok(Current {
            item: Item {
                kind,
                number,
                url: repo.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: labels(&value["labels"]),
                assignees: assignees.iter().map(|(name, _)| name.clone()).collect(),
                milestone: text(&value["milestone"]["title"]),
                pr,
            },
            node_id: None,
            head_repository: (kind == ItemKind::Pr && same_project).then(|| repo.path.clone()),
            body: text(&value["description"]).unwrap_or_default(),
            author: text(&value["author"]["username"]),
            head_sha: text(&value["sha"]),
            base_sha: None,
            // A fast-forward merge leaves the source branch's head.
            merge_commit: (state == ItemState::Merged)
                .then(|| {
                    text(&value["merge_commit_sha"])
                        .or_else(|| text(&value["squash_commit_sha"]))
                        .or_else(|| text(&value["sha"]))
                })
                .flatten(),
            conflict,
            merges_now: false,
            reviewers: reviewers.iter().map(|(name, _)| name.clone()).collect(),
            user_ids: reviewers.into_iter().chain(assignees).collect(),
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

    /// GitLab lists blocking issues among an issue's links.
    async fn open_issue_blockers(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Vec<String>> {
        let links = repo
            .pages(
                account,
                &repo.repo_endpoint(&format!("issues/{number}/links")),
            )
            .await
            .hint_login(self)?;
        open_blocking(&links)
    }

    fn pull_head_ref(&self, number: PullNumber) -> Option<String> {
        Some(format!("refs/merge-requests/{number}/head"))
    }

    /// Look up the IDs of the users the edit adds that `current` has none for.
    async fn prepare_edit(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        edit: &Edit,
        current: &mut Current,
    ) -> Result<()> {
        for name in edit.add_reviewers.iter().chain(&edit.add_assignees) {
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
    /// name, keeps draft state in the title, replaces reviewers and assignees
    /// by user ID and clears a milestone with ID 0.
    fn edit(&self, repo: &ForgeRepo, current: &Current, edit: &Edit) -> Result<Vec<Request>> {
        let item = &current.item;
        let mut fields = serde_json::Map::new();
        let mut title = edit.title.clone();
        if let Some(pr) = &item.pr {
            let draft = edit.draft.unwrap_or(pr.draft);
            if title.is_some() || draft != pr.draft {
                let current = title.as_deref().unwrap_or(&item.title);
                title = Some(DRAFT.draft_title(current, draft));
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
            let reviewers = changed_set(
                &current.reviewers,
                &edit.add_reviewers,
                &edit.remove_reviewers,
            );
            fields.insert("reviewer_ids".into(), user_ids(current, &reviewers)?.into());
        }
        if edit.assigns() {
            let assignees = edit.assignees_after(&current.item.assignees);
            fields.insert("assignee_ids".into(), user_ids(current, &assignees)?.into());
        }
        match &edit.milestone {
            Some(MilestoneChange::Set(title)) => {
                let id = current
                    .milestone_id
                    .with_context(|| format!("milestone {title} was not looked up"))?;
                fields.insert("milestone_id".into(), id.into());
            }
            Some(MilestoneChange::Clear) => {
                fields.insert("milestone_id".into(), 0.into());
            }
            None => {}
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
        let branch = encode(head.branch, false);
        let endpoint = format!("merge_requests?state=opened&source_branch={branch}");
        repo.pages(account, &repo.repo_endpoint(&endpoint)).await
    }

    /// Only merge requests within the project: a fork is another project.
    fn has_head(&self, repo: &ForgeRepo, pull: &Value, head: &Head<'_>) -> bool {
        head.repository.eq_ignore_ascii_case(&repo.path)
            && pull["source_branch"] == head.branch
            && pull["source_project_id"] == pull["target_project_id"]
    }

    fn branch_read(&self, repo: &ForgeRepo, branch: &str) -> (Request, &'static str) {
        let endpoint =
            repo.repo_endpoint(&format!("repository/branches/{}", encode(branch, false)));
        (Request::get(endpoint), "/commit/id")
    }

    fn new_pull(&self, repo: &ForgeRepo, pull: &NewPull) -> Request {
        let title = if pull.draft {
            DRAFT.draft_title(&pull.title, true)
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

    /// Discussion and inline comments are both an item's notes.
    fn edit_comment(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
        body: &str,
    ) -> Result<Request> {
        let (Subject::Comment(id) | Subject::ReviewComment(id)) = subject else {
            bail!("only a comment can be edited");
        };
        let endpoint = format!("{}/notes/{id}", repo.item_endpoint(kind, number));
        Ok(Request::new(
            HttpMethod::Put,
            endpoint,
            Some(json!({ "body": body })),
        ))
    }
}

/// Merge statuses during which GitLab has not yet decided mergeability.
const UNDECIDED: [&str; 4] = ["unchecked", "checking", "preparing", "approvals_syncing"];

/// Title prefixes that make a merge request a draft, including the former `WIP:`.
const DRAFT: DraftMarker = DraftMarker {
    prefixes: &["Draft:", "[Draft]", "(Draft)", "Draft -", "WIP:"],
    marker: "Draft:",
};

/// The IDs of the users `names`, which `current` records.
fn user_ids(current: &Current, names: &[String]) -> Result<Vec<u64>> {
    names
        .iter()
        .map(|name| {
            current
                .user_ids
                .get(name)
                .copied()
                .with_context(|| format!("GitLab user {name} was not looked up"))
        })
        .collect()
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
mod tests;
