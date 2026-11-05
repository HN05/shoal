//! Registered repositories: registering, renaming, moving and removing
//! them, their checkouts' sync, branches and local configuration, and the
//! agent configs Shoal keeps for them.
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use crate::{
    client::{
        connection::{Daemon, call, pages, request},
        repository::Selector,
    },
    config::edit::Change,
    model::{
        Branch, BranchTarget, CheckoutSync, KeptBranch, LocalConfigEdit, MovedRepository,
        Repository, RepositoryMoves, RepositoryRemoval, SyncedRepository,
    },
    protocol::RepositoryMethod,
};

/// Register `source`, a path or URL, as `name`, cloning a URL to `path`.
pub async fn register(
    daemon: &impl Daemon,
    source: Selector,
    name: Option<String>,
    path: Option<PathBuf>,
) -> Result<Repository> {
    request(
        daemon,
        RepositoryMethod::RegisterRepository { source, name, path },
    )
    .await
}

/// Register a fork of `source` to contribute to, or the repository already
/// registered for it.
pub async fn register_contribution(daemon: &impl Daemon, source: String) -> Result<Repository> {
    let method = RepositoryMethod::RegisterContribution { source, name: None };
    request(daemon, method).await
}

pub async fn rename(
    daemon: &impl Daemon,
    repository: Selector,
    name: String,
) -> Result<Repository> {
    request(
        daemon,
        RepositoryMethod::RenameRepository { repository, name },
    )
    .await
}

pub async fn remove(daemon: &impl Daemon, repository: Selector) -> Result<RepositoryRemoval> {
    request(daemon, RepositoryMethod::RemoveRepository { repository }).await
}

/// What moving every repository into `root`, or into the configured root,
/// would move; `regular_dir` lets a root that is not a mount point pass.
pub async fn plan_moves(
    daemon: &impl Daemon,
    root: Option<PathBuf>,
    regular_dir: bool,
) -> Result<RepositoryMoves> {
    request(
        daemon,
        RepositoryMethod::PlanRepositoryMoves { root, regular_dir },
    )
    .await
}

/// Move every repository into the configured root.
pub async fn move_all(daemon: &impl Daemon, regular_dir: bool) -> Result<Vec<MovedRepository>> {
    request(daemon, RepositoryMethod::MoveRepositories { regular_dir }).await
}

/// Fetch `repository`'s remote and fast-forward its default branch.
pub async fn sync(daemon: &impl Daemon, repository: Selector) -> Result<SyncedRepository> {
    request(daemon, RepositoryMethod::SyncRepository { repository }).await
}

/// The checkouts whose background sync fails.
pub async fn failing_syncs(daemon: &impl Daemon) -> Result<Vec<CheckoutSync>> {
    pages(daemon, RepositoryMethod::FailingSyncs).await
}

/// `repository`'s local and remote branches.
pub async fn branches(daemon: &impl Daemon, repository: Selector) -> Result<Vec<Branch>> {
    request(daemon, RepositoryMethod::ListBranches { repository }).await
}

/// The branches Shoal kept when it removed their workspaces, only those of
/// `repository` when given.
pub async fn kept_branches(
    daemon: &impl Daemon,
    repository: Option<String>,
) -> Result<Vec<KeptBranch>> {
    request(daemon, RepositoryMethod::KeptBranches { repository }).await
}

/// Delete the kept `branches`, each only while it is at the commit given.
pub async fn delete_kept_branches(
    daemon: &impl Daemon,
    branches: Vec<BranchTarget>,
) -> Result<Vec<KeptBranch>> {
    request(daemon, RepositoryMethod::DeleteKeptBranches { branches }).await
}

/// Where `repository`'s local configuration file is, or would be.
pub async fn local_config_path(daemon: &impl Daemon, repository: Selector) -> Result<PathBuf> {
    request(daemon, RepositoryMethod::LocalConfigPath { repository }).await
}

/// Apply `changes` to `repository`'s local configuration file.
pub async fn edit_local_config(
    daemon: &impl Daemon,
    repository: Selector,
    changes: Vec<Change>,
) -> Result<LocalConfigEdit> {
    request(
        daemon,
        RepositoryMethod::EditLocalConfig {
            repository,
            changes,
        },
    )
    .await
}

/// The agent environments Shoal keeps for the repository `repository_id`.
pub async fn agent_environments(
    daemon: &impl Daemon,
    repository_id: String,
) -> Result<Vec<BTreeMap<String, String>>> {
    pages(
        daemon,
        RepositoryMethod::AgentEnvironments { repository_id },
    )
    .await
}

/// Forget the agent environments kept for the removed repository
/// `repository_id`.
pub async fn forget_agent_environments(daemon: &impl Daemon, repository_id: String) -> Result<()> {
    call(
        daemon,
        RepositoryMethod::ForgetAgentEnvironments { repository_id },
    )
    .await?;
    Ok(())
}
