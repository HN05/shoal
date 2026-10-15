//! An issue or PR as one REST read reports it, in the same shape for every
//! forge: what an action returns and what planning an action reads.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ForgeKind, ForgeRepo, link::ItemKind};
use crate::state::states;

/// Forgejo treats a PR whose title starts with one of these as a draft.
pub(crate) const DRAFT_PREFIXES: [&str; 2] = ["WIP:", "[WIP]"];

states!(ItemState {
    Open => "open",
    Closed => "closed",
    /// A PR merged into its base.
    Merged => "merged",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: ItemState,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub head: String,
    pub base: String,
    pub draft: bool,
}

/// An item Shoal opened, or the open PR it found for a branch instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opened {
    #[serde(flatten)]
    pub item: Item,
    pub created: bool,
    pub linked: bool,
}

/// An item with the forge-specific facts some actions need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Current {
    pub item: Item,
    /// GitHub's GraphQL ID, which its draft mutations take.
    pub node_id: Option<String>,
    /// `owner/repository` of a PR's head branch.
    pub head_repository: Option<String>,
    pub body: String,
    /// The commit a PR's head branch points to.
    pub head_sha: Option<String>,
    /// Whether an open PR conflicts with its base; unknown while the forge
    /// computes it. Forgejo reports none for drafts.
    pub conflict: Option<bool>,
    /// The usernames of a GitLab merge request's reviewers.
    pub reviewers: Vec<String>,
    /// GitLab user IDs by username, which its reviewer changes take: the
    /// reviewers', and those of the users an edit adds.
    pub user_ids: std::collections::BTreeMap<String, u64>,
}

impl ForgeRepo {
    /// The item's REST endpoint.
    pub(crate) fn item_endpoint(&self, kind: ItemKind, number: u64) -> String {
        let resource = match (kind, self.kind) {
            (ItemKind::Issue, _) => "issues",
            (ItemKind::Pr, ForgeKind::GitHub | ForgeKind::Forgejo) => "pulls",
            (ItemKind::Pr, ForgeKind::GitLab) => "merge_requests",
        };
        self.repo_endpoint(&format!("{resource}/{number}"))
    }

    /// Read a REST item; an issue read that returns a PR is refused.
    pub(crate) fn current(&self, kind: ItemKind, number: u64, value: &Value) -> Result<Current> {
        ensure!(
            value[self.kind.number_field()] == number,
            "the forge returned a different item than #{number}"
        );
        match self.kind {
            ForgeKind::GitHub | ForgeKind::Forgejo => self.github_current(kind, number, value),
            ForgeKind::GitLab => self.gitlab_current(kind, number, value),
        }
    }

    /// The URL Shoal records for the item.
    pub(crate) fn item_url(&self, kind: ItemKind, number: u64) -> Result<String> {
        let number = number.to_string();
        Ok(match kind {
            ItemKind::Issue => self.issue(&number)?.1,
            ItemKind::Pr => self.pull(&number)?.1,
        })
    }

    /// GitHub's REST shape, which Forgejo shares.
    fn github_current(&self, kind: ItemKind, number: u64, value: &Value) -> Result<Current> {
        if kind == ItemKind::Issue {
            ensure!(
                value["pull_request"].is_null(),
                "#{number} is a PR; use shoal pr"
            );
        }
        let url = self.item_url(kind, number)?;
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
                head: text(&value["head"]["ref"]).context("PR has no head branch")?,
                base: text(&value["base"]["ref"]).context("PR has no base branch")?,
                draft: if self.kind == ForgeKind::Forgejo {
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
            conflict,
            reviewers: Vec::new(),
            user_ids: Default::default(),
        })
    }
}

pub(super) fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pulls_read_the_same_from_both_forges() {
        let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        let pull = json!({"number": 7, "title": "Fix", "state": "closed",
            "merged_at": "2026-01-01T00:00:00Z", "draft": false, "node_id": "PR_1",
            "labels": [{"name": "bug"}],
            "head": {"ref": "topic", "repo": {"full_name": "team/repo"}}, "base": {"ref": "main"}});
        let current = github.current(ItemKind::Pr, 7, &pull).unwrap();
        assert_eq!(
            current.item,
            Item {
                kind: ItemKind::Pr,
                number: 7,
                url: "https://github.com/team/repo/pull/7".into(),
                title: "Fix".into(),
                state: ItemState::Merged,
                labels: vec!["bug".into()],
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: false,
                }),
            }
        );
        assert_eq!(current.node_id.as_deref(), Some("PR_1"));
        assert_eq!(current.head_repository.as_deref(), Some("team/repo"));

        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let pull = json!({"number": 7, "title": "wip: Fix", "state": "open", "merged": false,
            "labels": [], "head": {"ref": "topic", "repo": null}, "base": {"ref": "main"}});
        let current = forgejo.current(ItemKind::Pr, 7, &pull).unwrap();
        assert_eq!(current.item.url, "https://forge.example/team/repo/pulls/7");
        assert_eq!(current.item.state, ItemState::Open);
        assert!(current.item.pr.unwrap().draft);
        assert_eq!(current.head_repository, None);
    }

    #[test]
    fn issue_reads_refuse_prs_and_other_numbers() {
        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let issue = json!({"number": 3, "title": "Bug", "state": "open", "labels": [],
            "pull_request": null});
        let current = forgejo.current(ItemKind::Issue, 3, &issue).unwrap();
        assert_eq!(current.item.url, "https://forge.example/team/repo/issues/3");
        assert_eq!(current.item.pr, None);
        let pull = json!({"number": 3, "title": "Fix", "state": "open",
            "pull_request": {"merged": false}});
        assert!(forgejo.current(ItemKind::Issue, 3, &pull).is_err());
        assert!(forgejo.current(ItemKind::Issue, 4, &issue).is_err());
    }

    #[test]
    fn draft_prefixes_ignore_case_and_need_the_whole_marker() {
        assert_eq!(draft_prefix("WIP: x"), Some(4));
        assert_eq!(draft_prefix("[wip] x"), Some(5));
        assert_eq!(draft_prefix("Wipe x"), None);
        assert_eq!(draft_prefix("Ü"), None);
    }
}
