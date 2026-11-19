use serde::{Deserialize, Serialize};
use shoal_refs::BranchName;

/// Requests about the branches on the forge of the target repository: that
/// of a space or registered checkout, or another one `--repo` names.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchMethod {
    /// The branches, with what decides whether each can go, answered in one
    /// or more `forge_branches` frames and then `ok`; with `merged`, only
    /// those the default branch is known to have the changes of, or to
    /// lack them.
    ListForgeBranches {
        #[serde(flatten)]
        target: super::RepoTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merged: Option<bool>,
    },
    /// Delete `branches` on the forge, each at the commit it points to, or
    /// none when one is refused, answered in one or more `deleted_branches`
    /// frames and then `ok`; `force` deletes those whose changes the
    /// default branch lacks.
    DeleteForgeBranches {
        #[serde(flatten)]
        target: super::RepoTarget,
        branches: Vec<BranchName>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        force: bool,
    },
}
