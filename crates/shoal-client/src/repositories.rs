//! Requests about registered repositories and what Shoal keeps for them.
use shoal_ids::repository_ids::RepositoryId;
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use anyhow::Result;
use tokio::{io::AsyncReadExt as _, net::UnixStream};

use crate::connection::{Daemon, call, open, pages, request};
use shoal_protocol::{
    config::edit::Change,
    hub::directory::HubView,
    model::{
        Branch, BranchTarget, CheckoutSync, KeptBranch, LocalConfigEdit, MovedRepository,
        Repository, RepositoryMoves, RepositoryRemoval, RepositoryRemovalPreview, SyncedRepository,
    },
    protocol::RepositoryMethod,
    repository::Selector,
};

/// Hold a contribution again this soon after the daemon closes its hold.
const HOLD_RETRY: Duration = Duration::from_secs(1);

pub async fn list(daemon: &impl Daemon) -> Result<Vec<Repository>> {
    request(daemon, RepositoryMethod::ListRepositories).await
}

/// The repositories the other joined machines register, with the hub's
/// machines; none on a machine in no hub.
pub async fn elsewhere(daemon: &impl Daemon) -> Result<Option<HubView>> {
    request(daemon, RepositoryMethod::RepositoriesElsewhere).await
}

/// Register `source`, a path or URL, as `name`, cloning a URL to `path`,
/// and share it with the other machines of the hub when `shared`.
pub async fn register(
    daemon: &impl Daemon,
    source: Selector,
    name: Option<String>,
    path: Option<PathBuf>,
    shared: bool,
) -> Result<Repository> {
    let method = RepositoryMethod::RegisterRepository {
        source,
        name,
        path,
        shared,
    };
    request(daemon, method).await
}

/// Register a fork of `source` to contribute to, or the repository already
/// registered for it, which cleanup keeps while the hold lives.
pub async fn register_contribution(
    daemon: &(impl Daemon + Clone + Send + 'static),
    source: String,
) -> Result<(Repository, ContributionHold)> {
    let method = RepositoryMethod::RegisterContribution { source, name: None };
    let (stream, body) = open(daemon, method).await?;
    let repo = Repository::try_from(body.into_result()?)?;
    let hold = tokio::spawn(keep_contribution(daemon.clone(), repo.id.clone(), stream));
    Ok((repo, ContributionHold(hold)))
}

/// Keeps a registered contribution from cleanup while it lives, from its
/// registration on. It holds it again after a daemon restart.
pub struct ContributionHold(tokio::task::JoinHandle<()>);

impl Drop for ContributionHold {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Hold `repository` on the registration's `stream`, and once the daemon
/// closes it, on a new connection, until the repository is gone.
async fn keep_contribution(daemon: impl Daemon, repository: RepositoryId, mut stream: UnixStream) {
    let mut closed = [0u8; 1];
    let _ = stream.read(&mut closed).await;
    loop {
        tokio::time::sleep(HOLD_RETRY).await;
        let repository = repository.clone();
        if let Ok((mut stream, body)) =
            open(&daemon, RepositoryMethod::HoldContribution { repository }).await
        {
            // A refusal means the repository is no longer registered.
            if body.into_result().is_err() {
                return;
            }
            let _ = stream.read(&mut closed).await;
        }
    }
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

pub async fn share(daemon: &impl Daemon, repository: Selector, shared: bool) -> Result<Repository> {
    request(
        daemon,
        RepositoryMethod::ShareRepository { repository, shared },
    )
    .await
}

/// Remove `repository` on the joined machine `machine` names, or on this
/// one.
pub async fn remove(
    daemon: &impl Daemon,
    repository: Selector,
    machine: Option<String>,
) -> Result<RepositoryRemoval> {
    let method = RepositoryMethod::RemoveRepository {
        repository,
        machine,
    };
    request(daemon, method).await
}

/// What removing `repository` would remove on the joined machine `machine`
/// names, or on this one.
pub async fn preview_removal(
    daemon: &impl Daemon,
    repository: Selector,
    machine: Option<String>,
) -> Result<RepositoryRemovalPreview> {
    let method = RepositoryMethod::PreviewRepositoryRemoval {
        repository,
        machine,
    };
    request(daemon, method).await
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

/// Keep where a chat agent of `repository` keeps its configuration for the
/// repository's removal or move, as
/// [`crate::workspaces::keep_agent_environment`] does for a workspace's
/// agent.
pub async fn keep_agent_environment(
    daemon: &impl Daemon,
    repository: &RepositoryId,
    environment: BTreeMap<String, String>,
) -> Result<()> {
    call(
        daemon,
        RepositoryMethod::KeepRepositoryAgentEnvironment {
            repository: repository.clone(),
            environment,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
