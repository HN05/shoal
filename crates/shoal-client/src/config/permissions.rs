//! `[permissions]`: what a scoped agent may do at once, after approval, or
//! not at all.
use serde::{Deserialize, Serialize};

use crate::{config::options::Options, state::states};

states!(
    #[derive(Default)]
    /// Whether a scoped caller's action happens at once or waits for the
    /// user's approval.
    Opening {
        #[default]
        Direct => "direct",
        Propose => "propose",
    }
);

states!(
    #[derive(Default)]
    /// Whether a scoped caller's push happens at once, waits for the user's
    /// approval, or is refused.
    Pushes {
        #[default]
        Direct => "direct",
        Propose => "propose",
        Deny => "deny",
    }
);

states!(
    #[derive(Default)]
    /// Whether a scoped caller may open an issue in another repository than
    /// its workspace's, at once or after the user's approval.
    OtherRepositories {
        Deny => "deny",
        #[default]
        Propose => "propose",
        Direct => "direct",
    }
);

states!(
    #[derive(Default)]
    /// Whether a scoped caller may send forge API requests that are not reads.
    ApiWrites {
        #[default]
        Deny => "deny",
        Direct => "direct",
    }
);

/// What a scoped agent may do at once, after the user's approval, or not at all.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Permissions)]
pub struct PermissionsConfig {
    #[option(default = Opening::Direct)]
    pub issue_open: Option<Opening>,
    #[option(default = Opening::Direct)]
    pub pr_open: Option<Opening>,
    /// Opening an issue in another repository than the workspace's.
    #[option(default = OtherRepositories::Propose)]
    pub issue_other_repositories: Option<OtherRepositories>,
    /// Merging a branch with `shoal merge`.
    #[option(default = Opening::Propose)]
    pub merge: Option<Opening>,
    /// Pushing to the workspace repository's remotes, with `shoal pr open`
    /// or plain `git push`.
    #[option(default = Pushes::Direct)]
    pub push: Option<Pushes>,
    /// Forge API requests through `shoal forge api` that are not reads.
    #[option(default = ApiWrites::Deny)]
    pub api_writes: Option<ApiWrites>,
}
