//! The branches on the forge of a space's repository, or another one:
//! listing them with what decides whether each can go.
use anyhow::Result;

use crate::connection::{Daemon, pages};
use shoal_protocol::{
    branches::ListedBranch,
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
