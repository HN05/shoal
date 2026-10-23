//! GitHub's REST shapes, which Forgejo and Gitea serve too; [`Dialect`]
//! names the forge where they differ.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::{
    ForgeRepo,
    items::{
        action::{Edit, draft_title},
        item::{Current, Item, ItemState, PrSummary, draft_prefix, text},
        link::ItemKind,
    },
    transport::api::{HttpMethod, Request},
};

/// Which forge serves GitHub's REST shapes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    GitHub,
    /// Keeps draft state in the title, and a PR's ref as the head of a PR
    /// whose branch is deleted.
    Forgejo,
}

/// An item in GitHub's REST shape, read as `dialect` serves it.
pub(crate) fn current(
    repo: &ForgeRepo,
    kind: ItemKind,
    number: u64,
    value: &Value,
    dialect: Dialect,
) -> Result<Current> {
    if kind == ItemKind::Issue {
        ensure!(
            value["pull_request"].is_null(),
            "#{number} is a PR; use shoal pr"
        );
    }
    let url = repo.item_url(kind, number)?;
    let title = text(&value["title"]).context("item has no title")?;
    let merged =
        value["merged"].as_bool().unwrap_or_default() || value["merged_at"].as_str().is_some();
    let state = match (text(&value["state"]).as_deref(), merged) {
        (_, true) => ItemState::Merged,
        (Some("open"), _) => ItemState::Open,
        (Some("closed"), _) => ItemState::Closed,
        (state, _) => anyhow::bail!("unrecognized item state {state:?}"),
    };
    let labels = value["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| text(&label["name"]))
        .collect();
    let pr = match kind {
        ItemKind::Issue => None,
        ItemKind::Pr => Some(PrSummary {
            head: head_branch(number, value, dialect)?,
            base: text(&value["base"]["ref"]).context("PR has no base branch")?,
            draft: if dialect == Dialect::Forgejo {
                draft_prefix(&title).is_some()
            } else {
                value["draft"].as_bool().unwrap_or_default()
            },
        }),
    };
    let conflict = pr
        .as_ref()
        .zip(value["mergeable"].as_bool())
        .map(|(pr, mergeable)| !pr.draft && !mergeable);
    Ok(Current {
        item: Item {
            kind,
            number,
            url,
            title,
            state,
            labels,
            pr,
        },
        node_id: text(&value["node_id"]),
        head_repository: text(&value["head"]["repo"]["full_name"]),
        body: text(&value["body"]).unwrap_or_default(),
        head_sha: text(&value["head"]["sha"]),
        base_sha: text(&value["base"]["sha"]),
        conflict,
        reviewers: Vec::new(),
        user_ids: Default::default(),
    })
}

/// Once a PR's branch is deleted, Forgejo reports the PR's own ref as its
/// head and keeps the branch name only as the head's label.
fn head_branch(number: u64, value: &Value, dialect: Dialect) -> Result<String> {
    let head = text(&value["head"]["ref"]).context("PR has no head branch")?;
    if dialect == Dialect::Forgejo && head == format!("refs/pull/{number}/head") {
        return text(&value["head"]["label"]).context("PR has no head branch");
    }
    Ok(head)
}

/// The requests that change `current` as `edit` says, except GitHub's draft
/// state, which only its GraphQL API changes.
pub(crate) fn edit(
    repo: &ForgeRepo,
    current: &Current,
    edit: &Edit,
    dialect: Dialect,
) -> Vec<Request> {
    let item = &current.item;
    let mut requests = Vec::new();
    let mut fields = serde_json::Map::new();
    let mut title = edit.title.clone();
    let draft = draft_change(current, edit);
    // Forgejo keeps draft state in the title, so a new title carries it too.
    if let (Some(pr), Dialect::Forgejo) = (&item.pr, dialect)
        && (title.is_some() || draft.is_some())
    {
        let current = title.as_deref().unwrap_or(&item.title);
        title = Some(draft_title(current, draft.unwrap_or(pr.draft)));
    }
    if let Some(title) = title {
        fields.insert("title".into(), title.into());
    }
    if let Some(body) = &edit.body {
        fields.insert("body".into(), body.clone().into());
    }
    if let Some(base) = &edit.base {
        fields.insert("base".into(), base.clone().into());
    }
    if !fields.is_empty() {
        requests.push(Request::new(
            HttpMethod::Patch,
            repo.item_endpoint(item.kind, item.number),
            Some(fields.into()),
        ));
    }
    if !edit.add_labels.is_empty() || !edit.remove_labels.is_empty() {
        // Replacing the set takes label names on both forges.
        let mut labels: Vec<_> = item
            .labels
            .iter()
            .filter(|label| !edit.remove_labels.contains(label))
            .cloned()
            .collect();
        for label in &edit.add_labels {
            if !labels.contains(label) {
                labels.push(label.clone());
            }
        }
        requests.push(Request::new(
            HttpMethod::Put,
            repo.repo_endpoint(&format!("issues/{}/labels", item.number)),
            Some(json!({ "labels": labels })),
        ));
    }
    let reviewers = repo.repo_endpoint(&format!("pulls/{}/requested_reviewers", item.number));
    for (method, reviewers_list) in [
        (HttpMethod::Post, &edit.add_reviewers),
        (HttpMethod::Delete, &edit.remove_reviewers),
    ] {
        if !reviewers_list.is_empty() {
            requests.push(Request::new(
                method,
                reviewers.clone(),
                Some(json!({ "reviewers": reviewers_list })),
            ));
        }
    }
    requests
}

/// The draft state `edit` changes the PR to, if it changes it.
pub(crate) fn draft_change(current: &Current, edit: &Edit) -> Option<bool> {
    edit.draft.filter(|draft| {
        current
            .item
            .pr
            .as_ref()
            .is_some_and(|pr| pr.draft != *draft)
    })
}

/// Post `body` as a comment on the item.
pub(crate) fn comment(repo: &ForgeRepo, item: &Item, body: &str) -> Request {
    let endpoint = repo.repo_endpoint(&format!("issues/{}/comments", item.number));
    Request::new(HttpMethod::Post, endpoint, Some(json!({ "body": body })))
}

/// Close or reopen the item.
pub(crate) fn set_state(repo: &ForgeRepo, item: &Item, state: ItemState) -> Request {
    let state = if state == ItemState::Open {
        "open"
    } else {
        "closed"
    };
    Request::new(
        HttpMethod::Patch,
        repo.item_endpoint(item.kind, item.number),
        Some(json!({ "state": state })),
    )
}
