use serde::{Deserialize, Serialize};

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
    /// Clone a repository URL for `shoal contribute`.
    RegisterContribution {
        source: String,
        name: Option<String>,
    },
    RenameRepository {
        repository: crate::repository::Selector,
        name: String,
    },
    RepositoryConfig {
        repository: crate::repository::Selector,
    },
    SetRepositoryConfig {
        repository: crate::repository::Selector,
        toml: Option<String>,
    },
    EditRepositoryConfig {
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
    },
    /// Move every repository directory into the configured `root_dir`.
    MoveRepositories,
    /// Fetch the default branch's remote and fast-forward the local default branch.
    SyncRepository {
        repository: crate::repository::Selector,
    },
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
        repository_id: String,
    },
    /// Forget [`Self::AgentEnvironments`] once a removed repository's trust
    /// entries there are clean.
    ForgetAgentEnvironments {
        repository_id: String,
    },
    /// [`WorkspaceMethod::KeepAgentEnvironment`] for a chat in the repository's
    /// checkout, which belongs to no workspace.
    KeepRepositoryAgentEnvironment {
        repository: String,
        environment: std::collections::BTreeMap<String, String>,
    },
}
