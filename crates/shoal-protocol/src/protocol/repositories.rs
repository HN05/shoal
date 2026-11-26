use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;

use super::{Forwarding, Lifetime, Target};

/// Requests about registered repositories.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryMethod {
    ListRepositories,
    /// The repositories the other joined machines register, with the
    /// hub's machines, as the hub lists them.
    RepositoriesElsewhere,
    RegisterRepository {
        source: crate::repository::Selector,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
        /// Share it with the other machines of the hub.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        shared: bool,
    },
    /// Clone a repository URL for `shoal contribute`, then keep the
    /// contribution from cleanup while the connection stays open.
    RegisterContribution {
        source: String,
        name: Option<String>,
    },
    /// Keep a contribution from cleanup while the connection stays open, as
    /// its registration did before the daemon restarted.
    HoldContribution {
        repository: RepositoryId,
    },
    RenameRepository {
        repository: crate::repository::Selector,
        name: String,
    },
    /// Share the repository with the other machines of the hub, or stop
    /// sharing it.
    ShareRepository {
        repository: crate::repository::Selector,
        shared: bool,
    },
    /// The repository's local config file, reserving its directory.
    LocalConfigPath {
        repository: crate::repository::Selector,
    },
    EditLocalConfig {
        repository: crate::repository::Selector,
        changes: Vec<crate::config::edit::Change>,
    },
    /// Remove the repository on the joined machine `machine` names by its
    /// name or ID, or on this one.
    RemoveRepository {
        repository: crate::repository::Selector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// What removing the repository would remove, on the joined machine
    /// `machine` names, or on this one.
    PreviewRepositoryRemoval {
        repository: crate::repository::Selector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// The repositories `MoveRepositories` would move into `root`, or the
    /// configured `root_dir`, after the checks it runs before moving any.
    PlanRepositoryMoves {
        root: Option<std::path::PathBuf>,
        /// Keep the root a regular directory instead of one Spotlight skips.
        #[serde(default)]
        regular_dir: bool,
    },
    /// Move every repository directory into the configured `root_dir`.
    MoveRepositories {
        #[serde(default)]
        regular_dir: bool,
    },
    /// Fetch the default branch's remote and fast-forward the local default branch.
    SyncRepository {
        repository: crate::repository::Selector,
    },
    /// The repositories whose background sync has failed long enough for the inbox.
    FailingSyncs,
    ListBranches {
        repository: crate::repository::Selector,
    },
    /// Branches removal kept because only they have some commits, only
    /// those of one repository when a selector names it.
    KeptBranches {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<String>,
    },
    /// Delete kept branches with the commits only they have; all or none.
    DeleteKeptBranches {
        branches: Vec<crate::model::BranchTarget>,
    },
    /// [`WorkspaceMethod::KeepAgentEnvironment`](super::WorkspaceMethod::KeepAgentEnvironment) for a chat in the repository's
    /// checkout, which belongs to no workspace.
    KeepRepositoryAgentEnvironment {
        repository: RepositoryId,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// Take `commit`, which another machine carries here for a start in the
    /// repository whose origin is `origin`. That repository is registered as
    /// a URL clone where there is none. Once the daemon answers with
    /// [`Body::Ok`](super::Body::Ok), the caller writes a bundle of the
    /// history the origin's `branch` lacks and ends its side. The daemon
    /// answers again once the repository has the commit.
    ReceiveCommits {
        origin: String,
        commit: shoal_refs::CommitId,
        branch: String,
    },
}

impl RepositoryMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            // Never sent again: once answered, its hold resumes as
            // `HoldContribution`.
            Self::RegisterContribution { .. } => Lifetime::Open,
            Self::HoldContribution { .. } => Lifetime::Resumed,
            // It takes the bundle the caller writes.
            Self::ReceiveCommits { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

impl RepositoryMethod {
    /// Each is about the caller's machine's registrations. The daemon sends
    /// a removal and its preview to the machine they name.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ReceiveCommits { .. } => Forwarding::Duplex,
            Self::RemoveRepository { .. } | Self::PreviewRepositoryRemoval { .. } => {
                Forwarding::Online
            }
            Self::ListRepositories
            | Self::RepositoriesElsewhere
            | Self::RegisterRepository { .. }
            | Self::RegisterContribution { .. }
            | Self::HoldContribution { .. }
            | Self::RenameRepository { .. }
            | Self::ShareRepository { .. }
            | Self::LocalConfigPath { .. }
            | Self::EditLocalConfig { .. }
            | Self::PlanRepositoryMoves { .. }
            | Self::MoveRepositories { .. }
            | Self::SyncRepository { .. }
            | Self::FailingSyncs
            | Self::ListBranches { .. }
            | Self::KeptBranches { .. }
            | Self::DeleteKeptBranches { .. }
            | Self::KeepRepositoryAgentEnvironment { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ReceiveCommits { .. }
            | Self::ListRepositories
            | Self::RepositoriesElsewhere
            | Self::RegisterRepository { .. }
            | Self::RegisterContribution { .. }
            | Self::HoldContribution { .. }
            | Self::RenameRepository { .. }
            | Self::ShareRepository { .. }
            | Self::LocalConfigPath { .. }
            | Self::EditLocalConfig { .. }
            | Self::RemoveRepository { .. }
            | Self::PreviewRepositoryRemoval { .. }
            | Self::PlanRepositoryMoves { .. }
            | Self::MoveRepositories { .. }
            | Self::SyncRepository { .. }
            | Self::FailingSyncs
            | Self::ListBranches { .. }
            | Self::KeptBranches { .. }
            | Self::DeleteKeptBranches { .. }
            | Self::KeepRepositoryAgentEnvironment { .. } => None,
        }
    }
}
