//! The branches on the forge of a space's repository, or another one:
//! listing them with what decides whether each can go, and deleting them.
use anyhow::Result;

use crate::connection::{Daemon, pages, request};
use shoal_protocol::{
    branches::{DeletedBranch, ListedBranch},
    protocol::{BranchMethod, RepoTarget},
};

/// The branches on the forge of the `target` repository, only those whose
/// merged state is known to be `merged` when it is given.
pub async fn list(
    daemon: &impl Daemon,
    target: RepoTarget,
    merged: Option<bool>,
) -> Result<Vec<ListedBranch>> {
    pages(daemon, BranchMethod::ListBranches { target, merged }).await
}

/// Delete `branches` on the forge of the `target` repository, those whose
/// changes the default branch lacks only with `force`, or none when one is
/// refused.
pub async fn delete(
    daemon: &impl Daemon,
    target: RepoTarget,
    branches: Vec<String>,
    force: bool,
) -> Result<Vec<DeletedBranch>> {
    let method = BranchMethod::DeleteBranches {
        target,
        branches: branches.into_iter().map(Into::into).collect(),
        force,
    };
    request(daemon, method).await
}
