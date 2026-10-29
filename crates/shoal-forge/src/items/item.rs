//! An issue or PR as one REST read reports it, in the same shape for every
//! forge: what an action returns and what planning an action reads.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backends::Backend;
use crate::state::states;
use crate::{ForgeRepo, items::link::ItemKind};

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
    /// Why the item was created without the workspace recording it, and
    /// what it leaves out of the repository's conventions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// An item with the forge-specific facts some actions need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub item: Item,
    /// GitHub's GraphQL ID, which its draft mutations take.
    pub node_id: Option<String>,
    /// `owner/repository` of a PR's head branch.
    pub head_repository: Option<String>,
    pub body: String,
    /// Who opened the item, named as reviewer changes name users.
    pub author: Option<String>,
    /// The commit a PR's head branch points to.
    pub head_sha: Option<String>,
    /// The commit a PR's base branch points to, where the forge reports it.
    pub base_sha: Option<String>,
    /// Whether an open PR conflicts with its base; unknown while the forge
    /// computes it. Forgejo reports none for drafts.
    pub conflict: Option<bool>,
    /// Whether GitHub would merge the open PR now, which it then refuses
    /// to merge later.
    pub merges_now: bool,
    /// The usernames of a GitLab merge request's reviewers.
    pub reviewers: Vec<String>,
    /// GitLab user IDs by username, which its reviewer changes take: the
    /// reviewers', and those of the users an edit adds.
    pub user_ids: std::collections::BTreeMap<String, u64>,
    /// The item's labels that a Forgejo edit's added exclusive labels
    /// replace, as the only one an item may have of their scope.
    pub replaced_labels: Vec<String>,
}

impl ForgeRepo {
    /// The item's REST endpoint.
    pub(crate) fn item_endpoint(&self, kind: ItemKind, number: u64) -> String {
        let resource = match kind {
            ItemKind::Issue => "issues",
            ItemKind::Pr => self.kind.pull_resource(),
        };
        self.repo_endpoint(&format!("{resource}/{number}"))
    }

    /// Read a REST item; an issue read that returns a PR is refused.
    pub(crate) fn current(&self, kind: ItemKind, number: u64, value: &Value) -> Result<Current> {
        ensure!(
            value[self.kind.number_field()] == number,
            "the forge returned a different item than #{number}"
        );
        self.kind.current(self, kind, number, value)
    }

    /// The URL Shoal records for the item.
    pub fn item_url(&self, kind: ItemKind, number: u64) -> Result<String> {
        let number = number.to_string();
        Ok(match kind {
            ItemKind::Issue => self.issue(&number)?.1,
            ItemKind::Pr => self.pull(&number)?.1,
        })
    }
}

/// The kind of the item a GitHub or Forgejo issue read returned, which
/// reports a PR as an issue with a `pull_request` field.
pub(crate) fn issue_read_kind(number: u64, value: &Value) -> Result<ItemKind> {
    ensure!(
        value["number"] == number,
        "the forge returned a different item than #{number}"
    );
    Ok(if value["pull_request"].is_null() {
        ItemKind::Issue
    } else {
        ItemKind::Pr
    })
}

pub(crate) fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests;
