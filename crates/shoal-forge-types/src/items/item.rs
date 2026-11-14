//! An issue or PR as one read reports it, in the same shape for every
//! forge.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use crate::{ForgeRepo, items::link::ItemKind};
use shoal_states::states;

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
    pub url: crate::items::reference::ItemRef,
    pub title: String,
    pub state: ItemState,
    pub labels: Vec<String>,
    /// Named as edits take them: logins, or Bitbucket account IDs.
    #[serde(default)]
    pub assignees: Vec<String>,
    /// The milestone's title.
    #[serde(default)]
    pub milestone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub head: BranchName,
    pub base: BranchName,
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
    pub head_sha: Option<CommitId>,
    /// The commit a PR's base branch points to, where the forge reports it.
    pub base_sha: Option<CommitId>,
    /// The commit a merged PR's merge left at the tip of its base branch: a
    /// merge commit, its squashed commit, or the last of its rebased ones.
    pub merge_commit: Option<CommitId>,
    /// Whether an open PR conflicts with its base; unknown while the forge
    /// computes it. Forgejo reports none for drafts.
    pub conflict: Option<bool>,
    /// Whether GitHub would merge the open PR now, which it then refuses
    /// to merge later.
    pub merges_now: bool,
    /// A GitLab merge request's reviewers by username, or a Bitbucket pull
    /// request's by account ID.
    pub reviewers: Vec<String>,
    /// GitLab user IDs by username, which its reviewer and assignee changes
    /// take: the reviewers' and assignees', and those of the users an edit
    /// adds.
    pub user_ids: std::collections::BTreeMap<String, u64>,
    /// The ID the forge takes for the milestone an edit sets, looked up
    /// before the edit is planned.
    pub milestone_id: Option<u64>,
    /// The item's labels that a Forgejo edit's added exclusive labels
    /// replace, as the only one an item may have of their scope.
    pub replaced_labels: Vec<String>,
}

impl Current {
    /// Whether the PR's head branch is in `repo` rather than in a fork.
    pub fn head_in(&self, repo: &ForgeRepo) -> bool {
        self.head_repository
            .as_deref()
            .is_some_and(|path| path.eq_ignore_ascii_case(&repo.path))
    }
}

impl ForgeRepo {
    /// The item Shoal records for the `kind` item numbered `number`.
    pub fn item_url(
        &self,
        kind: ItemKind,
        number: u64,
    ) -> Result<crate::items::reference::ItemRef> {
        crate::items::reference::ItemRef::numbered(self, kind.into(), number)
    }
}

/// Why a PR in `state` at `pull_head` on the forge cannot be marked ready
/// for review at the local `head`, if one is given: it is not open, or the
/// forge reports another head commit.
pub fn unmarkable_reason(
    state: ItemState,
    pull_head: Option<&str>,
    head: Option<&str>,
) -> Option<String> {
    if state != ItemState::Open {
        return Some(format!("the PR is {state}"));
    }
    match (pull_head, head) {
        (None, _) => Some("the forge reports no head commit".into()),
        (Some(pull), Some(head)) if pull != head => Some(format!(
            "its head on the forge is {}, not HEAD {}",
            short(pull),
            short(head)
        )),
        _ => None,
    }
}

/// A commit's abbreviated name.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[cfg(test)]
mod tests;
