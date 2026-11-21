use serde::{Deserialize, Serialize};
use shoal_ids::repository_ids::RepositoryId;

use super::{Forwarding, Lifetime, Target};

/// Requests about registered repositories, their configuration and their branches.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryMethod {
    ListRepositories,
    RegisterRepository {
        source: crate::repository::Selector,
        name: Option<String>,
        path: Option<std::path::PathBuf>,
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
    /// The repository's local config file, reserving its directory.
    LocalConfigPath {
        repository: crate::repository::Selector,
    },
    EditLocalConfig {
        repository: crate::repository::Selector,
        changes: Vec<crate::config::edit::Change>,
    },
    RemoveRepository {
        repository: crate::repository::Selector,
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
    /// Where the tracked agents of the repository with this ID kept their
    /// configuration, also once it is removed; in pages.
    AgentEnvironments {
        repository_id: RepositoryId,
    },
    /// Forget [`Self::AgentEnvironments`] once a removed repository's trust
    /// entries there are clean.
    ForgetAgentEnvironments {
        repository_id: RepositoryId,
    },
    /// [`WorkspaceMethod::KeepAgentEnvironment`] for a chat in the repository's
    /// checkout, which belongs to no workspace.
    KeepRepositoryAgentEnvironment {
        repository: RepositoryId,
        environment: std::collections::BTreeMap<String, String>,
    },
    /// Take the commit `commit` that another joined machine carries here
    /// for a start in the repository whose origin is `origin`, registered
    /// as a URL clone where there is none: once the daemon answers with
    /// [`Body::Ok`](super::Body::Ok), the caller writes a bundle of it with
    /// the history the origin's `branch` lacks, which the daemon fetches
    /// first, and ends its side, and the daemon answers again once the
    /// repository has the commit.
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
    /// Each is about the caller's machine's registrations, except commits
    /// another machine carries, which the hub sends to the machine that
    /// takes them.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ReceiveCommits { .. } => Forwarding::Duplex,
            Self::ListRepositories
            | Self::RegisterRepository { .. }
            | Self::RegisterContribution { .. }
            | Self::HoldContribution { .. }
            | Self::RenameRepository { .. }
            | Self::LocalConfigPath { .. }
            | Self::EditLocalConfig { .. }
            | Self::RemoveRepository { .. }
            | Self::PlanRepositoryMoves { .. }
            | Self::MoveRepositories { .. }
            | Self::SyncRepository { .. }
            | Self::FailingSyncs
            | Self::ListBranches { .. }
            | Self::KeptBranches { .. }
            | Self::DeleteKeptBranches { .. }
            | Self::AgentEnvironments { .. }
            | Self::ForgetAgentEnvironments { .. }
            | Self::KeepRepositoryAgentEnvironment { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ReceiveCommits { .. }
            | Self::ListRepositories
            | Self::RegisterRepository { .. }
            | Self::RegisterContribution { .. }
            | Self::HoldContribution { .. }
            | Self::RenameRepository { .. }
            | Self::LocalConfigPath { .. }
            | Self::EditLocalConfig { .. }
            | Self::RemoveRepository { .. }
            | Self::PlanRepositoryMoves { .. }
            | Self::MoveRepositories { .. }
            | Self::SyncRepository { .. }
            | Self::FailingSyncs
            | Self::ListBranches { .. }
            | Self::KeptBranches { .. }
            | Self::DeleteKeptBranches { .. }
            | Self::AgentEnvironments { .. }
            | Self::ForgetAgentEnvironments { .. }
            | Self::KeepRepositoryAgentEnvironment { .. } => None,
        }
    }
}
