//! GitHub's REST shapes, which Forgejo and Gitea serve too; [`Dialect`]
//! names the forge where they differ.
use anyhow::{Context, Result, ensure};
use serde_json::Value;

use crate::{
    ForgeRepo,
    items::{
        item::{Current, Item, ItemState, PrSummary, draft_prefix, text},
        link::ItemKind,
    },
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
